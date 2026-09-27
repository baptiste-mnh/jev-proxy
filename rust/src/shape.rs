//! What a recorded request asks, short enough for one row of the stream.
use indexmap::IndexMap;
use serde_json::{json, Value};

const SNIPPET_CHARS: usize = 140;

pub fn question_ids(request_body: &str) -> Vec<String> {
    match serde_json::from_str::<Value>(request_body) {
        Ok(v) => match v.get("questions") {
            Some(Value::Object(q)) => q.keys().cloned().collect(),
            _ => vec![],
        },
        Err(_) => vec![],
    }
}

fn record_text(value: &serde_json::Map<String, Value>) -> String {
    if value.len() == 1 && value.contains_key("text") {
        return value["text"].as_str().unwrap_or("").to_string();
    }
    value
        .iter()
        .map(|(f, t)| format!("{f}:\n{}", t.as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The texts a question can target, keyed like the question ids. An object of strings is
/// either a container (`sections`, `comments`) whose keys are the subjects, or one item with
/// fields, which it is when a question id starts with its key.
pub fn state_texts(state: &Value, qids: &[String]) -> IndexMap<String, String> {
    let mut texts = IndexMap::new();
    let Value::Object(map) = state else { return texts };
    for (key, value) in map {
        match value {
            Value::Object(o) if !o.is_empty() && o.values().all(Value::is_string) => {
                if qids.iter().any(|q| q.starts_with(&format!("{key}_"))) {
                    texts.entry(key.clone()).or_insert_with(|| record_text(o));
                } else {
                    for (sub, text) in o {
                        texts.entry(sub.clone()).or_insert_with(|| text.as_str().unwrap().to_string());
                    }
                }
            }
            Value::String(s) => {
                texts.entry(key.clone()).or_insert_with(|| s.clone());
            }
            _ => {}
        }
    }
    texts
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(SNIPPET_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// (subject, label) of a question id: on the longest state key it starts with, else on its last `__`.
pub fn split_question_id(qid: &str, keys: &[String]) -> (String, String) {
    if let Some(key) = keys.iter().find(|k| qid.starts_with(&format!("{k}_"))) {
        return (key.clone(), qid[key.len()..].trim_start_matches('_').to_string());
    }
    if let Some(i) = qid.rfind("__") {
        return (qid[..i].to_string(), qid[i + 2..].to_string());
    }
    (String::new(), qid.to_string())
}

/// Keys longest first. Stable, so equal lengths keep their order.
pub fn longest_first<'a>(keys: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut v: Vec<String> = keys.cloned().collect();
    v.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
    v
}

pub fn call_shape(request_body: &str) -> Value {
    let empty = json!({"subjects": 0, "subject": null, "firstSubject": null, "labels": [], "snippet": ""});
    let Ok(Value::Object(body)) = serde_json::from_str::<Value>(request_body) else { return empty };
    let state = body.get("state").cloned().unwrap_or(Value::Null);
    let qids: Vec<String> = match body.get("questions") {
        Some(Value::Object(q)) => q.keys().cloned().collect(),
        _ => vec![],
    };
    let texts = state_texts(&state, &qids);
    let keys = longest_first(texts.keys());
    let (mut subjects, mut labels): (Vec<String>, Vec<String>) = (vec![], vec![]);
    for qid in &qids {
        let (subject, label) = split_question_id(qid, &keys);
        if !subjects.contains(&subject) {
            subjects.push(subject);
        }
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let first_subject = subjects.iter().find(|s| texts.contains_key(*s)).cloned();
    let snip = match &first_subject {
        Some(s) => snippet(&texts[s]),
        None => snippet(&match &state {
            Value::String(s) => s.clone(),
            _ => texts.values().next().cloned().unwrap_or_default(),
        }),
    };
    json!({
        "subjects": subjects.len(),
        "subject": if subjects.len() == 1 && !subjects[0].is_empty() { json!(subjects[0]) } else { Value::Null },
        "firstSubject": first_subject,
        "labels": labels,
        "snippet": snip,
    })
}
