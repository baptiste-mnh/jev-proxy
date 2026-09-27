use serde_json::Value;

pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn iso_ago(seconds: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(seconds))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

pub fn now_epoch() -> f64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    d.as_secs_f64()
}

/// A number, or a numeric string, as Python's `float()` would read it.
pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn parse_object(body: Option<&str>) -> serde_json::Map<String, Value> {
    match serde_json::from_str::<Value>(body.unwrap_or("")) {
        Ok(Value::Object(m)) => m,
        _ => Default::default(),
    }
}
