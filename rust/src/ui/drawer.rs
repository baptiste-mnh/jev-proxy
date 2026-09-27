//! The Traffic drawer: one recorded call, its answers, its shadows, its raw JSON.
use super::answers::*;
use super::{html_reply, toast_header, upstream_word, with_header, Out, Req};
use crate::db::{call_by_id, call_detail, shadows_by_call};
use crate::proxy::{execute_call, resolve, start_shadows, write_shadow};
use indexmap::IndexMap;
use maud::{html, Markup};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn route(rq: &Req) -> Out {
    let Some(rest) = rq.path.strip_prefix("/ui/call/") else { return Ok(None) };
    let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
    match (rq.method, action) {
        ("GET", "") => open(rq, id, rq.q("tab").unwrap_or("result"), rq.q("item"), rq.q("poll").and_then(|p| p.parse().ok()).unwrap_or(0)),
        ("POST", "run") => run(rq, id),
        ("POST", "resend") => resend(rq, id),
        ("POST", "promote") => promote(rq, id),
        _ => Ok(None),
    }
}

fn load(rq: &Req, id: &str) -> Result<Option<Value>, crate::routes::Fail> {
    let Some(row) = call_by_id(rq.conn, id)? else { return Ok(None) };
    let shadows = shadows_by_call(rq.conn, &[row.id])?;
    Ok(Some(call_detail(&row, &shadows[&row.id])))
}

fn open(rq: &Req, id: &str, tab: &str, item: Option<&str>, poll: u32) -> Out {
    let Some(call) = load(rq, id)? else { return Ok(Some(super::html_reply(html! { aside id="drawer" hidden {} }))) };
    let pending = call["shadows"].as_array().is_some_and(|s| s.iter().any(|s| s["state"] == "pending"));
    let mut reply = html_reply(drawer(rq, &call, tab, item, pending && poll < 60, poll));
    // Once the last shadow answered, the list and the stats have new numbers.
    if poll > 0 && !pending {
        reply = with_header(reply, ("HX-Trigger".into(), "calls-changed".into()));
    }
    Ok(Some(reply))
}

fn other_engine(p: &str) -> &'static str {
    if p == "second" { "typesafe" } else { "second" }
}

fn run(rq: &Req, id: &str) -> Out {
    let Some(row) = call_by_id(rq.conn, id)? else { return Ok(None) };
    let provider = other_engine(&row.upstream);
    if provider == "typesafe" && rq.app.config.api_key.is_empty() {
        return fail(rq, id, "Run on Jev failed", "TYPESAFE_API_KEY not set (environment or .env)");
    }
    let resolved = resolve(rq.conn, provider, &row.path, row.request_body.as_bytes(), &rq.app.config, false)?;
    write_shadow(rq.conn, row.id, provider, "done", Some(&resolved), None)?;
    let r = load(rq, id)?.unwrap();
    Ok(Some(with_header(html_reply(drawer(rq, &r, "compare", None, false, 0)), ("HX-Trigger".into(), "calls-changed".into()))))
}

fn fail(rq: &Req, id: &str, title: &str, message: &str) -> Out {
    let call = load(rq, id)?.unwrap();
    Ok(Some(with_header(html_reply(drawer(rq, &call, "result", None, false, 0)), toast_header(title, message, true))))
}

/// Sends the recorded body again through the proxy, as a new call, and shows that call. The cache
/// still holds the answer while its TTL runs, so replaying a call to inspect it normally costs nothing.
fn resend(rq: &Req, id: &str) -> Out {
    let Some(row) = call_by_id(rq.conn, id)? else { return Ok(None) };
    let settings = crate::db::get_settings(rq.conn)?;
    if settings.served == "typesafe" && rq.app.config.api_key.is_empty() {
        return fail(rq, id, "Resend failed", "TYPESAFE_API_KEY not set (environment or .env)");
    }
    let body = row.request_body.as_bytes();
    let result = execute_call(rq.conn, &row.path, body, "ui", &rq.app.config, &settings.served, false)?;
    let shadows: Vec<String> = settings.engines.iter().filter(|e| **e != settings.served).cloned().collect();
    start_shadows(&Arc::clone(&rq.app.config), result.call_id, &shadows, &row.path, body, false);
    let call = load(rq, &result.call_id.to_string())?.unwrap();
    Ok(Some(with_header(html_reply(drawer(rq, &call, "result", None, true, 1)), ("HX-Trigger".into(), "calls-changed".into()))))
}

/// Edit turns the recorded call into a saved request and moves to the Requests view.
fn promote(rq: &Req, id: &str) -> Out {
    let r = crate::routes::promote_call(rq.conn, id).map_err(|e| crate::routes::Fail(e))?;
    let Some(created) = r else { return Ok(None) };
    Ok(Some(crate::routes::Reply {
        status: 200,
        headers: vec![("HX-Redirect".into(), format!("/requests?id={}", created["id"]))],
        body: vec![],
    }))
}

fn tab_button(id: i64, name: &str, label: &str, active: &str, item: Option<&str>) -> Markup {
    html! {
        button class=(if name == active { "drawer-tab on" } else { "drawer-tab" }) data-tab=(name)
            hx-get={"/ui/call/" (id) "?tab=" (name) @if let Some(i) = item { "&item=" (urlenc(i)) }} hx-target="#drawer" hx-swap="outerHTML" { (label) }
    }
}

pub fn urlenc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

pub fn drawer(rq: &Req, call: &Value, tab: &str, item: Option<&str>, poll: bool, round: u32) -> Markup {
    let id = call["id"].as_i64().unwrap_or(0);
    let upstream = call["upstream"].as_str().unwrap_or("typesafe");
    let shadow = call["shadows"].as_array().and_then(|s| s.iter().find(|s| s["provider"] == other_engine(upstream)));
    // The Compare tab exists only once there is a shadow to compare with.
    let tab = if tab == "compare" && shadow.is_none() { "result" } else { tab };
    let run_label = format!("Run on {}", if upstream == "second" { "Jev".to_string() } else { rq.second_label().to_string() });
    let back = format!("/ui/call/{id}?tab={tab}{}&poll={}", item.map(|i| format!("&item={}", urlenc(i))).unwrap_or_default(), round + 1);
    html! {
        aside id="drawer" data-id=(id) {
            div id="drawer-head" {
                b id="drawer-title" { "call " (id) " · " (call["caller"].as_str().unwrap_or("")) }
                button id="run-btn" hx-post={"/ui/call/" (id) "/run"} hx-target="#drawer" hx-swap="outerHTML" hx-disabled-elt="this" { (run_label) }
                button id="promote-btn" hx-post={"/ui/call/" (id) "/promote"} hx-swap="none" { "Edit" }
                button id="resend-btn" class="primary" hx-post={"/ui/call/" (id) "/resend"} hx-target="#drawer" hx-swap="outerHTML" hx-disabled-elt="this" { "Resend" }
                button id="drawer-close" class="icon" title="Close (Esc)" aria-label="Close" data-close-drawer { "✕" }
            }
            div id="drawer-tabs" {
                (tab_button(id, "result", "Result", tab, item))
                (tab_button(id, "json", "JSON", tab, item))
                @if shadow.is_some() { (tab_button(id, "compare", "Compare", tab, item)) }
            }
            @match tab {
                "json" => div id="drawer-json" { (json_tab(rq, call)) },
                "compare" => div id="drawer-compare" { (compare_tab(rq, call, shadow.unwrap())) },
                _ => div id="drawer-body" { (result_tab(rq, call, item, id)) },
            }
            // A shadow answers after the call, so the drawer reloads until none is pending.
            @if poll { div hx-get=(back) hx-trigger="load delay:1s" hx-target="#drawer" hx-swap="outerHTML" {} }
        }
    }
}

// ---- Result tab ----

fn plain(v: &Value) -> bool {
    v.is_object()
}

struct Subject {
    path: String,
    text: String,
}

/// The subjects a request asks about. A state value that is an object of strings is either a
/// container (`sections`, `comments`), whose keys are the subjects shown as `sections.<key>`, or
/// one item with fields (`item_a: {text, other_version}`), when a question id starts with its key.
fn state_subjects(state: &Value, ids: &[String]) -> IndexMap<String, Subject> {
    let mut subjects: IndexMap<String, Subject> = IndexMap::new();
    let Some(obj) = state.as_object() else { return subjects };
    for (key, value) in obj {
        let entries: Vec<(&String, &Value)> = value.as_object().map(|o| o.iter().collect()).unwrap_or_default();
        if !entries.is_empty() && entries.iter().all(|(_, v)| v.is_string()) {
            if ids.iter().any(|id| id.starts_with(&format!("{key}_"))) {
                let text = if entries.len() == 1 && entries[0].0 == "text" {
                    entries[0].1.as_str().unwrap().to_string()
                } else {
                    entries.iter().map(|(f, v)| format!("{f}:\n{}", v.as_str().unwrap())).collect::<Vec<_>>().join("\n\n")
                };
                subjects.entry(key.clone()).or_insert(Subject { path: key.clone(), text });
                continue;
            }
            for (sub, text) in entries {
                subjects.entry(sub.clone()).or_insert(Subject { path: format!("{key}.{sub}"), text: text.as_str().unwrap().to_string() });
            }
        } else {
            let text = match value { Value::String(s) => s.clone(), v => serde_json::to_string_pretty(v).unwrap_or_default() };
            subjects.entry(key.clone()).or_insert(Subject { path: key.clone(), text });
        }
    }
    subjects
}

/// The subjects a question names in backticks in its instructions.
fn named_subjects(subjects: &IndexMap<String, Subject>, question: &Value) -> Vec<String> {
    let instructions = question["instructions"].as_str().unwrap_or("");
    subjects
        .iter()
        .filter(|(k, s)| instructions.contains(&format!("`{k}`")) || instructions.contains(&format!("`{}`", s.path)))
        .map(|(k, _)| k.clone())
        .collect()
}

struct Entry {
    id: String,
    label: String,
}

struct Groups {
    groups: IndexMap<String, Vec<Entry>>,
    composites: IndexMap<String, (Vec<String>, Vec<Entry>)>,
    other: Vec<Entry>,
}

/// Assigns each question id to the subjects it asks about: the subject the id starts with (longest
/// first), else the subjects its instructions name in backticks, else the only subject.
fn group_ids(subjects: &IndexMap<String, Subject>, ids: &[String], questions: &Value) -> Groups {
    let mut groups: IndexMap<String, Vec<Entry>> = subjects.keys().map(|k| (k.clone(), vec![])).collect();
    let mut composites: IndexMap<String, (Vec<String>, Vec<Entry>)> = IndexMap::new();
    let mut keys: Vec<String> = groups.keys().cloned().collect();
    keys.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
    let mut other = vec![];
    for id in ids {
        if let Some(k) = keys.iter().find(|k| id.starts_with(&format!("{k}_"))) {
            let label = id[k.len()..].trim_start_matches('_').to_string();
            groups.get_mut(k).unwrap().push(Entry { id: id.clone(), label });
            continue;
        }
        let named = named_subjects(subjects, &questions[id.as_str()]);
        if named.len() > 1 {
            let key = named.join(" + ");
            composites.entry(key).or_insert((named.clone(), vec![])).1.push(Entry { id: id.clone(), label: id.clone() });
            continue;
        }
        let key = named.first().cloned().or_else(|| if keys.len() == 1 { Some(keys[0].clone()) } else { None });
        match key {
            Some(k) => groups.get_mut(&k).unwrap().push(Entry { id: id.clone(), label: id.clone() }),
            None => other.push(Entry { id: id.clone(), label: id.clone() }),
        }
    }
    Groups { groups, composites, other }
}

fn question_text(q: &Value) -> String {
    if q.is_null() {
        return String::new();
    }
    let criteria = q["criteria"].as_object().map(|c| c.iter().map(|(k, v)| format!("\n{k}: {}", v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))).collect::<String>()).unwrap_or_default();
    format!("{}{criteria}", q["instructions"].as_str().unwrap_or(""))
}

struct ShadowAns {
    label: String,
    note: String,
    answers: Value,
    model: String,
}

fn shadow_answers(rq: &Req, call: &Value) -> Vec<ShadowAns> {
    call["shadows"].as_array().map(|a| a.iter().map(|s| {
        let body = parse(s["responseBody"].as_str()).unwrap_or(Value::Null);
        ShadowAns {
            label: format!("+{}", upstream_word(rq, s["provider"].as_str().unwrap_or(""))),
            note: if s["state"] == "done" { "no answer".into() } else { s["state"].as_str().unwrap_or("").into() },
            answers: body["answers"].clone(),
            model: answered_by(&body),
        }
    }).collect()).unwrap_or_default()
}

fn merged_question(entry: &Entry, request: &Value, answers: &Value, shadows: &[ShadowAns], model: &str) -> Markup {
    let text = question_text(&request["questions"][entry.id.as_str()]);
    html! {
        div class="merged-q" {
            (answer_row(&entry.label, &answers[entry.id.as_str()], model, None))
            @for s in shadows { (answer_row(&s.label, &s.answers[entry.id.as_str()], &s.model, Some(&s.note))) }
            @if !text.is_empty() { pre class="raw-json" { (text) } }
        }
    }
}

struct Panel {
    key: String,
    label: String,
    node: Markup,
}

fn result_tab(rq: &Req, call: &Value, item: Option<&str>, id: i64) -> Markup {
    let request = parse(call["requestBody"].as_str());
    let response = parse(call["responseBody"].as_str());
    let answers = response.as_ref().map(|r| r["answers"].clone()).unwrap_or(Value::Null);
    let shadows = shadow_answers(rq, call);
    let meta = meta_line(rq, call);
    let has_answers = answers.is_object();
    let raw_response = match &response {
        Some(r) => serde_json::to_string_pretty(r).unwrap_or_default(),
        None => call["responseBody"].as_str().filter(|t| !t.is_empty()).unwrap_or("No response body recorded.").to_string(),
    };
    let head = html! {
        div class="meta" { (meta) }
        @if let Some(e) = call["error"].as_str().filter(|e| !e.is_empty()) { pre class="raw-json" { (e) } }
        @if !has_answers { pre class="raw-json" { (raw_response) } }
    };
    let Some(request) = request else {
        return html! { (head) pre class="raw-json" { (call["requestBody"].as_str().filter(|t| !t.is_empty()).unwrap_or("No request body recorded.")) } };
    };

    let is_object = plain(&request["state"]);
    let mut ids: Vec<String> = request["questions"].as_object().map(|q| q.keys().cloned().collect()).unwrap_or_default();
    for k in answers.as_object().map(|a| a.keys()).into_iter().flatten() {
        if !ids.contains(k) { ids.push(k.clone()); }
    }
    let subjects = state_subjects(if is_object { &request["state"] } else { &Value::Null }, &ids);
    let Groups { groups, composites, other } = group_ids(&subjects, &ids, &request["questions"]);
    // A question may name other items as context: the tab shows them first, and they never count as "not asked".
    let mut referenced: std::collections::HashSet<String> = composites.values().flat_map(|(k, _)| k.clone()).collect();
    let mut context_of = |own: &[String], entries: &[Entry]| -> Vec<String> {
        let mut extra: Vec<String> = vec![];
        for e in entries {
            for k in named_subjects(&subjects, &request["questions"][e.id.as_str()]) {
                if !own.contains(&k) && !extra.contains(&k) { extra.push(k); }
            }
        }
        for k in &extra { referenced.insert(k.clone()); }
        extra
    };
    let mut contexts: IndexMap<String, Vec<String>> = IndexMap::new();
    for (key, entries) in &groups {
        if !entries.is_empty() { contexts.insert(key.clone(), context_of(&[key.clone()], entries)); }
    }
    for (key, (keys, entries)) in &composites {
        contexts.insert(format!("+{key}"), context_of(keys, entries));
    }

    let model = answered_by(response.as_ref().unwrap_or(&Value::Null));
    let card = |keys: &[String], entries: &[Entry], context: &[String]| -> Markup {
        html! {
            div class="merged" {
                @for k in context.iter().chain(keys.iter()) {
                    @if let Some(s) = subjects.get(k) {
                        div class="merged-key" { (s.path) }
                        div class="merged-text" { (s.text) }
                    }
                }
                @for e in entries { (merged_question(e, &request, &answers, &shadows, &model)) }
            }
        }
    };
    let mut panels: Vec<Panel> = vec![];
    let mut idle: Vec<&Subject> = vec![];
    for (key, entries) in &groups {
        if entries.is_empty() {
            if !referenced.contains(key) { idle.push(&subjects[key]); }
            continue;
        }
        panels.push(Panel { key: key.clone(), label: key.clone(), node: card(&[key.clone()], entries, &contexts[key]) });
    }
    for (key, (keys, entries)) in &composites {
        panels.push(Panel { key: format!("+{key}"), label: key.clone(), node: card(keys, entries, &contexts[&format!("+{key}")]) });
    }
    if !other.is_empty() || !is_object {
        let node = html! {
            div class="merged" {
                @if !is_object && !request["state"].is_null() {
                    div class="merged-key" { "state" }
                    div class="merged-text" { (match &request["state"] { Value::String(s) => s.clone(), v => serde_json::to_string_pretty(v).unwrap_or_default() }) }
                }
                @for e in &other { (merged_question(e, &request, &answers, &shadows, &model)) }
            }
        };
        panels.push(Panel { key: "~other".into(), label: if is_object { "other".into() } else { "state".into() }, node });
    }
    if !idle.is_empty() {
        let node = html! {
            div class="merged idle" {
                @for s in &idle {
                    div class="merged-key" { (s.path) }
                    div class="merged-text" { (s.text) }
                }
            }
        };
        panels.push(Panel { key: "~idle".into(), label: format!("not asked · {}", idle.len()), node });
    }

    if panels.len() <= 1 {
        return html! { (head) div class="answers" { @for p in &panels { (p.node) } } };
    }
    let current = panels.iter().position(|p| Some(p.key.as_str()) == item).unwrap_or(0);
    html! {
        (head)
        div class="item-tabs" {
            @for (i, p) in panels.iter().enumerate() {
                button class=(if i == current { "item-tab on" } else { "item-tab" }) title=(p.label)
                    hx-get={"/ui/call/" (id) "?tab=result&item=" (urlenc(&p.key))} hx-target="#drawer" hx-swap="outerHTML" {
                    span class="item-tab-label" { (p.label) }
                }
            }
        }
        div class="answers" { (panels[current].node) }
    }
}

// ---- JSON tab ----

fn json_panel(title: &str, parsed: Option<Value>, raw: &str) -> Markup {
    html! {
        div class="json-panel" {
            div class="sent-title" { (title) }
            pre class="json-code" { @if let Some(p) = &parsed { (highlight_json(p)) } @else { (if raw.is_empty() { "(empty)" } else { raw }) } }
        }
    }
}

fn json_tab(rq: &Req, call: &Value) -> Markup {
    let req_text = call["requestBody"].as_str().unwrap_or("");
    let resp_text = call["responseBody"].as_str().unwrap_or("");
    let upstream = call["upstream"].as_str().unwrap_or("");
    let served_title = if upstream.is_empty() { "Response".to_string() } else { format!("Response · {}", upstream_word(rq, upstream)) };
    html! {
        (json_panel("Request", parse(Some(req_text)), req_text))
        div class="json-responses" {
            (json_panel(&served_title, parse(Some(resp_text)), resp_text))
            @for s in call["shadows"].as_array().into_iter().flatten() {
                @let state = s["state"].as_str().unwrap_or("");
                @let text = s["responseBody"].as_str().filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| if state == "done" { s["error"].as_str().unwrap_or("").to_string() } else { state.to_string() });
                (json_panel(&format!("Response · +{} ({state})", upstream_word(rq, s["provider"].as_str().unwrap_or(""))), parse(s["responseBody"].as_str()), &text))
            }
        }
    }
}

// ---- Compare tab ----

fn disagrees(a: &Value, b: &Value) -> bool {
    crate::compare::disagrees(a, b)
}

fn answer_delta(jev: &Value, second: &Value) -> String {
    if jev.is_null() || second.is_null() || jev["type"] != second["type"] { return String::new(); }
    let signed = |n: f64| format!("{}{}", if n >= 0.0 { "+" } else { "" }, fixed(n, 2));
    match jev["type"].as_str() {
        Some("noul") => signed(second["noul"].as_f64().unwrap_or(0.0) - jev["noul"].as_f64().unwrap_or(0.0)),
        Some("score") => signed(second["score"].as_f64().unwrap_or(0.0) - jev["score"].as_f64().unwrap_or(0.0)),
        Some("choice") => if jev["choice"] == second["choice"] { "same".into() } else { "differs".into() },
        _ => String::new(),
    }
}

/// One row per question id, Jev and Second side by side.
fn compare_tab(rq: &Req, call: &Value, shadow: &Value) -> Markup {
    let upstream = call["upstream"].as_str().unwrap_or("typesafe");
    let served_meta = call.clone();
    let mut shadow_meta = shadow.clone();
    shadow_meta["upstream"] = shadow["provider"].clone();
    let served_body = parse(call["responseBody"].as_str());
    let (note, other_body) = match shadow["state"].as_str() {
        Some("pending") => (Some("pending".to_string()), None),
        Some("skipped") => (Some(format!("skipped: {}", shadow["error"].as_str().filter(|e| !e.is_empty()).unwrap_or("no reason recorded"))), None),
        _ => (None, Some(parse(shadow["responseBody"].as_str()))),
    };
    let other_body = other_body.flatten();
    // (name, note, meta, body, raw)
    struct Side<'a> { name: String, note: Option<String>, meta: &'a Value, body: Option<Value>, raw: String }
    let served = Side { name: String::new(), note: None, meta: &served_meta, body: served_body, raw: call["responseBody"].as_str().unwrap_or("").to_string() };
    let other = Side { name: String::new(), note, meta: &shadow_meta, body: other_body, raw: shadow["responseBody"].as_str().or(shadow["error"].as_str()).unwrap_or("").to_string() };
    let (mut jev, mut second) = if upstream == "second" { (other, served) } else { (served, other) };
    jev.name = "Jev".into();
    second.name = rq.second_label().to_string();
    let empty = json!({});
    let ja = jev.body.as_ref().map(|b| &b["answers"]).filter(|a| a.is_object()).unwrap_or(&empty);
    let sa = second.body.as_ref().map(|b| &b["answers"]).filter(|a| a.is_object()).unwrap_or(&empty);
    let mut ids: Vec<&String> = ja.as_object().unwrap().keys().collect();
    for k in sa.as_object().unwrap().keys() { if !ids.contains(&k) { ids.push(k); } }
    let mut disagreements = 0;
    let rows: Vec<Markup> = ids.iter().map(|id| {
        let (a, b) = (&ja[id.as_str()], &sa[id.as_str()]);
        let flag = disagrees(a, b);
        if flag { disagreements += 1; }
        html! {
            tr class=(if flag { "disagree" } else { "" }) {
                td class="qid" { (id) }
                td class="num" { (render_cell(a).text) }
                td class="num" { (render_cell(b).text) }
                td class="num" { (answer_delta(a, b)) }
                td class="num" { (if flag { "DISAGREE" } else { "" }) }
            }
        }
    }).collect();
    html! {
        div class="compare-meta" {
            @for s in [&jev, &second] {
                div class="meta" { (s.name) " · " (match &s.note { Some(n) => n.clone(), None => meta_line(rq, s.meta) }) }
            }
        }
        @for s in [&jev, &second] {
            @if s.note.is_none() && !s.body.as_ref().is_some_and(|b| b["answers"].is_object()) {
                pre class="raw-json" { (s.name) ": " (if s.raw.is_empty() { "No response body." } else { &s.raw }) }
            }
        }
        table {
            tr { th { "Question" } th { "Jev" } th { (second.name) } th { "Delta" } th {} }
            @for r in rows { (r) }
        }
        div class="meta" { (disagreements) " disagreement" (if disagreements == 1 { "" } else { "s" }) }
    }
}
