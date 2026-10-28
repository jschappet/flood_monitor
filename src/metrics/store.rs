use super::{atomic_write, now_unix, validate_payload, Config, MAX_PENDING};
use nix::fcntl::{flock, FlockArg};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReport {
    pub payload: Value,
    pub elapsed_ms: u64,
    pub frozen: bool,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub reports: VecDeque<SavedReport>,
    pub dropped: u64,
    pub rejected: u64,
    pub provisional_discarded: u64,
    pub latest_rejected: Option<Value>,
    pub last_http_status: Option<u16>,
    pub enrollment_status: Option<String>,
    pub retry_not_before_unix: u64,
    pub retry_delay_seconds: u64,
}

pub struct Store {
    pub dir: PathBuf,
    pub state: State,
    pub healthy: bool,
    pub missed_windows: u64,
    pub retry_at: Instant,
    _lock: File,
}

fn digest_hex(bytes: &[u8]) -> String {
    crate::protocol::hex(digest::digest(&digest::SHA256, bytes).as_ref())
}
fn load_state(path: &Path) -> io::Result<State> {
    // A checksum detects damaged snapshots; this is not protection from a local attacker.
    let bytes = fs::read(path)?;
    if bytes.len() > 40 * 1024 * 1024 {
        return Err(io::Error::other("metrics snapshot too large"));
    }
    let envelope: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let data = &envelope["state"];
    let encoded = serde_json::to_vec(data).map_err(io::Error::other)?;
    if envelope["sha256"].as_str() != Some(&digest_hex(&encoded)) {
        return Err(io::Error::other(
            "metrics snapshot checksum mismatch; preserving files",
        ));
    }
    let state: State = serde_json::from_value(data.clone()).map_err(io::Error::other)?;
    if state.reports.len() > MAX_PENDING {
        return Err(io::Error::other("invalid metrics queue size"));
    }
    Ok(state)
}

impl Store {
    pub fn open(config: &Config) -> io::Result<Self> {
        fs::create_dir_all(&config.spool_dir)?;
        fs::set_permissions(&config.spool_dir, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(config.spool_dir.join("queue.lock"))?;
        flock(lock.as_raw_fd(), FlockArg::LockExclusiveNonblock)
            .map_err(|_| io::Error::other("metrics outbox already in use"))?;
        let destination_path = config.spool_dir.join("destination.json");
        let destination = json!({"url":config.url,"device_id":config.device_id});
        if destination_path.exists() {
            let existing: Value =
                serde_json::from_slice(&fs::read(&destination_path)?).map_err(io::Error::other)?;
            if existing != destination {
                return Err(io::Error::other(
                    "outbox belongs to another URL/device; use a separate spool_dir",
                ));
            }
        } else {
            if config.spool_dir.join("queue.json").exists()
                || config.spool_dir.join("device.token").exists()
            {
                return Err(io::Error::other(
                    "outbox destination metadata missing; preserving files",
                ));
            }
            atomic_write(&destination_path, &serde_json::to_vec(&destination)?)?;
        }
        // Preserve the previous experimental shared-token queue for deliberate migration.
        if fs::read_dir(&config.spool_dir)?
            .any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("report-")))
        {
            return Err(io::Error::other("legacy shared-token reports found; preserve this outbox and use a new spool_dir for enrollment"));
        }
        let path = config.spool_dir.join("queue.json");
        let state = if path.exists() {
            load_state(&path)?
        } else {
            State::default()
        };
        let mut ids = std::collections::HashSet::new();
        for r in &state.reports {
            validate_payload(&r.payload)?;
            if r.payload["device_id"] != config.device_id
                || !ids.insert(r.payload["reading"]["report_id"].as_str().unwrap())
            {
                return Err(io::Error::other(
                    "invalid device or duplicate IDs in metrics snapshot",
                ));
            }
        }
        let now = now_unix();
        let remaining = if now > 0 && state.retry_not_before_unix > 0 {
            state.retry_not_before_unix.saturating_sub(now)
        } else {
            state.retry_delay_seconds
        };
        let retry_at = Instant::now() + Duration::from_secs(remaining.min(604800));
        Ok(Self {
            dir: config.spool_dir.clone(),
            state,
            healthy: true,
            missed_windows: 0,
            retry_at,
            _lock: lock,
        })
    }
    pub fn token(&self) -> io::Result<String> {
        let path = self.dir.join("device.token");
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                    return Err(io::Error::other(
                        "device.token must be a regular file with mode 0600",
                    ));
                }
                let token = fs::read_to_string(path)?;
                if token.len() != 64
                    || !token
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(io::Error::other(
                        "invalid persisted device token; preserving it for diagnosis",
                    ));
                }
                Ok(token)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if self.dir.join("queue.json").exists() {
                    return Err(io::Error::other("device credential missing; use a new spool_dir and explicit replacement approval"));
                }
                let mut bytes = [0; 32];
                SystemRandom::new()
                    .fill(&mut bytes)
                    .map_err(|_| io::Error::other("secure random generation failed"))?;
                let token = crate::protocol::hex(&bytes);
                atomic_write(&path, token.as_bytes())?;
                Ok(token)
            }
            Err(e) => Err(e),
        }
    }
    pub fn save(&mut self, state: State) -> io::Result<()> {
        if !self.healthy {
            return Err(io::Error::other("metrics storage disabled"));
        }
        let data = serde_json::to_value(&state)?;
        let digest = digest_hex(&serde_json::to_vec(&data)?);
        let result = atomic_write(
            &self.dir.join("queue.json"),
            &serde_json::to_vec(&json!({"sha256":digest,"state":data}))?,
        );
        if let Err(e) = result {
            self.healthy = false;
            return Err(e);
        }
        self.state = state;
        Ok(())
    }
    pub fn append(&mut self, report: SavedReport) -> io::Result<()> {
        validate_payload(&report.payload)?;
        let mut state = self.state.clone();
        if state.reports.len() == MAX_PENDING {
            state.reports.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
        state.reports.push_back(report);
        self.save(state)
    }
    pub fn diagnostics(&self) -> Value {
        json!({"type":"metrics_status","queue_depth":self.state.reports.len(),"queue_capacity":MAX_PENDING,
            "dropped":self.state.dropped,"rejected":self.state.rejected,"provisional_discarded":self.state.provisional_discarded,
            "storage_ok":self.healthy,"missed_windows_since_boot":self.missed_windows,"last_http_status":self.state.last_http_status,
            "enrollment_status":self.state.enrollment_status,"retry_not_before_unix":self.state.retry_not_before_unix})
    }
}

pub fn read_status(dir: &Path) -> io::Result<Value> {
    let s = load_state(&dir.join("queue.json"))?;
    Ok(
        json!({"queue_depth":s.reports.len(),"queue_capacity":MAX_PENDING,"dropped":s.dropped,
        "rejected":s.rejected,"provisional_discarded":s.provisional_discarded,
        "last_http_status":s.last_http_status,"enrollment_status":s.enrollment_status,
        "retry_not_before_unix":s.retry_not_before_unix,"snapshot_valid":true}),
    )
}
