use super::{
    now_unix,
    store::{SavedReport, Store},
    validate_payload, MAX_BODY, PROVISIONAL_DELAY,
};
use reqwest::{blocking::Client, header::RETRY_AFTER};
use serde_json::Value;
use std::{
    io::{self, Read},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Approved,
    Provisional {
        sample_recorded: bool,
        duplicate: bool,
        delay: u64,
    },
    Rejected,
    Retry(u64),
}
fn backoff(body: &Value, header: Option<&str>) -> u64 {
    body["retry_after_seconds"]
        .as_u64()
        .unwrap_or(0)
        .max(header.and_then(|s| s.parse().ok()).unwrap_or(0))
        .clamp(PROVISIONAL_DELAY, 604800)
}
pub fn classify(status: u16, body: &Value, header: Option<&str>, id: &str) -> Outcome {
    if status == 429 {
        return Outcome::Retry(backoff(body, header));
    }
    if matches!(status, 400 | 409) {
        return Outcome::Rejected;
    }
    if matches!(status, 401 | 403) {
        return Outcome::Retry(300);
    }
    let matched = body["report_id"].as_str() == Some(id) && body["metric_duplicate"].is_boolean();
    if status == 202 {
        // Even a malformed provisional acknowledgment must not cause rapid retries.
        let delay = backoff(body, header);
        if matched
            && body["status"] == "provisional"
            && body["sample_recorded"].is_boolean()
            && body["sample_count"].as_u64().is_some_and(|n| n <= 5)
        {
            return Outcome::Provisional {
                sample_recorded: body["sample_recorded"].as_bool().unwrap(),
                duplicate: body["metric_duplicate"].as_bool().unwrap(),
                delay,
            };
        }
        return Outcome::Retry(delay);
    }
    if status == 201 && body["status"] == "approved" && matched {
        Outcome::Approved
    } else {
        Outcome::Retry(30)
    }
}
fn deliver(client: &Client, url: &str, token: &str, payload: &Value) -> (Option<u16>, Outcome) {
    if validate_payload(payload).is_err() {
        return (None, Outcome::Rejected);
    }
    let Ok(response) = client.post(url).bearer_auth(token).json(payload).send() else {
        return (None, Outcome::Retry(30));
    };
    let status = response.status().as_u16();
    let header = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    let body = if response
        .take((MAX_BODY + 1) as u64)
        .read_to_end(&mut bytes)
        .is_ok()
        && bytes.len() <= MAX_BODY
    {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    (
        Some(status),
        classify(
            status,
            &body,
            header.as_deref(),
            payload["reading"]["report_id"].as_str().unwrap(),
        ),
    )
}

pub fn freeze(
    store: &mut Store,
    boot: &str,
    elapsed_ms: u64,
    now: u64,
) -> io::Result<Option<SavedReport>> {
    let Some(report) = store.state.reports.front() else {
        return Ok(None);
    };
    if report.frozen {
        return Ok(Some(report.clone()));
    }
    let mut state = store.state.clone();
    let report = state.reports.front_mut().unwrap();
    if report.payload["reading"]["time_quality"] == "unknown"
        && report.payload["reading"]["boot_id"] == boot
        && now > 0
    {
        let observed = now.saturating_sub(elapsed_ms.saturating_sub(report.elapsed_ms) / 1000);
        if observed > 0 {
            report.payload["reading"]["observed_at_unix"] = observed.into();
            report.payload["reading"]["time_quality"] = "estimated".into();
        }
    }
    report.frozen = true;
    let result = report.clone();
    // Freeze timestamps/values durably before a request can reach the server.
    store.save(state)?;
    Ok(Some(result))
}

pub fn apply(
    store: &mut Store,
    report: &SavedReport,
    status: Option<u16>,
    outcome: Outcome,
    now: u64,
) -> io::Result<Duration> {
    let mut state = store.state.clone();
    state.last_http_status = status;
    let id = &report.payload["reading"]["report_id"];
    let position = state
        .reports
        .iter()
        .position(|r| &r.payload["reading"]["report_id"] == id);
    let mut remove = false;
    let delay = match outcome {
        Outcome::Approved => {
            state.enrollment_status = Some("approved".into());
            remove = true;
            0
        }
        Outcome::Provisional {
            sample_recorded,
            duplicate,
            delay,
        } => {
            state.enrollment_status = Some("provisional".into());
            remove = true;
            if !sample_recorded && !duplicate {
                state.provisional_discarded = state.provisional_discarded.saturating_add(1);
            }
            delay
        }
        Outcome::Rejected => {
            state.rejected = state.rejected.saturating_add(1);
            state.latest_rejected = Some(report.payload.clone());
            remove = true;
            0
        }
        Outcome::Retry(delay) => {
            if status == Some(403) {
                state.enrollment_status = Some("forbidden_or_revoked".into());
            }
            if status == Some(202) {
                state.enrollment_status = Some("provisional".into());
            }
            delay
        }
    };
    // A full ring may evict an in-flight record. Never acknowledge a different head.
    if remove {
        if let Some(index) = position {
            state.reports.remove(index);
        }
    }
    state.retry_delay_seconds = delay;
    state.retry_not_before_unix = if now > 0 {
        now.saturating_add(delay)
    } else {
        0
    };
    store.save(state)?;
    let wait = if delay == 0 {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(delay)
    };
    store.retry_at = Instant::now() + wait;
    Ok(wait)
}

pub fn run(
    client: Client,
    store: Arc<Mutex<Store>>,
    url: String,
    token: String,
    boot: String,
    started: Instant,
) {
    loop {
        let prepared = {
            let mut s = store.lock().unwrap();
            if !s.healthy {
                return;
            }
            if Instant::now() < s.retry_at {
                None
            } else {
                match freeze(
                    &mut s,
                    &boot,
                    started.elapsed().as_millis() as u64,
                    now_unix(),
                ) {
                    Ok(report) => report,
                    Err(e) => {
                        log::error!("Metrics storage fault: {e}; uploads stopped");
                        return;
                    }
                }
            }
        };
        if let Some(report) = prepared {
            let (status, outcome) = deliver(&client, &url, &token, &report.payload);
            log::info!("Metrics upload: HTTP {:?}, {:?}", status, outcome);
            let mut s = store.lock().unwrap();
            if let Err(e) = apply(&mut s, &report, status, outcome, now_unix()) {
                log::error!("Metrics storage fault: {e}; uploads stopped");
                return;
            }
            println!("{}", s.diagnostics());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
pub fn test_deliver(
    client: &Client,
    url: &str,
    token: &str,
    payload: &Value,
) -> (Option<u16>, Outcome) {
    deliver(client, url, token, payload)
}
