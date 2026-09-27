//! Shared test harness: a stub upstream, a temp database, and the real routes called directly.
#![allow(dead_code)]
use jev_proxy::proxy::Config;
use jev_proxy::routes::{handle, App, Reply};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex};
use std::time::{Duration, Instant};

/// (status, body, Retry-After)
pub type Queued = (u16, String, Option<u64>);

#[derive(Default)]
pub struct StubState {
    pub replies: Vec<Queued>,
    pub bodies: Vec<Vec<u8>>,
    pub auth: Vec<Option<String>>,
}

/// Replies with the statuses queued on it, in order. Listens for the life of the process.
#[derive(Clone)]
pub struct Stub {
    pub state: Arc<Mutex<StubState>>,
    pub url: String,
}

impl Stub {
    pub fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(StubState::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                std::thread::spawn(move || serve(stream, shared));
            }
        });
        Stub { state, url }
    }
    pub fn reply(&self, status: u16, body: &str) {
        self.state.lock().unwrap().replies.push((status, body.to_string(), None));
    }
    pub fn reply_retry_after(&self, status: u16, body: &str, seconds: u64) {
        self.state.lock().unwrap().replies.push((status, body.to_string(), Some(seconds)));
    }
    pub fn bodies(&self) -> Vec<Vec<u8>> {
        self.state.lock().unwrap().bodies.clone()
    }
    pub fn auth(&self) -> Vec<Option<String>> {
        self.state.lock().unwrap().auth.clone()
    }
}

fn serve(mut stream: std::net::TcpStream, state: Arc<Mutex<StubState>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let header = |name: &str| {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let len: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < header_end + len {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let is_get = head.starts_with("GET");
    let mut st = state.lock().unwrap();
    let (status, body, retry) = if is_get {
        (200, r#"{"status":"ok","loaded":["m"],"device":"cpu"}"#.to_string(), None)
    } else {
        st.bodies.push(buf[header_end..header_end + len.min(buf.len() - header_end)].to_vec());
        st.auth.push(header("authorization"));
        if st.replies.is_empty() { (500, r#"{"error":"no reply queued"}"#.to_string(), None) } else { st.replies.remove(0) }
    };
    drop(st);
    let mut out = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if let Some(r) = retry {
        out += &format!("Retry-After: {r}\r\n");
    }
    out += "\r\n";
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct Env {
    pub dir: PathBuf,
    pub upstream: Stub,
    pub second: Stub,
    pub app: App,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Env {
    pub fn new() -> Env {
        Env::with(|_| {})
    }

    /// `tweak` may change the config before the app is built.
    pub fn with(tweak: impl FnOnce(&mut Config)) -> Env {
        let dir = std::env::temp_dir().join(format!("jev-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let (upstream, second) = (Stub::start(), Stub::start());
        let mut config = Config {
            base_url: upstream.url.clone(),
            api_key: "k".into(),
            cache_ttl: 3600,
            second_base_url: second.url.clone(),
            second_api_key: String::new(),
            second_model: String::new(),
            second_label: "Second".into(),
            db_path: dir.join("jev.db"),
            max_pending_shadows: 4,
        };
        tweak(&mut config);
        let app = App { config: Arc::new(config), static_dir: Some(dir.clone()) };
        Env { dir, upstream, second, app }
    }

    pub fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &Value) -> Reply {
        let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let body = if body.is_null() { vec![] } else { serde_json::to_vec(body).unwrap() };
        handle(&self.app, method, path, &headers, &body)
    }
    pub fn post(&self, path: &str, body: Value) -> Reply {
        self.request("POST", path, &[], &body)
    }
    pub fn post_h(&self, path: &str, headers: &[(&str, &str)], body: Value) -> Reply {
        self.request("POST", path, headers, &body)
    }
    pub fn get(&self, path: &str) -> (u16, Value) {
        let r = self.request("GET", path, &[], &Value::Null);
        (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }
    pub fn db(&self) -> rusqlite::Connection {
        self.app.config.open_db().unwrap()
    }
    /// Polls until the shadow of (`call_id`, `provider`) reaches `state`, or 5 seconds pass.
    pub fn shadow(&self, call_id: i64, provider: &str, until: Option<&str>) -> Option<(String, Option<i64>, Option<String>, Option<String>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let row = self.db().query_row(
                "SELECT state, status, response_body, error FROM shadows WHERE call_id = ? AND provider = ?",
                rusqlite::params![call_id, provider],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            ).ok();
            let done = match (&row, until) {
                (_, None) => true,
                (Some(r), Some(u)) => r.0 == u,
                _ => false,
            };
            if done || Instant::now() > deadline {
                return row;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

pub fn header(r: &Reply, name: &str) -> String {
    r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default()
}
pub fn body_json(r: &Reply) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
pub fn call_id(r: &Reply) -> i64 {
    header(r, "X-Jev-Call-Id").parse().unwrap()
}

pub const OK: &str = r#"{"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.9}},"usage":{"input_tokens":1,"output_tokens":2}}"#;
pub const JEV_OK: &str = r#"{"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.2}},"usage":{"input_tokens":30,"output_tokens":2}}"#;
pub const SECOND_OK: &str = r#"{"model":"second","answers":{"q":{"type":"noul","noul":0.7}},"usage":{"input_tokens":3,"output_tokens":1}}"#;
