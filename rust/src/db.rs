//! SQLite access: saved requests, recorded calls, shadows, verdicts, settings, response cache.
use crate::util::now_epoch;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub const PROVIDERS: [&str; 2] = ["typesafe", "second"];

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS requests (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        model TEXT NOT NULL DEFAULT 'jev-latest',
        items_json TEXT NOT NULL DEFAULT '[]',
        questions_json TEXT NOT NULL DEFAULT '[]',
        raw_json_override TEXT,
        last_response TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS calls (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts TEXT NOT NULL,
        caller TEXT NOT NULL,
        path TEXT NOT NULL,
        model TEXT,
        request_body TEXT NOT NULL,
        response_body TEXT,
        status INTEGER NOT NULL,
        elapsed_ms INTEGER,
        cached INTEGER NOT NULL DEFAULT 0,
        body_hash TEXT NOT NULL,
        input_tokens INTEGER,
        output_tokens INTEGER,
        attempts INTEGER NOT NULL DEFAULT 1,
        error TEXT,
        upstream TEXT NOT NULL DEFAULT 'typesafe'
    )",
    "CREATE INDEX IF NOT EXISTS idx_calls_ts ON calls(ts)",
    "CREATE INDEX IF NOT EXISTS idx_calls_caller ON calls(caller)",
    "CREATE INDEX IF NOT EXISTS idx_calls_body_hash ON calls(body_hash)",
    "CREATE TABLE IF NOT EXISTS shadows (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        call_id INTEGER NOT NULL,
        provider TEXT NOT NULL,
        state TEXT NOT NULL,
        ts TEXT NOT NULL,
        status INTEGER,
        response_body TEXT,
        elapsed_ms INTEGER,
        cached INTEGER NOT NULL DEFAULT 0,
        input_tokens INTEGER,
        output_tokens INTEGER,
        attempts INTEGER,
        error TEXT,
        UNIQUE(call_id, provider)
    )",
    "CREATE TABLE IF NOT EXISTS verdicts (
        call_id INTEGER NOT NULL,
        question_id TEXT NOT NULL,
        verdict TEXT NOT NULL,
        ts TEXT NOT NULL,
        PRIMARY KEY (call_id, question_id)
    )",
    "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS cache (
        body_hash TEXT PRIMARY KEY,
        response_body TEXT NOT NULL,
        status INTEGER NOT NULL,
        created_at TEXT NOT NULL
    )",
];

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    // Shadow threads write while the request thread writes, so a lock waits instead of failing.
    conn.busy_timeout(Duration::from_secs(10))?;
    for statement in SCHEMA {
        conn.execute(statement, [])?;
    }
    let has_upstream: bool = conn
        .prepare("PRAGMA table_info(calls)")?
        .query_map([], |r| r.get::<_, String>("name"))?
        .any(|n| n.map(|n| n == "upstream").unwrap_or(false));
    if !has_upstream {
        conn.execute(
            "ALTER TABLE calls ADD COLUMN upstream TEXT NOT NULL DEFAULT 'typesafe'",
            [],
        )?;
    }
    Ok(conn)
}

// ---- settings ----

pub struct Settings {
    pub engines: Vec<String>,
    pub served: String,
}

impl Settings {
    pub fn to_json(&self) -> Value {
        json!({"engines": self.engines, "served": self.served})
    }
}

fn check_settings(engines: &[String], served: &str) -> Result<(), String> {
    if engines.is_empty() {
        return Err("enable at least one engine".into());
    }
    let unknown: Vec<&str> = engines
        .iter()
        .map(|e| e.as_str())
        .filter(|e| !PROVIDERS.contains(e))
        .collect();
    if !unknown.is_empty() {
        return Err(format!("unknown engine: {}", unknown.join(", ")));
    }
    if !engines.iter().any(|e| e == served) {
        return Err(format!("the served engine '{served}' is not enabled"));
    }
    Ok(())
}

pub fn get_settings(conn: &Connection) -> rusqlite::Result<Settings> {
    let default = || Settings { engines: vec!["typesafe".into()], served: "typesafe".into() };
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows: std::collections::HashMap<String, String> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let (Some(engines), Some(served)) = (rows.get("engines"), rows.get("served")) else {
        return Ok(default());
    };
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(engines) else {
        return Ok(default());
    };
    let engines: Vec<String> = list.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    if engines.len() != list.len() || check_settings(&engines, served).is_err() {
        return Ok(default());
    }
    Ok(Settings { engines, served: served.clone() })
}

/// Err(message) when `engines` is empty, holds an unknown engine, or omits `served`.
pub fn save_settings(conn: &Connection, engines: &Value, served: &Value) -> Result<Settings, String> {
    let list: Vec<String> = engines
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect())
        .unwrap_or_default();
    let served = served.as_str().unwrap_or("").to_string();
    check_settings(&list, &served)?;
    let ordered: Vec<String> = PROVIDERS.iter().filter(|p| list.iter().any(|e| e == *p)).map(|p| p.to_string()).collect();
    let mut stmt = conn
        .prepare("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .map_err(|e| e.to_string())?;
    stmt.execute(params!["engines", json!(ordered).to_string()]).map_err(|e| e.to_string())?;
    stmt.execute(params!["served", served]).map_err(|e| e.to_string())?;
    Ok(Settings { engines: ordered, served })
}

// ---- cache ----

pub fn cache_get(conn: &Connection, hash: &str, ttl: i64) -> rusqlite::Result<Option<(String, i64)>> {
    let row: Option<(String, i64, String)> = conn
        .query_row(
            "SELECT response_body, status, created_at FROM cache WHERE body_hash = ?",
            [hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((body, status, created)) = row else { return Ok(None) };
    if now_epoch() - created.parse::<f64>().unwrap_or(0.0) > ttl as f64 {
        conn.execute("DELETE FROM cache WHERE body_hash = ?", [hash])?;
        return Ok(None);
    }
    Ok(Some((body, status)))
}

/// Only a 2xx response is stored: a 429 or a 529 is retryable and a 422 depends on the body.
pub fn cache_put(conn: &Connection, hash: &str, body: &str, status: i64) -> rusqlite::Result<bool> {
    if !(200..300).contains(&status) {
        return Ok(false);
    }
    conn.execute(
        "INSERT OR REPLACE INTO cache (body_hash, response_body, status, created_at) VALUES (?, ?, ?, ?)",
        params![hash, body, status, now_epoch().to_string()],
    )?;
    Ok(true)
}

// ---- requests ----

pub fn request_to_json(row: &Row) -> rusqlite::Result<Value> {
    let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
    let last: Option<String> = row.get("last_response")?;
    Ok(json!({
        "id": row.get::<_, i64>("id")?,
        "name": row.get::<_, String>("name")?,
        "model": row.get::<_, String>("model")?,
        "items": parse(row.get("items_json")?),
        "questions": parse(row.get("questions_json")?),
        "rawJsonOverride": row.get::<_, Option<String>>("raw_json_override")?,
        "lastResponse": last.filter(|s| !s.is_empty()).map(parse),
        "createdAt": row.get::<_, String>("created_at")?,
        "updatedAt": row.get::<_, String>("updated_at")?,
    }))
}

pub fn request_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    conn.query_row("SELECT * FROM requests WHERE id = ?", [id], request_to_json).optional()
}

// ---- calls ----

/// A recorded call. Columns a query did not select read as None.
pub struct CallRow {
    pub id: i64,
    pub ts: String,
    pub caller: String,
    pub path: String,
    pub model: Option<String>,
    pub request_body: String,
    pub response_body: Option<String>,
    pub status: i64,
    pub elapsed_ms: Option<i64>,
    pub cached: bool,
    pub attempts: i64,
    pub upstream: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub error: Option<String>,
}

impl CallRow {
    pub fn from_row(r: &Row) -> rusqlite::Result<CallRow> {
        Ok(CallRow {
            id: r.get("id")?,
            ts: r.get("ts")?,
            caller: r.get("caller")?,
            path: r.get("path")?,
            model: r.get("model")?,
            request_body: r.get("request_body")?,
            response_body: r.get("response_body").unwrap_or(None),
            status: r.get("status")?,
            elapsed_ms: r.get("elapsed_ms")?,
            cached: r.get::<_, i64>("cached")? != 0,
            attempts: r.get("attempts")?,
            upstream: r.get("upstream")?,
            input_tokens: r.get("input_tokens")?,
            output_tokens: r.get("output_tokens")?,
            error: r.get("error").unwrap_or(None),
        })
    }
}

pub fn call_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<CallRow>> {
    conn.query_row("SELECT * FROM calls WHERE id = ?", [id], CallRow::from_row).optional()
}

pub struct ShadowRow {
    pub provider: String,
    pub state: String,
    pub ts: String,
    pub status: Option<i64>,
    pub response_body: Option<String>,
    pub elapsed_ms: Option<i64>,
    pub cached: bool,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub attempts: Option<i64>,
    pub error: Option<String>,
}

pub fn shadows_by_call(conn: &Connection, ids: &[i64]) -> rusqlite::Result<std::collections::HashMap<i64, Vec<ShadowRow>>> {
    let mut grouped: std::collections::HashMap<i64, Vec<ShadowRow>> = ids.iter().map(|i| (*i, vec![])).collect();
    if ids.is_empty() {
        return Ok(grouped);
    }
    let marks = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!("SELECT * FROM shadows WHERE call_id IN ({marks}) ORDER BY id"))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        Ok((
            r.get::<_, i64>("call_id")?,
            ShadowRow {
                provider: r.get("provider")?,
                state: r.get("state")?,
                ts: r.get("ts")?,
                status: r.get("status")?,
                response_body: r.get("response_body")?,
                elapsed_ms: r.get("elapsed_ms")?,
                cached: r.get::<_, i64>("cached")? != 0,
                input_tokens: r.get("input_tokens")?,
                output_tokens: r.get("output_tokens")?,
                attempts: r.get("attempts")?,
                error: r.get("error")?,
            },
        ))
    })?;
    for row in rows {
        let (call_id, shadow) = row?;
        grouped.entry(call_id).or_default().push(shadow);
    }
    Ok(grouped)
}

pub fn call_summary(row: &CallRow, shadows: &[ShadowRow]) -> Value {
    json!({
        "id": row.id, "ts": row.ts, "caller": row.caller, "path": row.path,
        "model": row.model, "status": row.status, "elapsedMs": row.elapsed_ms,
        "cached": row.cached, "attempts": row.attempts, "upstream": row.upstream,
        "inputTokens": row.input_tokens, "outputTokens": row.output_tokens,
        "questionIds": crate::shape::question_ids(&row.request_body),
        "shape": crate::shape::call_shape(&row.request_body),
        "shadows": shadows.iter().map(|s| json!({
            "provider": s.provider, "state": s.state, "status": s.status, "cached": s.cached,
        })).collect::<Vec<_>>(),
    })
}

pub fn call_detail(row: &CallRow, shadows: &[ShadowRow]) -> Value {
    let mut v = call_summary(row, &[]);
    let o = v.as_object_mut().unwrap();
    o.insert("requestBody".into(), json!(row.request_body));
    o.insert("responseBody".into(), json!(row.response_body));
    o.insert("error".into(), json!(row.error));
    o.insert(
        "shadows".into(),
        shadows.iter().map(|s| json!({
            "provider": s.provider, "state": s.state, "ts": s.ts, "status": s.status,
            "cached": s.cached, "elapsedMs": s.elapsed_ms, "attempts": s.attempts,
            "inputTokens": s.input_tokens, "outputTokens": s.output_tokens,
            "responseBody": s.response_body, "error": s.error,
        })).collect::<Vec<_>>().into(),
    );
    v
}

