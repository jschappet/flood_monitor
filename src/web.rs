use crate::protocol::{chat_command, decode, hex};
use hyper::{
    body::HttpBody,
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server, StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    convert::Infallible,
    io,
    net::{SocketAddr, TcpListener},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub type Hub = Arc<Mutex<State>>;
const HISTORY: usize = 1000;
#[derive(Clone, Serialize)]
pub struct Message {
    pub id: u64,
    pub conversation: String,
    pub direction: String,
    pub sender: String,
    pub text: String,
    pub timestamp: u64,
    pub snr_db: Option<f64>,
    pub status: String,
    #[serde(skip)]
    request_id: Option<String>,
    #[serde(skip)]
    ack: Option<String>,
    #[serde(skip)]
    deadline: Option<Instant>,
}
#[derive(Clone, Serialize)]
pub struct Contact {
    pub key: String,
    pub name: String,
    pub prefix: String,
}
#[derive(Clone)]
pub struct Pending {
    pub id: u64,
    pub target: String,
    pub text: String,
}
pub struct State {
    pub name: String,
    pub connected: bool,
    pub channel: u8,
    pub contacts: BTreeMap<String, Contact>,
    pub messages: VecDeque<Message>,
    pub outbox: VecDeque<Pending>,
    pub refresh: bool,
    pub refreshing: bool,
    pub contacts_error: Option<String>,
    next_id: u64,
    pub last_tx_timestamp: u32,
    acks: VecDeque<(String, Instant)>,
}
impl State {
    pub fn new(channel: u8) -> Self {
        Self {
            name: "MeshCore node".into(),
            connected: false,
            channel,
            contacts: BTreeMap::new(),
            messages: VecDeque::new(),
            outbox: VecDeque::new(),
            refresh: false,
            refreshing: false,
            contacts_error: None,
            next_id: 1,
            last_tx_timestamp: 0,
            acks: VecDeque::new(),
        }
    }
    fn push(&mut self, mut message: Message) -> u64 {
        message.id = self.next_id;
        self.next_id += 1;
        if self.messages.len() == HISTORY {
            self.messages.pop_front();
        }
        let id = message.id;
        self.messages.push_back(message);
        id
    }
    pub fn observe(&mut self, p: &[u8]) {
        let Ok(e) = decode(p) else { return };
        match e["type"].as_str().unwrap_or("") {
            "self_info" => {
                self.name = e["name"].as_str().unwrap_or("MeshCore node").into();
            }
            "contact" => {
                let key = e["public_key"].as_str().unwrap().to_owned();
                if self.contacts.len() < 2048 || self.contacts.contains_key(&key) {
                    self.contacts.insert(
                        key.clone(),
                        Contact {
                            prefix: key[..12].into(),
                            key,
                            name: e["name"].as_str().unwrap().into(),
                        },
                    );
                }
            }
            "contact_deleted" => {
                self.contacts.remove(e["public_key"].as_str().unwrap());
            }
            "delivery_ack" => {
                let tag = e["tag"].as_str().unwrap().to_owned();
                self.acks.push_back((tag.clone(), Instant::now()));
                while self.acks.len() > 64 {
                    self.acks.pop_front();
                }
                for m in &mut self.messages {
                    if m.ack.as_ref() == Some(&tag) {
                        m.status = "delivered".into();
                        m.deadline = None;
                    }
                }
            }
            "channel_message" | "direct_message" => {
                if !matches!(e["text_type"].as_u64(), Some(0 | 2)) {
                    return;
                }
                let public = e["type"] == "channel_message";
                if public && e["channel"] != self.channel {
                    return;
                }
                let conversation = if public {
                    "public".to_owned()
                } else {
                    e["sender_key_prefix"].as_str().unwrap().to_owned()
                };
                let raw = e["text"].as_str().unwrap_or("");
                let (sender, text) = if public {
                    raw.split_once(": ").unwrap_or(("Public channel", raw))
                } else {
                    ("", raw)
                };
                self.push(Message {
                    id: 0,
                    conversation,
                    direction: "in".into(),
                    sender: sender.into(),
                    text: text.into(),
                    timestamp: e["timestamp"].as_u64().unwrap_or(0),
                    snr_db: e["snr_db"].as_f64(),
                    status: "received".into(),
                    request_id: None,
                    ack: None,
                    deadline: None,
                });
            }
            _ => {}
        }
    }
    pub fn can_dm(&self, prefix: &str) -> bool {
        !self.refreshing
            && self
                .contacts
                .values()
                .filter(|c| c.prefix == prefix)
                .count()
                == 1
    }
    pub fn snapshot(&mut self) -> Value {
        for m in &mut self.messages {
            if m.deadline.is_some_and(|d| Instant::now() > d) {
                m.status = "unconfirmed".into();
                m.deadline = None;
            }
        }
        json!({"name":self.name,"connected":self.connected,"public_channel":self.channel,"public_limit":133usize.saturating_sub(self.name.len()+2),"dm_limit":133,"contacts":self.contacts.values().collect::<Vec<_>>(),"messages":self.messages,"refreshing":self.refreshing,"contacts_error":self.contacts_error})
    }
    pub fn enqueue(&mut self, r: SendRequest) -> Result<u64, (StatusCode, &'static str)> {
        if !self.connected {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "Radio is not connected"));
        }
        if r.request_id.len() < 8
            || r.request_id.len() > 80
            || !r
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err((StatusCode::BAD_REQUEST, "Invalid request ID"));
        }
        if let Some(m) = self
            .messages
            .iter()
            .find(|m| m.request_id.as_ref() == Some(&r.request_id))
        {
            if m.text == r.text && m.conversation == r.target {
                return Ok(m.id);
            }
            return Err((
                StatusCode::CONFLICT,
                "Request ID already used for another message",
            ));
        }
        if r.target != "public" && !self.can_dm(&r.target) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Recipient is missing or ambiguous. Refresh contacts before replying.",
            ));
        }
        chat_command(&r.target, self.channel, &self.name, &r.text, 0)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        if self.outbox.len() >= 32 {
            return Err((StatusCode::TOO_MANY_REQUESTS, "Send queue is full"));
        }
        let id = self.push(Message {
            id: 0,
            conversation: r.target.clone(),
            direction: "out".into(),
            sender: self.name.clone(),
            text: r.text.clone(),
            timestamp: unix(),
            snr_db: None,
            status: "queued".into(),
            request_id: Some(r.request_id),
            ack: None,
            deadline: None,
        });
        self.outbox.push_back(Pending {
            id,
            target: r.target,
            text: r.text,
        });
        Ok(id)
    }
    pub fn sent(&mut self, id: u64, response: &[u8]) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.id == id) {
            m.status = "submitted".into();
            if response.first() == Some(&6) && response.len() >= 10 {
                let tag = hex(&response[2..6]);
                if self
                    .acks
                    .iter()
                    .any(|(a, t)| a == &tag && t.elapsed() < Duration::from_secs(60))
                {
                    m.status = "delivered".into();
                } else {
                    m.status = "awaiting_ack".into();
                    let ms = u32::from_le_bytes(response[6..10].try_into().unwrap());
                    m.deadline = Some(
                        Instant::now() + Duration::from_millis(u64::from(ms).clamp(1000, 300000)),
                    );
                }
                m.ack = Some(tag);
            }
        }
    }
    pub fn failed(&mut self, id: u64, uncertain: bool) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.id == id) {
            m.status = if uncertain { "unknown" } else { "failed" }.into();
        }
    }
}
pub fn unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    pub request_id: String,
    pub target: String,
    pub text: String,
}

fn response(status: StatusCode, kind: &str, body: impl Into<Body>) -> Response<Body> {
    Response::builder().status(status).header("content-type",kind).header("cache-control","no-store")
        .header("x-content-type-options","nosniff").header("referrer-policy","no-referrer")
        .header("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
        .body(body.into()).unwrap()
}
fn error(status: StatusCode, message: &str) -> Response<Body> {
    response(
        status,
        "application/json",
        json!({"error":message}).to_string(),
    )
}
async fn handle(
    mut req: Request<Body>,
    hub: Hub,
    key: Arc<String>,
) -> Result<Response<Body>, Infallible> {
    let route = req.uri().path();
    if req.method() == hyper::Method::GET {
        let asset = match route {
            "/" => Some((
                "text/html; charset=utf-8",
                include_str!("../web/index.html"),
            )),
            "/app.js" => Some((
                "text/javascript; charset=utf-8",
                include_str!("../web/app.js"),
            )),
            "/style.css" => Some(("text/css; charset=utf-8", include_str!("../web/style.css"))),
            _ => None,
        };
        if let Some((kind, body)) = asset {
            return Ok(response(StatusCode::OK, kind, body));
        }
    }
    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        == Some(key.as_str());
    if !authorized {
        return Ok(error(StatusCode::UNAUTHORIZED, "Enter the web access key"));
    }
    if req.method() == hyper::Method::GET && route == "/api/state" {
        return Ok(response(
            StatusCode::OK,
            "application/json",
            hub.lock().unwrap().snapshot().to_string(),
        ));
    }
    if req.method() != hyper::Method::POST {
        return Ok(error(StatusCode::NOT_FOUND, "Not found"));
    }
    if route == "/api/contacts/refresh" {
        let mut s = hub.lock().unwrap();
        s.refresh = true;
        s.refreshing = true;
        return Ok(response(StatusCode::ACCEPTED, "application/json", "{}"));
    }
    if route != "/api/send" {
        return Ok(error(StatusCode::NOT_FOUND, "Not found"));
    }
    if !req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.split(';').next() == Some("application/json"))
    {
        return Ok(error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Expected JSON"));
    }
    let read = async {
        let mut bytes = Vec::new();
        while let Some(chunk) = req.body_mut().data().await {
            let chunk = chunk.map_err(|_| ())?;
            if bytes.len() + chunk.len() > 2048 {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice::<SendRequest>(&bytes).map_err(|_| ())
    };
    let r = match tokio::time::timeout(Duration::from_secs(5), read).await {
        Ok(Ok(r)) => r,
        _ => {
            return Ok(error(
                StatusCode::BAD_REQUEST,
                "Invalid, oversized, or timed-out request",
            ))
        }
    };
    let result = hub.lock().unwrap().enqueue(r);
    Ok(match result {
        Ok(id) => response(
            StatusCode::ACCEPTED,
            "application/json",
            json!({"id":id}).to_string(),
        ),
        Err((code, msg)) => error(code, msg),
    })
}

pub fn start(address: SocketAddr, channel: u8) -> io::Result<Hub> {
    let key = std::env::var("MESHCORE_WEB_TOKEN").map_err(|_| {
        io::Error::other(
            "Set MESHCORE_WEB_TOKEN to a separate web access key (at least 24 characters)",
        )
    })?;
    if key.len() < 24 || key.len() > 256 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(io::Error::other(
            "MESHCORE_WEB_TOKEN must contain 24–256 printable ASCII characters",
        ));
    }
    let listener = TcpListener::bind(address)?;
    listener.set_nonblocking(true)?;
    let hub = Arc::new(Mutex::new(State::new(channel)));
    let shared = Arc::clone(&hub);
    let key = Arc::new(key);
    thread::Builder::new()
        .name("meshcore-web".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let service = make_service_fn(move |_| {
                    let hub = Arc::clone(&shared);
                    let key = Arc::clone(&key);
                    async move {
                        Ok::<_, Infallible>(service_fn(move |r| {
                            handle(r, Arc::clone(&hub), Arc::clone(&key))
                        }))
                    }
                });
                if let Err(e) = Server::from_tcp(listener)
                    .unwrap()
                    .http1_header_read_timeout(Duration::from_secs(5))
                    .serve(service)
                    .await
                {
                    log::error!("Web server stopped: {e}");
                }
            });
        })?;
    log::info!("Web interface listening on http://{address}");
    Ok(hub)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> Hub {
        let mut s = State::new(0);
        s.connected = true;
        s.name = "Schappet".into();
        let key = "ab".repeat(32);
        s.contacts.insert(
            key.clone(),
            Contact {
                key,
                prefix: "ab".repeat(6),
                name: "ReVillage".into(),
            },
        );
        Arc::new(Mutex::new(s))
    }
    #[test]
    fn browser_send_is_validated_idempotent_and_bounded() {
        let hub = ready();
        let mut s = hub.lock().unwrap();
        let request = || SendRequest {
            request_id: "request-12345678".into(),
            target: "abababababab".into(),
            text: "Hello".into(),
        };
        let id = s.enqueue(request()).unwrap();
        assert_eq!(s.enqueue(request()).unwrap(), id);
        assert_eq!(s.outbox.len(), 1);
        let mut r = request();
        r.text = "Different".into();
        assert_eq!(s.enqueue(r).unwrap_err().0, StatusCode::CONFLICT);
        let mut r = request();
        r.request_id = "another-request".into();
        r.target = "cd".repeat(6);
        assert!(s.enqueue(r).is_err());
        let mut r = request();
        r.request_id = "another-request".into();
        r.text = "é".repeat(67);
        assert!(s.enqueue(r).is_err());
        let second = format!("{}{}", "ab".repeat(6), "cd".repeat(26));
        s.contacts.insert(
            second.clone(),
            Contact {
                key: second,
                prefix: "ab".repeat(6),
                name: "Collision".into(),
            },
        );
        let mut r = request();
        r.request_id = "another-request".into();
        assert!(s.enqueue(r).is_err());
        for i in 1..32 {
            s.enqueue(SendRequest {
                request_id: format!("public-request-{i}"),
                target: "public".into(),
                text: "hello".into(),
            })
            .unwrap();
        }
        assert_eq!(
            s.enqueue(SendRequest {
                request_id: "queue-full-request".into(),
                target: "public".into(),
                text: "hello".into()
            })
            .unwrap_err()
            .0,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    #[test]
    fn observation_filters_other_channels_and_tracks_delivery_ack() {
        let hub = ready();
        let mut s = hub.lock().unwrap();
        let mut p = vec![8, 1, 0, 0, 1, 0, 0, 0];
        p.extend_from_slice(b"ReVillage: hello");
        s.observe(&p);
        assert!(s.messages.is_empty());
        p[1] = 0;
        s.observe(&p);
        assert_eq!(s.messages[0].sender, "ReVillage");
        assert_eq!(s.messages[0].text, "hello");
        let id = s
            .enqueue(SendRequest {
                request_id: "send-with-ack".into(),
                target: "ab".repeat(6),
                text: "Reply".into(),
            })
            .unwrap();
        s.sent(id, &[6, 1, 1, 2, 3, 4, 0x10, 0x27, 0, 0]);
        assert_eq!(s.messages.back().unwrap().status, "awaiting_ack");
        s.observe(&[0x82, 1, 2, 3, 4]);
        assert_eq!(s.messages.back().unwrap().status, "delivered");
        let id = s
            .enqueue(SendRequest {
                request_id: "send-public-123".into(),
                target: "public".into(),
                text: "Hi".into(),
            })
            .unwrap();
        s.sent(id, &[0]);
        assert_eq!(s.messages.back().unwrap().status, "submitted");
    }
    #[test]
    fn outbound_wire_format_and_utf8_limits() {
        assert_eq!(
            chat_command("public", 0, "Schappet", "Hi", 1).unwrap(),
            [3, 0, 0, 1, 0, 0, 0, b'H', b'i']
        );
        assert_eq!(
            chat_command("010203040506", 0, "Schappet", "Hi", 1).unwrap(),
            [2, 0, 0, 1, 0, 0, 0, 1, 2, 3, 4, 5, 6, b'H', b'i']
        );
        assert!(chat_command("public", 0, "Schappet", &"a".repeat(124), 1).is_err());
        assert!(chat_command("public", 0, "Schappet", "a\0b", 1).is_err());
        assert!(chat_command("not-a-key", 0, "Schappet", "Hi", 1).is_err());
        let mut contact = vec![0; 148];
        contact[0] = 3;
        contact[1..33].fill(0xab);
        contact[100..109].copy_from_slice(b"ReVillage");
        let decoded = decode(&contact).unwrap();
        assert_eq!(decoded["name"], "ReVillage");
        assert_eq!(decoded["public_key"], "ab".repeat(32));
        assert!(decode(&contact[..131]).is_err());
    }
    #[test]
    fn http_authentication_assets_and_send_route() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let hub = ready();
            let key = Arc::new(String::from("test-web-key-123456789012345"));
            let req = Request::builder()
                .uri("/api/state")
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                handle(req, hub.clone(), key.clone())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            let req = Request::builder().uri("/").body(Body::empty()).unwrap();
            let r = handle(req, hub.clone(), key.clone()).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            assert!(r.headers().contains_key("content-security-policy"));
            let request = || {
                Request::builder()
                    .method("POST")
                    .uri("/api/send")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"request_id":"browser-request-1","target":"public","text":"hello"}"#,
                    ))
                    .unwrap()
            };
            assert_eq!(
                handle(request(), hub.clone(), key.clone())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::ACCEPTED
            );
            assert_eq!(
                handle(request(), hub.clone(), key.clone())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::ACCEPTED
            );
            assert_eq!(hub.lock().unwrap().outbox.len(), 1);
            let req = Request::builder()
                .method("POST")
                .uri("/api/send")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(2049)))
                .unwrap();
            assert_eq!(
                handle(req, hub.clone(), key.clone())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        });
    }
    #[test]
    #[ignore = "Interactive browser preview using synthetic messages; no radio or API"]
    fn browser_preview() {
        let hub = start("127.0.0.1:8091".parse().unwrap(), 0).unwrap();
        {
            let mut s = hub.lock().unwrap();
            s.name = "Demo — Schappet".into();
            s.connected = true;
            let key = "ab".repeat(32);
            s.contacts.insert(
                key.clone(),
                Contact {
                    key,
                    prefix: "ab".repeat(6),
                    name: "ReVillage".into(),
                },
            );
            let mut p = vec![8, 0, 0, 0];
            p.extend_from_slice(&(unix() as u32).to_le_bytes());
            p.extend_from_slice(b"ReVillage: Hello All. Anyone listening near the river?");
            s.observe(&p);
            let mut p = vec![7];
            p.extend_from_slice(&[0xab; 6]);
            p.extend_from_slice(&[0, 0]);
            p.extend_from_slice(&(unix() as u32).to_le_bytes());
            p.extend_from_slice(b"Hello from ReVillage. How is the signal looking?");
            s.observe(&p);
        }
        loop {
            thread::sleep(Duration::from_millis(250));
            let mut s = hub.lock().unwrap();
            if let Some(p) = s.outbox.pop_front() {
                s.sent(p.id, &[0]);
            }
            s.refresh = false;
            s.refreshing = false;
        }
    }
}
