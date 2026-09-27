//! Proxy core: request identity, upstream calls with retry, call recording, shadows.
use crate::db::{self, cache_get, cache_put};
use crate::util::now_iso;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub cache_ttl: i64,
    pub second_base_url: String,
    pub second_api_key: String,
    pub second_model: String,
    pub second_label: String,
    pub db_path: PathBuf,
    /// A local second upstream may run one inference at a time, so a burst must not build an unbounded queue.
    pub max_pending_shadows: i64,
}

impl Config {
    pub fn open_db(&self) -> rusqlite::Result<Connection> {
        db::open(&self.db_path)
    }
}

/// JSON with sorted object keys, compact, non-ASCII kept: the same text as Python's
/// `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap()),
    }
}

/// Identity of a request, stable across key order and whitespace. Array order is kept.
pub fn canonical_hash(path: &str, body: &Value) -> String {
    let mut text = format!("{path}\n");
    canonical(body, &mut text);
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

const RETRYABLE: [i64; 2] = [429, 529];
const MAX_RETRIES: u32 = 2;
const BACKOFF: [u64; 2] = [1, 2];

pub struct UpstreamResult {
    pub status: i64, // 0 when the request never reached the API
    pub body: String,
    pub attempts: i64,
    pub error: Option<String>,
}

fn client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(timeout).build().unwrap()
}

/// POST to an upstream, retrying only 429 and 529. A `Retry-After` header overrides the
/// backoff. The last status is returned when retries run out: the proxy never invents a success.
pub fn upstream_call(base_url: &str, path: &str, body: &[u8], api_key: &str, sleep: impl Fn(u64)) -> UpstreamResult {
    let http = client(Duration::from_secs(60));
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let mut req = http.post(format!("{base_url}{path}")).header("Content-Type", "application/json").body(body.to_vec());
        if !api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }
        let resp = match req.send() {
            Ok(r) => r,
            Err(e) => {
                return UpstreamResult { status: 0, body: String::new(), attempts: attempts as i64, error: Some(e.to_string()) }
            }
        };
        let status = resp.status().as_u16() as i64;
        let retry_after = resp.headers().get("Retry-After").and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
        let text = resp.text().unwrap_or_default();
        if !RETRYABLE.contains(&status) || attempts > MAX_RETRIES {
            return UpstreamResult { status, body: text, attempts: attempts as i64, error: None };
        }
        sleep(retry_after.unwrap_or(BACKOFF[attempts as usize - 1]));
    }
}

pub struct Upstream<'a> {
    
    pub base_url: &'a str,
    pub api_key: &'a str,
}

pub fn upstream_for<'a>(provider: &str, c: &'a Config) -> Upstream<'a> {
    if provider == "second" {
        Upstream { base_url: &c.second_base_url, api_key: &c.second_api_key }
    } else {
        Upstream { base_url: &c.base_url, api_key: &c.api_key }
    }
}

pub struct Resolved {
    pub status: i64,
    pub body: String,
    pub elapsed_ms: i64,
    pub cached: bool,
    pub attempts: i64,
    pub error: Option<String>,
    pub body_hash: String,
    pub model: Value, // the caller's model, before the rewrite
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

fn usage(body: &str) -> (Option<i64>, Option<i64>) {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let u = &v["usage"];
    (u["input_tokens"].as_i64(), u["output_tokens"].as_i64())
}

/// Send a body to one engine through the cache, with `model` rewritten for that engine.
/// When the rewrite changes nothing, the exact bytes received are sent.
pub fn resolve(conn: &Connection, provider: &str, path: &str, body_bytes: &[u8], config: &Config, bypass_cache: bool) -> rusqlite::Result<Resolved> {
    let parsed: Option<Value> = serde_json::from_slice(body_bytes).ok();
    let mut model = Value::Null;
    let mut sent: Vec<u8> = body_bytes.to_vec();
    let body_hash;
    match &parsed {
        None => {
            // No canonical form: key it on its exact text, not one shared entry for all bad bodies.
            let text = String::from_utf8_lossy(body_bytes).to_string();
            body_hash = canonical_hash(&format!("{path}\0raw\0{provider}"), &json!(text));
        }
        Some(p) => {
            model = p.get("model").cloned().unwrap_or(Value::Null);
            let mut effective = p.clone();
            if let Value::Object(m) = &mut effective {
                if provider == "second" && !config.second_model.is_empty() && model != json!(config.second_model) {
                    m.insert("model".into(), json!(config.second_model));
                    sent = serde_json::to_vec(&effective).unwrap();
                }
            }
            let keyed = if provider == "typesafe" { path.to_string() } else { format!("{path}\0{provider}") };
            body_hash = canonical_hash(&keyed, &effective);
        }
    }
    let upstream = upstream_for(provider, config);
    let started = Instant::now();
    let hit = if bypass_cache { None } else { cache_get(conn, &body_hash, config.cache_ttl)? };
    let (status, body, attempts, error, cached) = match hit {
        Some((body, status)) => (status, body, 0, None, true),
        None => {
            let r = upstream_call(upstream.base_url, path, &sent, upstream.api_key, |s| std::thread::sleep(Duration::from_secs(s)));
            cache_put(conn, &body_hash, &r.body, r.status)?;
            (r.status, r.body, r.attempts, r.error, false)
        }
    };
    let (input_tokens, output_tokens) = usage(&body);
    Ok(Resolved {
        status, elapsed_ms: started.elapsed().as_millis() as i64, cached, attempts: attempts.max(1),
        error, body_hash, model, input_tokens, output_tokens, body,
    })
}

pub struct CallResult {
    pub status: i64,
    pub body: String,
    pub elapsed_ms: i64,
    pub cached: bool,
    pub attempts: i64,
    pub call_id: i64,
    pub upstream: String,
}

/// Resolve a call on the served engine and record it either way. A cache hit is recorded too,
/// flagged `cached`, so call frequency stays visible.
pub fn execute_call(conn: &Connection, path: &str, body_bytes: &[u8], caller: &str, config: &Config, provider: &str, bypass_cache: bool) -> rusqlite::Result<CallResult> {
    let r = resolve(conn, provider, path, body_bytes, config, bypass_cache)?;
    conn.execute(
        "INSERT INTO calls (ts, caller, path, model, request_body, response_body, status, elapsed_ms, cached, \
         body_hash, input_tokens, output_tokens, attempts, error, upstream) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            now_iso(), caller, path, r.model.as_str(), String::from_utf8_lossy(body_bytes), r.body, r.status,
            r.elapsed_ms, r.cached as i64, r.body_hash, r.input_tokens, r.output_tokens, r.attempts, r.error, provider
        ],
    )?;
    Ok(CallResult {
        status: r.status, body: r.body, elapsed_ms: r.elapsed_ms, cached: r.cached, attempts: r.attempts,
        call_id: conn.last_insert_rowid(), upstream: provider.to_string(),
    })
}

/// Insert or replace the shadow row of (`call_id`, `provider`).
pub fn write_shadow(conn: &Connection, call_id: i64, provider: &str, state: &str, resolved: Option<&Resolved>, error: Option<&str>) -> rusqlite::Result<()> {
    let r = resolved;
    conn.execute(
        "INSERT INTO shadows (call_id, provider, state, ts, status, response_body, elapsed_ms, cached, \
         input_tokens, output_tokens, attempts, error) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(call_id, provider) DO UPDATE SET \
         state = excluded.state, ts = excluded.ts, status = excluded.status, \
         response_body = excluded.response_body, elapsed_ms = excluded.elapsed_ms, \
         cached = excluded.cached, input_tokens = excluded.input_tokens, \
         output_tokens = excluded.output_tokens, attempts = excluded.attempts, error = excluded.error",
        params![
            call_id, provider, state, now_iso(), r.map(|r| r.status), r.map(|r| r.body.as_str()),
            r.map(|r| r.elapsed_ms), r.map_or(0, |r| r.cached as i64), r.and_then(|r| r.input_tokens),
            r.and_then(|r| r.output_tokens), r.map(|r| r.attempts),
            r.and_then(|r| r.error.as_deref()).or(error),
        ],
    )?;
    Ok(())
}

static SHADOW_LOCK: Mutex<()> = Mutex::new(());

/// Record a `pending` shadow per engine and run each on its own thread and connection. An engine
/// that cannot run gets a `skipped` row with the reason.
pub fn start_shadows(config: &Arc<Config>, call_id: i64, providers: &[String], path: &str, body: &[u8], bypass_cache: bool) -> Vec<std::thread::JoinHandle<()>> {
    let mut threads = vec![];
    let Ok(conn) = config.open_db() else { return threads };
    for provider in providers {
        let reason = {
            let _guard = SHADOW_LOCK.lock().unwrap();
            let pending: i64 = conn
                .query_row("SELECT COUNT(*) FROM shadows WHERE provider = ? AND state = 'pending'", [provider], |r| r.get(0))
                .unwrap_or(0);
            let reason = if provider == "typesafe" && config.api_key.is_empty() {
                Some("TYPESAFE_API_KEY not set".to_string())
            } else if provider == "second" && config.second_base_url.is_empty() {
                Some("SECOND_BASE_URL not set".to_string())
            } else if pending >= config.max_pending_shadows {
                Some(format!("{pending} {provider} shadows already pending"))
            } else {
                None
            };
            let _ = write_shadow(&conn, call_id, provider, if reason.is_some() { "skipped" } else { "pending" }, None, reason.as_deref());
            reason
        };
        if reason.is_some() {
            continue;
        }
        let (config, provider, path, body) = (config.clone(), provider.clone(), path.to_string(), body.to_vec());
        threads.push(std::thread::spawn(move || {
            let Ok(conn) = config.open_db() else { return };
            match resolve(&conn, &provider, &path, &body, &config, bypass_cache) {
                Ok(r) => { let _ = write_shadow(&conn, call_id, &provider, "done", Some(&r), None); }
                Err(e) => { let _ = write_shadow(&conn, call_id, &provider, "done", None, Some(&e.to_string())); }
            }
        }));
    }
    threads
}

/// What the second upstream reports on `/health`, or why it did not answer.
pub fn second_health(config: &Config) -> Value {
    if config.second_base_url.is_empty() {
        return json!({"up": false, "error": "SECOND_BASE_URL not set"});
    }
    let resp = client(Duration::from_secs(1)).get(format!("{}/health", config.second_base_url)).send();
    match resp.and_then(|r| r.json::<Value>()) {
        Ok(h) => json!({
            "up": h["status"] == "ok", "loaded": if h["loaded"].is_null() { json!([]) } else { h["loaded"].clone() },
            "device": h["device"], "error": null,
        }),
        Err(e) => json!({"up": false, "error": e.to_string()}),
    }
}

/// Delete calls older than `days` and every cache row past that age. Manual only.
pub fn prune(conn: &Connection, days: i64) -> rusqlite::Result<(usize, usize)> {
    let cutoff = crate::util::iso_ago(days * 86400);
    let horizon = crate::util::now_epoch() - (days * 86400) as f64;
    let calls = conn.execute("DELETE FROM calls WHERE ts < ?", [cutoff])?;
    conn.execute("DELETE FROM shadows WHERE call_id NOT IN (SELECT id FROM calls)", [])?;
    conn.execute("DELETE FROM verdicts WHERE call_id NOT IN (SELECT id FROM calls)", [])?;
    let cache = conn.execute("DELETE FROM cache WHERE CAST(created_at AS REAL) < ?", [horizon])?;
    Ok((calls, cache))
}
