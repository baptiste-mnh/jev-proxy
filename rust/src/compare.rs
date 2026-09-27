//! Shadow Compare: one row per question that both engines answered on the same call.
use crate::shape::{longest_first, split_question_id, state_texts};
use crate::util::{now_iso, num, parse_object};
use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const VERDICTS: [&str; 4] = ["typesafe", "second", "tie", "neither"];

fn model(response: &Map<String, Value>) -> Value {
    let model = response.get("model").and_then(Value::as_str).filter(|m| !m.is_empty());
    let checkpoint = response
        .get("routing")
        .and_then(|r| r.get("model"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty());
    match (model, checkpoint) {
        (Some(m), Some(c)) => json!(format!("{m} · {c}")),
        (m, _) => json!(m),
    }
}

/// How far two answers are apart, 0 to 1. None when they cannot be compared.
pub fn gap(a: &Value, b: &Value) -> Option<f64> {
    let (ao, bo) = (a.as_object()?, b.as_object()?);
    let kind = ao.get("type")?;
    if Some(kind) != bo.get("type") {
        return None;
    }
    match kind.as_str()? {
        "noul" => Some((num(ao.get("noul")?)? - num(bo.get("noul")?)?).abs()),
        "choice" => Some(if ao.get("choice") == bo.get("choice") { 0.0 } else { 1.0 }),
        "score" => {
            let legend = ao.get("legend").and_then(Value::as_object).map_or(0, |l| l.len()).max(2);
            let d = (num(ao.get("score")?)? - num(bo.get("score")?)?).abs() / (legend as f64 - 1.0);
            Some(d.min(1.0))
        }
        _ => None,
    }
}

/// A `noul` on opposite sides of 0.5, or a `choice` that differs.
pub fn disagrees(a: &Value, b: &Value) -> bool {
    if gap(a, b).is_none() {
        return false;
    }
    match a["type"].as_str() {
        Some("noul") => (num(&a["noul"]).unwrap() < 0.5) != (num(&b["noul"]).unwrap() < 0.5),
        Some("choice") => a.get("choice") != b.get("choice"),
        _ => false,
    }
}

fn instructions(question: Option<&Value>) -> String {
    let Some(Value::Object(q)) = question else { return String::new() };
    let mut lines = vec![match q.get("instructions") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }];
    if let Some(Value::Object(c)) = q.get("criteria") {
        for (k, v) in c {
            lines.push(format!("{k}: {}", v.as_str().map(String::from).unwrap_or_else(|| v.to_string())));
        }
    }
    lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

fn subject_text(state: &Value, texts: &IndexMap<String, String>, subject: &str) -> String {
    if let Some(t) = texts.get(subject) {
        return t.clone();
    }
    if subject.is_empty() {
        if let Value::String(s) = state {
            return s.clone();
        }
        if texts.len() == 1 {
            return texts.values().next().unwrap().clone();
        }
        return texts.iter().map(|(k, t)| format!("{k}: {t}")).collect::<Vec<_>>().join("\n\n");
    }
    match state.get(subject) {
        Some(Value::Object(o)) if o.get("text").is_some_and(Value::is_string) => {
            o["text"].as_str().unwrap().to_string()
        }
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
        None => String::new(),
    }
}

pub fn shadow_rows(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<Value>> {
    shadow_rows_of(conn, limit, None)
}

/// Like `shadow_rows`, for one call when `only_call` is set.
pub fn shadow_rows_of(conn: &Connection, limit: i64, only_call: Option<i64>) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.ts, c.caller, c.upstream, c.request_body, c.response_body, \
         s.provider, s.response_body AS shadow_body FROM calls c \
         JOIN shadows s ON s.call_id = c.id \
         WHERE s.state = 'done' AND c.status BETWEEN 200 AND 299 AND s.status BETWEEN 200 AND 299 \
         AND (?2 IS NULL OR c.id = ?2) \
         ORDER BY c.id DESC LIMIT ?1",
    )?;
    type CallJoin = (i64, String, String, String, String, Option<String>, String, Option<String>);
    let calls: Vec<CallJoin> = stmt
        .query_map(rusqlite::params![limit, only_call], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut verdicts: HashMap<(i64, String), String> = HashMap::new();
    let mut vstmt = conn.prepare("SELECT call_id, question_id, verdict FROM verdicts")?;
    for r in vstmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (c, q, v) = r?;
        verdicts.insert((c, q), v);
    }

    let mut rows: Vec<Value> = vec![];
    for (id, ts, caller, upstream, req_body, resp_body, provider, shadow_body) in calls {
        let request = parse_object(Some(&req_body));
        let mut sides: HashMap<String, Map<String, Value>> = HashMap::new();
        sides.insert(upstream.clone(), parse_object(resp_body.as_deref()));
        sides.insert(provider, parse_object(shadow_body.as_deref()));
        if sides.len() != 2 || !sides.contains_key("typesafe") || !sides.contains_key("second") {
            continue;
        }
        let state = request.get("state").cloned().unwrap_or(Value::Null);
        let questions = match request.get("questions") {
            Some(Value::Object(q)) => q.clone(),
            _ => Map::new(),
        };
        let qids: Vec<String> = questions.keys().cloned().collect();
        let texts = state_texts(&state, &qids);
        let mut all: Vec<&String> = texts.keys().collect();
        if let Value::Object(s) = &state {
            for k in s.keys() {
                if !all.contains(&k) {
                    all.push(k);
                }
            }
        }
        let keys = longest_first(all.into_iter());
        let answers = |side: &Map<String, Value>| side.get("answers").and_then(Value::as_object).cloned().unwrap_or_default();
        let (jev, second) = (answers(&sides["typesafe"]), answers(&sides["second"]));
        for (qid, a) in &jev {
            let Some(b) = second.get(qid) else { continue };
            let Some(distance) = gap(a, b) else { continue };
            let (subject, label) = split_question_id(qid, &keys);
            rows.push(json!({
                "callId": id, "ts": ts, "caller": caller, "served": upstream, "questionId": qid,
                "label": label, "text": subject_text(&state, &texts, &subject), "subject": subject,
                "instructions": instructions(questions.get(qid)),
                "typesafe": {"answer": a, "model": model(&sides["typesafe"])},
                "second": {"answer": b, "model": model(&sides["second"])},
                "gap": format!("{distance:.4}").parse::<f64>().unwrap(), "disagree": disagrees(a, b),
                "verdict": verdicts.get(&(id, qid.clone())),
            }));
        }
    }
    rows.sort_by(|x, y| {
        y["gap"].as_f64().partial_cmp(&x["gap"].as_f64()).unwrap()
            .then(y["callId"].as_i64().cmp(&x["callId"].as_i64()))
            .then(x["questionId"].as_str().cmp(&y["questionId"].as_str()))
    });
    Ok(rows)
}

/// `verdict` None removes the saved verdict. Err on an unknown verdict or a missing call.
pub fn save_verdict(conn: &Connection, call_id: &Value, question_id: &Value, verdict: &Value) -> Result<(), String> {
    let verdict = match verdict {
        Value::Null => None,
        Value::String(v) if VERDICTS.contains(&v.as_str()) => Some(v.as_str()),
        other => {
            let shown = other.as_str().map(String::from).unwrap_or_else(|| other.to_string());
            return Err(format!("unknown verdict: {shown}, use one of {}", VERDICTS.join(", ")));
        }
    };
    let call_id = call_id.as_i64().ok_or_else(|| format!("no call {call_id}"))?;
    let exists = conn.query_row("SELECT 1 FROM calls WHERE id = ?", [call_id], |_| Ok(())).is_ok();
    if !exists {
        return Err(format!("no call {call_id}"));
    }
    let qid = question_id.as_str().unwrap_or("");
    let res = match verdict {
        None => conn.execute("DELETE FROM verdicts WHERE call_id = ? AND question_id = ?", params![call_id, qid]),
        Some(v) => conn.execute(
            "INSERT OR REPLACE INTO verdicts (call_id, question_id, verdict, ts) VALUES (?, ?, ?, ?)",
            params![call_id, qid, v, now_iso()],
        ),
    };
    res.map(|_| ()).map_err(|e| e.to_string())
}
