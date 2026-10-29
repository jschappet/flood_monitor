use super::upload::{self, Outcome};
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
};

static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "flood-metrics-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config(path: &Path) -> Config {
    Config {
        url: "https://example.test/api/iot".into(),
        device_id: "meshcore-test".into(),
        name: "Test".into(),
        ip_address: "127.0.0.1".parse().unwrap(),
        port: 80,
        public_channel: 0,
        spool_dir: path.into(),
        root_ca_file: None,
    }
}
fn report(id: &str) -> SavedReport {
    SavedReport {
        payload: json!({"device_id":"meshcore-test","name":"Test","ip_address":"127.0.0.1","port":80,
        "deployed_code":{"name":"flood-monitor-meshcore","version":"0.1.0"},"capabilities":["meshcore_message_count"],
        "reading":{"schema":"meshcore_message_count","schema_version":1,"report_id":id,"boot_id":"boot",
        "observed_at_unix":1793145000u64,"time_quality":"estimated","window_start_ms":0,"window_end_ms":300000,
        "window_duration_ms":300000,"data":{"public_channel":0,"public_count":5,"dm_count":2}}}),
        elapsed_ms: 300000,
        frozen: false,
    }
}
fn channel(index: u8) -> Vec<u8> {
    vec![8, index, 0, 0, 1, 0, 0, 0, b'x']
}
fn direct(kind: u8) -> Vec<u8> {
    vec![7, 1, 2, 3, 4, 5, 6, 0, kind, 1, 0, 0, 0, b'x']
}
fn ack(id: &str, status: &str) -> Value {
    json!({"report_id":id,"status":status,"metric_duplicate":false,"sample_recorded":true,"sample_count":1,"retry_after_seconds":10800})
}

#[test]
fn counting_boundaries_idle_windows_and_wraparound() {
    let mut w = Window::default();
    for p in [
        channel(0),
        channel(1),
        direct(0),
        direct(1),
        direct(3),
        vec![0x88, 0],
        vec![8],
    ] {
        w.count(&p, 0);
    }
    assert!(w.close(299999, 1793145000, "boot").is_empty());
    let r = w.close(300000, 1793145000, "boot");
    assert_eq!(r[0]["data"], json!({"public_count":1,"dm_count":1}));
    w.count(&channel(0), 0);
    let r = w.close(900000, 1793145600, "boot");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0]["data"]["public_count"], 1);
    assert_eq!(r[1]["data"]["public_count"], 0);
    let mut w = Window {
        start_ms: u32::MAX as u64 - 1000,
        ..Window::default()
    };
    let r = w.close(w.start_ms + WINDOW_MS, 1793145000, "boot");
    let a = r[0]["window_start_ms"].as_u64().unwrap() as u32;
    let b = r[0]["window_end_ms"].as_u64().unwrap() as u32;
    assert_eq!(b.wrapping_sub(a), 300000);
}
#[test]
fn token_is_private_stable_random_and_bound_to_destination() {
    let temp = Temp::new();
    let c = config(&temp.0);
    let mut store = Store::open(&c).unwrap();
    let token = store.token().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(
        fs::metadata(temp.0.join("device.token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(Store::open(&c).is_err()); // Single writer, including another process.
    store.append(report("r1")).unwrap();
    drop(store);
    let store = Store::open(&c).unwrap();
    assert_eq!(store.token().unwrap(), token);
    assert!(!fs::read_to_string(temp.0.join("queue.json"))
        .unwrap()
        .contains(&token));
    assert!(!fs::read_to_string(temp.0.join("destination.json"))
        .unwrap()
        .contains(&token));
    drop(store);
    let mut changed = config(&temp.0);
    changed.url = "https://other.test/api/iot".into();
    assert!(Store::open(&changed).is_err());
    changed = config(&temp.0);
    changed.device_id = "other-device".into();
    assert!(Store::open(&changed).is_err());
    let other = Temp::new();
    let other = Store::open(&config(&other.0)).unwrap();
    assert_ne!(other.token().unwrap(), token);
    fs::remove_file(temp.0.join("device.token")).unwrap();
    assert!(Store::open(&c).unwrap().token().is_err()); // No silent re-enrollment.
}
#[test]
fn provisional_approved_rate_limit_and_errors_are_distinct() {
    let p = ack("r1", "provisional");
    assert_eq!(
        upload::classify(202, &p, None, "r1"),
        Outcome::Provisional {
            sample_recorded: true,
            duplicate: false,
            delay: 10800
        }
    );
    assert_eq!(
        upload::classify(202, &Value::Null, None, "r1"),
        Outcome::Retry(10800)
    );
    assert_eq!(
        upload::classify(202, &p, None, "wrong"),
        Outcome::Retry(10800)
    );
    assert_eq!(
        upload::classify(201, &ack("r1", "approved"), None, "r1"),
        Outcome::Approved
    );
    assert_eq!(
        upload::classify(201, &ack("wrong", "approved"), None, "r1"),
        Outcome::Retry(30)
    );
    assert_eq!(
        upload::classify(429, &Value::Null, None, "r1"),
        Outcome::Retry(10800)
    );
    assert_eq!(
        upload::classify(429, &Value::Null, Some("14400"), "r1"),
        Outcome::Retry(14400)
    );
    for status in [401, 403] {
        assert_eq!(
            upload::classify(status, &Value::Null, None, "r1"),
            Outcome::Retry(300)
        );
    }
    for status in [400, 409] {
        assert_eq!(
            upload::classify(status, &Value::Null, None, "r1"),
            Outcome::Rejected
        );
    }
    for status in [200, 302, 500, 503] {
        assert_eq!(
            upload::classify(status, &Value::Null, None, "r1"),
            Outcome::Retry(30)
        );
    }
}
#[test]
fn provisional_discard_reject_and_backoff_survive_restart() {
    let temp = Temp::new();
    let c = config(&temp.0);
    let mut s = Store::open(&c).unwrap();
    s.token().unwrap();
    s.append(report("r1")).unwrap();
    s.append(report("r2")).unwrap();
    let r = upload::freeze(&mut s, "boot", 300000, now_unix())
        .unwrap()
        .unwrap();
    upload::apply(
        &mut s,
        &r,
        Some(202),
        Outcome::Provisional {
            sample_recorded: false,
            duplicate: false,
            delay: 10800,
        },
        now_unix(),
    )
    .unwrap();
    assert_eq!(s.state.reports.len(), 1);
    assert_eq!(s.state.provisional_discarded, 1);
    drop(s);
    let mut s = Store::open(&c).unwrap();
    assert!(s.retry_at.duration_since(Instant::now()).as_secs() > 10790);
    let r = s.state.reports.front().unwrap().clone();
    upload::apply(&mut s, &r, Some(409), Outcome::Rejected, now_unix()).unwrap();
    assert_eq!(s.state.rejected, 1);
    assert!(s.state.reports.is_empty());
    assert_eq!(
        s.state.latest_rejected.as_ref().unwrap()["reading"]["report_id"],
        "r2"
    );
    drop(s);
    let s = Store::open(&c).unwrap();
    assert_eq!(s.state.rejected, 1);
}
#[test]
fn queue_overflow_is_bounded_and_inflight_ack_cannot_remove_another_record() {
    let temp = Temp::new();
    let c = config(&temp.0);
    let mut s = Store::open(&c).unwrap();
    s.token().unwrap();
    let mut state = s.state.clone();
    for i in 0..MAX_PENDING {
        state.reports.push_back(report(&format!("r{i}")));
    }
    s.save(state).unwrap();
    let inflight = s.state.reports.front().unwrap().clone();
    s.append(report("new")).unwrap();
    assert_eq!(s.state.dropped, 1);
    assert_eq!(s.state.reports.len(), MAX_PENDING);
    upload::apply(&mut s, &inflight, Some(201), Outcome::Approved, now_unix()).unwrap();
    assert_eq!(s.state.reports.len(), MAX_PENDING);
    assert_eq!(
        s.state.reports.front().unwrap().payload["reading"]["report_id"],
        "r1"
    );
    drop(s);
    let s = Store::open(&c).unwrap();
    assert_eq!(s.state.dropped, 1);
}
#[test]
fn timestamps_are_frozen_before_upload_and_unknown_across_boots() {
    let temp = Temp::new();
    let c = config(&temp.0);
    let mut s = Store::open(&c).unwrap();
    s.token().unwrap();
    let mut r = report("r1");
    r.payload["reading"]["observed_at_unix"] = 0.into();
    r.payload["reading"]["time_quality"] = "unknown".into();
    s.append(r.clone()).unwrap();
    let first = upload::freeze(&mut s, "boot", 600000, 1793145600)
        .unwrap()
        .unwrap();
    assert_eq!(first.payload["reading"]["observed_at_unix"], 1793145300u64);
    drop(s);
    let mut s = Store::open(&c).unwrap();
    assert_eq!(
        upload::freeze(&mut s, "new-boot", 0, 1793150000)
            .unwrap()
            .unwrap()
            .payload,
        first.payload
    );
    upload::apply(&mut s, &first, Some(201), Outcome::Approved, now_unix()).unwrap();
    r.payload["reading"]["report_id"] = "r2".into();
    s.append(r).unwrap();
    assert_eq!(
        upload::freeze(&mut s, "new-boot", 0, 1793150000)
            .unwrap()
            .unwrap()
            .payload["reading"]["time_quality"],
        "unknown"
    );
}
#[test]
fn generic_limits_and_timestamps_are_validated() {
    let p = report("r1").payload;
    assert!(validate_payload(&p).is_ok());
    let mut p = p.clone();
    p["reading"]["schema"] = "future.metric-1".into();
    p["reading"]["data"] = json!({"arbitrary":"value"});
    assert!(validate_payload(&p).is_ok());
    for (key, value) in [
        ("schema", json!("bad schema")),
        ("schema_version", json!(0)),
        ("report_id", json!("")),
        ("data", json!([])),
        ("observed_at_unix", json!(0)),
    ] {
        let mut bad = p.clone();
        bad["reading"][key] = value;
        assert!(validate_payload(&bad).is_err(), "{key}");
    }
    let mut bad = p.clone();
    bad["reading"]["data"] = json!({"text":"x".repeat(12*1024)});
    assert!(validate_payload(&bad).is_err());
    let mut nested = json!(0);
    for _ in 0..9 {
        nested = json!({"n":nested});
    }
    bad["reading"]["data"] = nested;
    assert!(validate_payload(&bad).is_err());
    let mut bad = p.clone();
    bad["extra"] = "x".repeat(16 * 1024).into();
    assert!(validate_payload(&bad).is_err());
    let mut unknown = p;
    unknown["reading"]["observed_at_unix"] = 0.into();
    unknown["reading"]["time_quality"] = "unknown".into();
    assert!(validate_payload(&unknown).is_ok());
}
#[test]
fn corruption_and_write_failure_stop_mutations_without_erasing_queue() {
    let temp = Temp::new();
    let c = config(&temp.0);
    let mut s = Store::open(&c).unwrap();
    s.token().unwrap();
    s.append(report("r1")).unwrap();
    let before = fs::read(temp.0.join("queue.json")).unwrap();
    fs::create_dir(temp.0.join("queue.tmp")).unwrap();
    assert!(s.append(report("r2")).is_err());
    assert!(!s.healthy);
    assert_eq!(fs::read(temp.0.join("queue.json")).unwrap(), before);
    assert_eq!(s.state.reports.len(), 1);
    fs::remove_dir(temp.0.join("queue.tmp")).unwrap();
    assert!(s.append(report("r3")).is_err());
    drop(s);
    let mut damaged: Value = serde_json::from_slice(&before).unwrap();
    damaged["state"]["dropped"] = 99.into();
    fs::write(
        temp.0.join("queue.json"),
        serde_json::to_vec(&damaged).unwrap(),
    )
    .unwrap();
    assert!(Store::open(&c).is_err());
}
#[test]
fn http_posts_use_token_without_leaking_it_and_do_not_follow_redirects() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/api/iot", server.local_addr().unwrap());
    let token = "ab".repeat(32);
    let expected = token.clone();
    let worker = thread::spawn(move || {
        let mut bodies = Vec::new();
        for (status, body) in [
            ("202 Accepted", ack("r1", "provisional")),
            (
                "429 Too Many Requests",
                json!({"retry_after_seconds":10800}),
            ),
            ("302 Found", Value::Null),
            ("201 Created", ack("r1", "approved")),
        ] {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                header.push(b[0]);
            }
            let header = String::from_utf8(header).unwrap().to_lowercase();
            assert!(header.starts_with("post /api/iot "));
            assert!(header.contains(&format!("authorization: bearer {expected}")));
            let len: usize = header
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut bytes = vec![0; len];
            stream.read_exact(&mut bytes).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(&expected));
            bodies.push(bytes);
            let body = body.to_string();
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nLocation: /wrong\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]));
    });
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let p = report("r1").payload;
    assert!(matches!(
        upload::test_deliver(&client, &url, &token, &p).1,
        Outcome::Provisional { delay: 10800, .. }
    ));
    assert_eq!(
        upload::test_deliver(&client, &url, &token, &p).1,
        Outcome::Retry(10800)
    );
    assert_eq!(
        upload::test_deliver(&client, &url, &token, &p).1,
        Outcome::Retry(30)
    );
    assert_eq!(
        upload::test_deliver(&client, &url, &token, &p).1,
        Outcome::Approved
    );
    worker.join().unwrap();
}

#[test]
fn config_requires_remote_tls_and_supports_relative_ca_without_shared_token() {
    let temp = Temp::new();
    fs::create_dir_all(&temp.0).unwrap();
    let path = temp.0.join("config.json");
    let mut value = json!({"url":"https://example.test/api/iot","device_id":"test","name":"test","ip_address":"127.0.0.1","port":80,"spool_dir":"outbox","root_ca_file":"root.pem"});
    fs::write(&path, value.to_string()).unwrap();
    let c = Config::load(&path).unwrap();
    assert_eq!(c.spool_dir, temp.0.join("outbox"));
    assert_eq!(c.root_ca_file, Some(temp.0.join("root.pem")));
    for url in [
        "http://public.example/api/iot",
        "https://user:secret@example.test/api/iot",
        "https://example.test/api/iot?token=secret",
    ] {
        value["url"] = url.into();
        fs::write(&path, value.to_string()).unwrap();
        assert!(Config::load(&path).is_err());
    }
    value["url"] = "http://workshop.localhost:8582/api/iot".into();
    fs::write(&path, value.to_string()).unwrap();
    assert!(Config::load(&path).is_ok());
}
#[test]
fn provisional_duplicate_does_not_count_as_discard_and_revocation_never_rotates_token() {
    let temp = Temp::new();
    let c = config(&temp.0);
    let mut s = Store::open(&c).unwrap();
    let token = s.token().unwrap();
    let r = report("r1");
    s.append(r.clone()).unwrap();
    upload::apply(
        &mut s,
        &r,
        Some(202),
        Outcome::Provisional {
            sample_recorded: false,
            duplicate: true,
            delay: 10800,
        },
        now_unix(),
    )
    .unwrap();
    assert_eq!(s.state.provisional_discarded, 0);
    s.append(report("r2")).unwrap();
    let r = s.state.reports.front().unwrap().clone();
    upload::apply(&mut s, &r, Some(403), Outcome::Retry(300), now_unix()).unwrap();
    assert_eq!(s.state.reports.len(), 1);
    assert_eq!(s.token().unwrap(), token);
    let diagnostic = s.diagnostics().to_string();
    assert!(!diagnostic.contains(&token));
    assert!(diagnostic.contains("forbidden_or_revoked"));
}
