//! Recovers the Requests-form shape (items plus shared questions) from a recorded body.
use serde_json::{json, Map, Value};

pub fn compose(items: &[Value], questions: &[Value], model: &Value) -> Value {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut state = Map::new();
    let mut composed = Map::new();
    for item in items {
        state.insert(s(item, "key"), json!(s(item, "text")));
    }
    for item in items {
        let key = s(item, "key");
        for q in questions {
            composed.insert(
                format!("{key}_{}", s(q, "suffix")),
                json!({
                    "type": "noul",
                    "instructions": s(q, "instructions").replace("{ref}", &format!("`{key}`")),
                    "criteria": {"true": s(q, "criteria_true"), "false": s(q, "criteria_false")},
                }),
            );
        }
    }
    json!({"state": state, "model": model, "questions": composed})
}

/// (items, questions), or None when the form cannot rebuild `body` exactly.
pub fn decompose(body: &Value) -> Option<(Vec<Value>, Vec<Value>)> {
    let obj = body.as_object()?;
    let state = obj.get("state")?.as_object().filter(|s| !s.is_empty())?;
    let questions = obj.get("questions")?.as_object()?;
    if !state.values().all(Value::is_string) {
        return None;
    }
    let mut keys: Vec<&String> = state.keys().collect();
    keys.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));

    let mut shared: Vec<Value> = vec![];
    for (qid, q) in questions {
        let key = keys.iter().find(|k| qid.starts_with(&format!("{k}_")))?;
        let q = q.as_object().filter(|q| q.get("type").and_then(Value::as_str) == Some("noul"))?;
        let criteria = q.get("criteria").and_then(Value::as_object);
        let crit = |k: &str| criteria.and_then(|c| c.get(k)).cloned().unwrap_or(json!(""));
        let instructions = match q.get("instructions") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        let shape = json!({
            "suffix": &qid[key.len() + 1..],
            "instructions": instructions.replace(&format!("`{key}`"), "{ref}"),
            "criteria_true": crit("true"),
            "criteria_false": crit("false"),
        });
        match shared.iter().find(|s| s["suffix"] == shape["suffix"]) {
            Some(existing) if *existing != shape => return None,
            Some(_) => {}
            None => shared.push(shape),
        }
    }

    let items: Vec<Value> = state.iter().map(|(k, v)| json!({"key": k, "text": v})).collect();
    let model = obj.get("model").cloned().unwrap_or(json!("jev-latest"));
    let mut expected = body.as_object().unwrap().clone();
    expected.insert("model".into(), model.clone());
    if compose(&items, &shared, &model) != Value::Object(expected) {
        return None;
    }
    Some((items, shared))
}
