//! Heron device enrollment and five-minute reception metrics.
mod store;
#[cfg(test)]
mod tests;
mod upload;

use crate::protocol::decode;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::IpAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use store::{SavedReport, Store};

const WINDOW_MS: u64 = 300_000;
const MAX_PENDING: usize = 2048;
const MAX_BODY: usize = 16 * 1024;
const PROVISIONAL_DELAY: u64 = 10800;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub url: String,
    pub device_id: String,
    pub name: String,
    pub ip_address: IpAddr,
    pub port: u16,
    #[serde(default)]
    pub public_channel: u8,
    pub spool_dir: PathBuf,
    #[serde(default)]
    pub root_ca_file: Option<PathBuf>,
}
impl Config {
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut config: Self =
            serde_json::from_reader(File::open(path)?).map_err(io::Error::other)?;
        let url = Url::parse(&config.url).map_err(|_| io::Error::other("invalid metrics URL"))?;
        let local = url.host_str().is_some_and(|h| {
            h == "localhost"
                || h.ends_with(".localhost")
                || h.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(url.scheme() == "https" || url.scheme() == "http" && local)
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
        {
            return Err(io::Error::other("metrics require HTTPS (HTTP allowed only for localhost development), without URL credentials, query or fragment"));
        }
        if !valid_id(&config.device_id, 80) || !valid_id(&config.name, 160) || config.port == 0 {
            return Err(io::Error::other("invalid metrics device identity or port"));
        }
        let base = path.parent().unwrap_or(Path::new("."));
        if config.spool_dir.is_relative() {
            config.spool_dir = base.join(&config.spool_dir);
        }
        if let Some(ca) = &mut config.root_ca_file {
            if ca.is_relative() {
                *ca = base.join(&*ca);
            }
        }
        Ok(config)
    }
    fn client(&self) -> io::Result<Client> {
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(Policy::none());
        if let Some(path) = &self.root_ca_file {
            let ca = reqwest::Certificate::from_pem(&fs::read(path)?)
                .map_err(|_| io::Error::other("invalid root CA PEM"))?;
            builder = builder.add_root_certificate(ca);
        }
        builder
            .build()
            .map_err(|_| io::Error::other("cannot initialize HTTPS client"))
    }
}
fn valid_id(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn now_unix() -> u64 {
    // A plausible system clock is estimated, not claimed to be NTP-synchronized.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_secs())
        .ok()
        .filter(|t| *t >= 1_577_836_800)
        .unwrap_or(0)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    File::open(path.parent().unwrap())?.sync_all()
}

fn validate_payload(payload: &Value) -> io::Result<()> {
    fn depth(v: &Value) -> usize {
        match v {
            Value::Object(m) => m.values().map(|v| 1 + depth(v)).max().unwrap_or(0),
            Value::Array(a) => a.iter().map(|v| 1 + depth(v)).max().unwrap_or(0),
            _ => 0,
        }
    }
    let r = &payload["reading"];
    let valid_text = |key, limit| r[key].as_str().is_some_and(|s| valid_id(s, limit));
    let schema = r["schema"].as_str().unwrap_or("");
    let start = r["window_start_ms"].as_u64();
    let end = r["window_end_ms"].as_u64();
    let duration = r["window_duration_ms"].as_u64();
    let window = match (start, end, duration) {
        (Some(a), Some(b), Some(d))
            if a <= u32::MAX as u64 && b <= u32::MAX as u64 && d > 0 && d <= u32::MAX as u64 =>
        {
            (b as u32).wrapping_sub(a as u32) == d as u32
        }
        _ => false,
    };
    let time = match r["time_quality"].as_str() {
        Some("unknown") => r["observed_at_unix"].as_i64() == Some(0),
        None | Some("estimated" | "synchronized") => {
            r["observed_at_unix"].as_i64().is_some_and(|t| t > 0)
        }
        _ => false,
    };
    if !payload["device_id"]
        .as_str()
        .is_some_and(|s| valid_id(s, 80))
        || schema.is_empty()
        || schema.len() > 80
        || !schema
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        || !r["schema_version"]
            .as_u64()
            .is_some_and(|v| (1..=i32::MAX as u64).contains(&v))
        || !valid_text("report_id", 160)
        || !valid_text("boot_id", 160)
        || !window
        || !time
        || !r["data"].is_object()
        || serde_json::to_vec(&r["data"])?.len() > 12 * 1024
        || depth(&r["data"]) > 8
        || serde_json::to_vec(payload)?.len() > MAX_BODY
    {
        return Err(io::Error::other(
            "metric exceeds Heron envelope/data limits or has invalid timing/identity",
        ));
    }
    Ok(())
}
#[derive(Default)]
struct Window {
    start_ms: u64,
    public: u32,
    dm: u32,
}
impl Window {
    fn count(&mut self, packet: &[u8], public_channel: u8) {
        let Ok(event) = decode(packet) else {
            return;
        };
        // CLI requests/responses are not person-to-person chat. Signed text (2) is chat.
        if !matches!(event["text_type"].as_u64(), Some(0 | 2)) {
            return;
        }
        if event["type"] == "channel_message" && event["channel"] == public_channel {
            self.public = self.public.saturating_add(1);
        } else if event["type"] == "direct_message" {
            self.dm = self.dm.saturating_add(1);
        }
    }
    fn close(&mut self, elapsed_ms: u64, now_unix: u64, boot_id: &str) -> Vec<Value> {
        let mut reports = Vec::new();
        while elapsed_ms.saturating_sub(self.start_ms) >= WINDOW_MS {
            let end = self.start_ms + WINDOW_MS;
            reports.push(json!({
                "schema":"meshcore_message_count", "schema_version":1,
                "report_id":format!("{boot_id}-{}", end / WINDOW_MS), "boot_id":boot_id,
                "observed_at_unix":now_unix.saturating_sub((elapsed_ms-end)/1000),
                "window_start_ms":self.start_ms as u32, "window_end_ms":end as u32,
                "window_duration_ms":WINDOW_MS,
                "data":{"public_count":self.public,"dm_count":self.dm}
            }));
            self.start_ms = end;
            self.public = 0;
            self.dm = 0;
        }
        reports
    }
}

pub struct Reporter {
    config: Config,
    started: Instant,
    boot_id: String,
    window: Window,
    store: Arc<Mutex<Store>>,
}
impl Reporter {
    pub fn start(config: Config) -> io::Result<Self> {
        let mut store = Store::open(&config)?;
        let token = store.token()?;
        let client = config.client()?;
        store.save(store.state.clone())?;
        let store = Arc::new(Mutex::new(store));
        let started = Instant::now();
        let mut random = [0; 16];
        use ring::rand::SecureRandom;
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| io::Error::other("secure random generation failed"))?;
        let boot_id = format!("meshcore-{}", crate::protocol::hex(&random));
        let upload_store = Arc::clone(&store);
        let url = config.url.clone();
        let upload_boot = boot_id.clone();
        thread::Builder::new()
            .name("metrics-upload".into())
            .spawn(move || upload::run(client, upload_store, url, token, upload_boot, started))?;
        log::info!("Metrics enabled: public channel {}, DMs, five-minute windows; first upload enrolls device for admin approval",config.public_channel);
        Ok(Self {
            config,
            started,
            boot_id,
            window: Window::default(),
            store,
        })
    }
    pub fn tick(&mut self, packet: Option<&[u8]>) -> io::Result<()> {
        let elapsed = self.started.elapsed().as_millis() as u64;
        let now = now_unix();
        let first_end = self.window.start_ms + WINDOW_MS;
        for (index, mut reading) in self
            .window
            .close(elapsed, now, &self.boot_id)
            .into_iter()
            .enumerate()
        {
            reading["data"]["public_channel"] = self.config.public_channel.into();
            if now == 0 {
                reading["observed_at_unix"] = 0.into();
                reading["time_quality"] = "unknown".into();
            } else {
                reading["time_quality"] = "estimated".into();
            }
            let payload = json!({"device_id":self.config.device_id,"name":self.config.name,
                "ip_address":self.config.ip_address.to_string(),"port":self.config.port,
                "deployed_code":{"name":"flood-monitor-meshcore","version":env!("CARGO_PKG_VERSION")},
                "capabilities":["meshcore_message_count"],"reading":reading});
            let mut store = self.store.lock().unwrap();
            if let Err(e) = store.append(SavedReport {
                payload,
                elapsed_ms: first_end + index as u64 * WINDOW_MS,
                frozen: false,
            }) {
                store.missed_windows = store.missed_windows.saturating_add(1);
                log::error!("Metrics window not saved: {e}; collection continues, reporting storage requires attention");
            }
            println!("{}", store.diagnostics());
        }
        if let Some(p) = packet {
            self.window.count(p, self.config.public_channel);
        }
        Ok(())
    }
}

pub fn status(config: &Config) -> io::Result<Value> {
    // Read-only snapshot: no lock acquisition or creation while a monitor is running.
    store::read_status(&config.spool_dir)
}
