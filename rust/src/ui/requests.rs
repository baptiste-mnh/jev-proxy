//! The Requests view: saved requests and the item/question editor. The form is the state: every
//! edit posts it, the server applies the change and renders the editor again.
use super::answers::*;
use super::{html_reply, toast_header, with_header, Out, Req};
use crate::db::{get_settings, request_by_id};
use crate::proxy::{execute_call, start_shadows};
use crate::routes::{Fail, Reply};
use crate::util::now_iso;
use maud::{html, Markup};
use rusqlite::params;
use serde_json::{json, Value};

#[derive(Clone)]
struct Question {
    suffix: String,
    instructions: String,
    criteria_true: String,
    criteria_false: String,
}

struct Editor {
    id: i64,
    name: String,
    model: String,
    items: Vec<(String, String)>,
    questions: Vec<Question>,
    json: String,
    is_override: bool,
    tab: String,
}

const DEFAULT_QUESTION: (&str, &str) = ("new_question", "Does {ref} ...?");

/// Expands `{ref}` to every item key, which is how one question becomes an answer per item. The
/// answer id shape `${item}_${suffix}` is the contract the results matrix reads back.
fn build_request_json(e: &Editor) -> Value {
    let state: serde_json::Map<String, Value> = e.items.iter().map(|(k, t)| (k.clone(), json!(t))).collect();
    let mut questions = serde_json::Map::new();
    for (key, _) in &e.items {
        for q in &e.questions {
            questions.insert(
                format!("{key}_{}", q.suffix),
                json!({"type": "noul", "instructions": q.instructions.replace("{ref}", &format!("`{key}`")),
                       "criteria": {"true": q.criteria_true, "false": q.criteria_false}}),
            );
        }
    }
    json!({"state": state, "model": if e.model.is_empty() { "jev-latest" } else { &e.model }, "questions": questions})
}

fn generated(e: &Editor) -> String {
    serde_json::to_string_pretty(&build_request_json(e)).unwrap_or_default()
}

fn from_row(row: &Value) -> Editor {
    let items = row["items"].as_array().map(|a| a.iter().map(|i| (i["key"].as_str().unwrap_or("").to_string(), i["text"].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
    let questions = row["questions"].as_array().map(|a| a.iter().map(|q| Question {
        suffix: q["suffix"].as_str().unwrap_or("").into(),
        instructions: q["instructions"].as_str().unwrap_or("").into(),
        criteria_true: q["criteria_true"].as_str().unwrap_or("").into(),
        criteria_false: q["criteria_false"].as_str().unwrap_or("").into(),
    }).collect()).unwrap_or_default();
    let mut e = Editor {
        id: row["id"].as_i64().unwrap_or(0), name: row["name"].as_str().unwrap_or("").into(),
        model: row["model"].as_str().unwrap_or("jev-latest").into(), items, questions,
        json: String::new(), is_override: row["rawJsonOverride"].is_string(), tab: "body".into(),
    };
    e.json = row["rawJsonOverride"].as_str().map(String::from).unwrap_or_else(|| generated(&e));
    e
}

fn from_form(rq: &Req, id: i64) -> Editor {
    let (keys, texts) = (rq.all("item_key"), rq.all("item_text"));
    let items = keys.iter().enumerate().map(|(i, k)| (k.to_string(), texts.get(i).copied().unwrap_or("").to_string())).collect();
    let (s, ins, t, f) = (rq.all("q_suffix"), rq.all("q_instructions"), rq.all("q_true"), rq.all("q_false"));
    let questions = s.iter().enumerate().map(|(i, suffix)| Question {
        suffix: suffix.to_string(),
        instructions: ins.get(i).copied().unwrap_or("").into(),
        criteria_true: t.get(i).copied().unwrap_or("").into(),
        criteria_false: f.get(i).copied().unwrap_or("").into(),
    }).collect();
    let mut e = Editor {
        id, name: rq.f("name").unwrap_or("").into(), model: rq.f("model").unwrap_or("jev-latest").into(), items, questions,
        json: rq.f("json").unwrap_or("").into(), is_override: rq.f("override") == Some("1"), tab: rq.f("tab").unwrap_or("body").into(),
    };
    if !e.is_override { e.json = generated(&e); }
    e
}

fn apply(e: &mut Editor, op: &str) {
    let (name, n) = op.split_once(':').map(|(a, b)| (a, b.parse::<usize>().unwrap_or(usize::MAX))).unwrap_or((op, usize::MAX));
    match name {
        "add-item" => e.items.push((format!("item_{}", e.items.len() + 1), String::new())),
        "dup-item" if n < e.items.len() => { let (k, t) = e.items[n].clone(); e.items.insert(n + 1, (format!("{k}_copy"), t)); }
        "del-item" if n < e.items.len() => { e.items.remove(n); }
        "add-q" => e.questions.push(Question { suffix: DEFAULT_QUESTION.0.into(), instructions: DEFAULT_QUESTION.1.into(), criteria_true: String::new(), criteria_false: String::new() }),
        "dup-q" if n < e.questions.len() => { let mut q = e.questions[n].clone(); q.suffix = format!("{}_copy", q.suffix); e.questions.insert(n + 1, q); }
        "del-q" if n < e.questions.len() => { e.questions.remove(n); }
        "revert" => { e.is_override = false; e.json = generated(e); }
        _ => {}
    }
    if !e.is_override { e.json = generated(e); }
}

// ---- routes ----

fn selected(rq: &Req) -> Option<i64> {
    rq.q("id").and_then(|i| i.parse().ok())
}

pub fn page(rq: &Req) -> Result<Markup, Fail> {
    let id = selected(rq);
    let editor = match id {
        Some(id) => request_by_id(rq.conn, &id.to_string())?.map(|r| from_row(&r)),
        None => None,
    };
    Ok(html! {
        section id="requests-view" {
            (request_bar(rq, editor.as_ref().map(|e| e.id), false)?)
            @if let Some(e) = &editor {
                (editor_markup(rq, e))
                div id="output-panel" {
                    div class="meta" { "Send a request to see the response here." }
                }
            } @else {
                div class="empty-note" {
                    p { "No request open." }
                    p { "Press + to write one, or open a call in Traffic and press Edit to start from it." }
                }
            }
        }
    })
}

fn request_bar(rq: &Req, current: Option<i64>, oob: bool) -> Result<Markup, Fail> {
    let mut stmt = rq.conn.prepare("SELECT id, name FROM requests ORDER BY updated_at DESC")?;
    let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    Ok(html! {
        div id="request-bar" hx-swap-oob=[oob.then_some("true")] {
            ul id="request-list" {
                @for (id, name) in &rows {
                    li class=(if Some(*id) == current { "on" } else { "" }) {
                        a class="req-name" href={"/requests?id=" (id)} { (name) }
                        button class="icon small req-close" title="Delete this saved request" aria-label={"Delete " (name)}
                            hx-delete={"/ui/requests/" (id)} hx-swap="none"
                            hx-confirm={"Delete \"" (name) "\"? The recorded calls stay in Traffic."} { "✕" }
                    }
                }
            }
            button id="new-btn" class="icon" title="New request" aria-label="New request" hx-post="/ui/requests/new" hx-swap="none" { "+" }
        }
    })
}

pub fn route(rq: &Req) -> Out {
    if rq.path == "/ui/requests/new" && rq.method == "POST" {
        let now = now_iso();
        rq.conn.execute(
            "INSERT INTO requests (name, model, items_json, questions_json, raw_json_override, last_response, created_at, updated_at) VALUES ('Untitled request', 'jev-latest', ?, ?, NULL, NULL, ?, ?)",
            params![json!([{"key": "item_1", "text": ""}]).to_string(), default_questions().to_string(), now, now],
        )?;
        return Ok(Some(redirect(&format!("/requests?id={}", rq.conn.last_insert_rowid()))));
    }
    let Some(rest) = rq.path.strip_prefix("/ui/requests/") else { return Ok(None) };
    let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
    let Ok(id) = id.parse::<i64>() else { return Ok(None) };
    match (rq.method, action) {
        ("DELETE", "") => {
            rq.conn.execute("DELETE FROM requests WHERE id = ?", [id])?;
            Ok(Some(redirect("/requests")))
        }
        ("POST", "edit") => edit(rq, id),
        ("POST", "save") => save(rq, id),
        ("POST", "send") => send(rq, id),
        _ => Ok(None),
    }
}

fn redirect(to: &str) -> Reply {
    Reply { status: 200, headers: vec![("HX-Redirect".into(), to.into())], body: vec![] }
}

fn default_questions() -> Value {
    json!([
        {"suffix": "to_the_point", "instructions": "Does {ref} get straight to the point, with no preamble and no over-explanation of the mechanism before or instead of stating the point?", "criteria_true": "Direct, states the point and stops", "criteria_false": "Preamble, hedging, or explains the mechanism before the point"},
        {"suffix": "sounds_human", "instructions": "Does {ref} read as written by a person on a dev team, not generated by an AI?", "criteria_true": "Natural, casual, varied phrasing", "criteria_false": "Robotic: restates the location, over-explains, flat even tone"},
        {"suffix": "is_kiss", "instructions": "Is {ref} as short as it can be while still making sense on its own? One line is the norm, two sentences is the ceiling.", "criteria_true": "As short as possible without losing the point", "criteria_false": "Could be shorter, or needs more context to land"},
    ])
}

/// A structural edit renders the editor again. `sync` only refreshes the JSON tab and the previews,
/// so typing never loses focus.
fn edit(rq: &Req, id: i64) -> Out {
    let mut e = from_form(rq, id);
    let op = rq.q("op").unwrap_or("");
    apply(&mut e, op);
    if op == "sync" {
        let first = e.items.first().map(|i| i.0.clone()).unwrap_or_else(|| "item_1".into());
        return Ok(Some(html_reply(html! {
            @if !e.is_override { (json_textarea(&e, true)) }
            @for (i, q) in e.questions.iter().enumerate() { (hint(i, &q.instructions, &first, true)) }
        })));
    }
    Ok(Some(html_reply(editor_markup(rq, &e))))
}

fn update_row(rq: &Req, e: &Editor) -> Result<(), Fail> {
    let raw = if e.is_override { Some(e.json.clone()) } else { None };
    let items: Vec<Value> = e.items.iter().map(|(k, t)| json!({"key": k, "text": t})).collect();
    let questions: Vec<Value> = e.questions.iter().map(|q| json!({"suffix": q.suffix, "instructions": q.instructions, "criteria_true": q.criteria_true, "criteria_false": q.criteria_false})).collect();
    rq.conn.execute(
        "UPDATE requests SET name = ?, model = ?, items_json = ?, questions_json = ?, raw_json_override = ?, updated_at = ? WHERE id = ?",
        params![if e.name.is_empty() { "Untitled request" } else { &e.name }, if e.model.is_empty() { "jev-latest" } else { &e.model },
                json!(items).to_string(), json!(questions).to_string(), raw, now_iso(), e.id],
    )?;
    Ok(())
}

fn save(rq: &Req, id: i64) -> Out {
    let e = from_form(rq, id);
    update_row(rq, &e)?;
    let mut fresh = request_by_id(rq.conn, &id.to_string())?.map(|r| from_row(&r)).unwrap_or(e);
    fresh.tab = rq.f("tab").unwrap_or("body").into();
    Ok(Some(html_reply(html! { (editor_markup(rq, &fresh)) (request_bar(rq, Some(id), true)?) })))
}

fn send(rq: &Req, id: i64) -> Out {
    let e = from_form(rq, id);
    let payload: Value = match serde_json::from_str(&e.json) {
        Ok(p) => p,
        Err(err) => return Ok(Some(with_header(Reply { status: 200, headers: vec![("HX-Reswap".into(), "none".into())], body: vec![] }, toast_header("Invalid JSON", &err.to_string(), true)))),
    };
    update_row(rq, &e)?;
    let settings = get_settings(rq.conn)?;
    if settings.served == "typesafe" && rq.app.config.api_key.is_empty() {
        return Ok(Some(with_header(Reply { status: 200, headers: vec![("HX-Reswap".into(), "none".into())], body: vec![] }, toast_header("Send failed", "TYPESAFE_API_KEY not set (environment or .env)", true))));
    }
    let body = serde_json::to_vec(&payload).unwrap_or_default();
    let result = execute_call(rq.conn, "/v1/systemone", &body, "ui", &rq.app.config, &settings.served, false)?;
    let shadows: Vec<String> = settings.engines.iter().filter(|s| **s != settings.served).cloned().collect();
    start_shadows(&rq.app.config, result.call_id, &shadows, "/v1/systemone", &body, false);
    let parsed = parse(Some(&result.body));
    let meta = json!({
        "status": result.status, "elapsedMs": result.elapsed_ms, "cached": result.cached, "attempts": result.attempts,
        "upstream": result.upstream,
        "inputTokens": parsed.as_ref().and_then(|b| b["usage"]["input_tokens"].as_i64()),
        "outputTokens": parsed.as_ref().and_then(|b| b["usage"]["output_tokens"].as_i64()),
    });
    let saved = request_by_id(rq.conn, &id.to_string())?.map(|r| from_row(&r));
    Ok(Some(html_reply(html! {
        (matrix(rq, parsed.as_ref(), &meta, &result.body, &e))
        (request_bar(rq, saved.as_ref().map(|s| s.id), true)?)
    })))
}

// ---- editor markup ----

fn json_textarea(e: &Editor, oob: bool) -> Markup {
    html! {
        textarea id="json-textarea" name="json" spellcheck="false" hx-swap-oob=[oob.then_some("true")]
            hx-on:input="this.form.override.value='1';document.getElementById('override-badge').hidden=false;document.getElementById('revert-override-btn').hidden=false" { (e.json) }
    }
}

fn hint(i: usize, instructions: &str, first_key: &str, oob: bool) -> Markup {
    html! { div id={"hint-" (i)} class="hint" hx-swap-oob=[oob.then_some("true")] { "→ " (instructions.replace("{ref}", &format!("`{first_key}`"))) } }
}

fn editor_markup(rq: &Req, e: &Editor) -> Markup {
    let _ = rq;
    let id = e.id;
    let edit = |op: &str| format!("/ui/requests/{id}/edit?op={op}");
    let mut seen = std::collections::HashSet::new();
    let dupes: std::collections::HashSet<&String> = e.items.iter().filter(|(k, _)| !seen.insert(k)).map(|(k, _)| k).collect();
    let first = e.items.first().map(|i| i.0.as_str()).unwrap_or("item_1");
    let body_on = e.tab != "json";
    // A structural button: posts the form with an operation, and the editor comes back rendered.
    let op_button = |class: &str, title: &str, label: &str, op: String| html! {
        button type="button" class=(class) title=(title) hx-post=(edit(&op)) hx-target="#editor" hx-swap="outerHTML" { (label) }
    };
    html! {
        form id="editor" hx-target="#editor" hx-swap="outerHTML" autocomplete="off" {
            input type="hidden" name="tab" value=(e.tab);
            input type="hidden" name="override" value=(if e.is_override { "1" } else { "0" });
            div id="editor-head" {
                input id="request-name" type="text" name="name" value=(e.name) placeholder="Untitled request";
                button id="save-btn" type="button" title="⌘S" hx-post={"/ui/requests/" (id) "/save"} { "Save" }
                button id="delete-btn" type="button" class="danger" hx-delete={"/ui/requests/" (id)} hx-swap="none"
                    hx-confirm={"Delete \"" (e.name) "\"? The recorded calls stay in Traffic."} { "Delete" }
                button id="send-btn" type="button" class="primary" title="⌘⏎" hx-post={"/ui/requests/" (id) "/send"} hx-target="#output-panel" hx-swap="outerHTML" hx-disabled-elt="this" { "Send" }
            }
            div class="tabs" {
                button type="button" class=(if body_on { "tab-btn on" } else { "tab-btn" }) data-tab="body" { "Body" }
                button type="button" class=(if body_on { "tab-btn" } else { "tab-btn on" }) data-tab="json" { "JSON" span id="override-badge" hidden[!e.is_override] { "override" } }
                button type="button" id="revert-override-btn" class="ghost small" hidden[!e.is_override] hx-post=(edit("revert")) hx-target="#editor" hx-swap="outerHTML" { "Revert to form" }
            }
            div id="tab-body" class=(if body_on { "on" } else { "" })
                hx-post=(edit("sync")) hx-trigger="input changed delay:400ms" hx-swap="none" {
                label { "Model" }
                input type="text" id="model-input" name="model" value=(e.model);

                div class="section-header" {
                    label { "Items (state) · " (e.items.len()) }
                    (op_button("small", "", "+ item", "add-item".into()))
                }
                @for (i, (key, text)) in e.items.iter().enumerate() {
                    div class="item-row" {
                        div class="row-top" {
                            input type="text" name="item_key" value=(key) hx-post=(edit("noop")) hx-trigger="change" hx-target="#editor" hx-swap="outerHTML";
                            (op_button("icon", "Duplicate", "⧉", format!("dup-item:{i}")))
                            (op_button("icon danger", "Remove", "✕", format!("del-item:{i}")))
                        }
                        textarea name="item_text" { (text) }
                        @if dupes.contains(key) { div class="warn" { "Duplicate key: this item overwrites the other one in `state`." } }
                    }
                }

                div class="section-header" {
                    label { "Questions · applied to every item" }
                    (op_button("small", "", "+ question", "add-q".into()))
                }
                @for (i, q) in e.questions.iter().enumerate() {
                    div class="question-row" {
                        div class="row-top" {
                            input type="text" name="q_suffix" value=(q.suffix) hx-post=(edit("noop")) hx-trigger="change" hx-target="#editor" hx-swap="outerHTML";
                            (op_button("icon", "Duplicate", "⧉", format!("dup-q:{i}")))
                            (op_button("icon danger", "Remove", "✕", format!("del-q:{i}")))
                        }
                        textarea name="q_instructions" { (q.instructions) }
                        (hint(i, &q.instructions, first, false))
                        div class="criteria-row" {
                            input type="text" name="q_true" placeholder="true means…" value=(q.criteria_true);
                            input type="text" name="q_false" placeholder="false means…" value=(q.criteria_false);
                        }
                    }
                }
            }
            div id="tab-json" class=(if body_on { "" } else { "on" }) { (json_textarea(e, false)) }
        }
    }
}

// ---- results matrix ----

/// Rebuilds the item x question grid from answer ids shaped `${item}_${suffix}`. An id matching no
/// known pair lands in the orphans, which is what a manual JSON override produces.
fn matrix(rq: &Req, body: Option<&Value>, meta: &Value, raw: &str, e: &Editor) -> Markup {
    let line = meta_line(rq, meta);
    let Some(answers) = body.and_then(|b| b["answers"].as_object()) else {
        let text = match body { Some(b) => serde_json::to_string_pretty(b).unwrap_or_default(), None => raw.to_string() };
        return html! { div id="output-panel" { div class="meta" { (line) } pre class="raw-json" { (text) } } };
    };
    let rows: Vec<&String> = e.items.iter().map(|i| &i.0).collect();
    let cols: Vec<&String> = e.questions.iter().map(|q| &q.suffix).collect();
    let mut placed = std::collections::HashSet::new();
    for r in &rows { for c in &cols { placed.insert(format!("{r}_{c}")); } }
    let orphans: Vec<(&String, &Value)> = answers.iter().filter(|(id, _)| !placed.contains(id.as_str())).collect();
    html! {
        div id="output-panel" {
            div class="meta" { (line) }
            @if !rows.is_empty() && !cols.is_empty() {
                table {
                    tr { th {} @for c in &cols { th { (c) } } }
                    @for r in &rows {
                        tr {
                            td class="cell" { (r) }
                            @for c in &cols {
                                @let answer = answers.get(&format!("{r}_{c}"));
                                @if let Some(a) = answer {
                                    @let cell = render_cell(a);
                                    td data-toast-title={(r) " · " (c)} data-toast-body=(serde_json::to_string_pretty(a).unwrap_or_default()) {
                                        div class=(if cell.flagged { "cell crit" } else { "cell" }) { (cell.text) }
                                        @if let Some(ratio) = cell.ratio {
                                            div class=(if cell.flagged { "bar crit" } else { "bar" }) style={"width:" ((ratio.clamp(0.0, 1.0) * 100.0).round()) "%"} {}
                                        }
                                    }
                                } @else { td {} }
                            }
                        }
                    }
                }
            }
            @if !orphans.is_empty() {
                div class="section-header" { label { "Unmatched answers · " (orphans.len()) } }
                div class="answers" { @for (id, a) in &orphans { (answer_row(id, a, "", None)) } }
            }
        }
    }
}
