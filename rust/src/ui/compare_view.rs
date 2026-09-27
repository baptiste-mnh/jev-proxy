//! The Shadow Compare view: one row per question both engines answered, biggest gap first.
use super::answers::*;
use super::drawer::urlenc;
use super::{html_reply, toast_header, with_header, Out, Req};
use crate::compare::{save_verdict, shadow_rows, shadow_rows_of};
use crate::routes::Fail;
use maud::{html, Markup};
use serde_json::{json, Value};
use std::collections::BTreeMap;

struct Filter {
    label: Option<String>,
    disagree: bool,
    unvoted: bool,
}

fn filter(rq: &Req) -> Filter {
    Filter {
        label: rq.q("label").filter(|l| !l.is_empty()).map(String::from),
        disagree: rq.q("disagree") == Some("1"),
        unvoted: rq.q("unvoted") == Some("1"),
    }
}

fn href(f: &Filter, label: Option<&str>, disagree: bool, unvoted: bool) -> String {
    let _ = f;
    format!("/ui/compare?label={}&disagree={}&unvoted={}", label.map(urlenc).unwrap_or_default(), disagree as u8, unvoted as u8)
}

pub fn page(rq: &Req) -> Result<Markup, Fail> {
    Ok(html! { section id="compare-view" { (content(rq, &filter(rq))?) } })
}

pub fn route(rq: &Req) -> Out {
    match (rq.method, rq.path) {
        ("GET", "/ui/compare") => Ok(Some(html_reply(content(rq, &filter(rq))?))),
        ("POST", "/ui/vote") => vote(rq),
        ("GET", "/ui/compare/detail") => detail(rq),
        _ => Ok(None),
    }
}

fn content(rq: &Req, f: &Filter) -> Result<Markup, Fail> {
    let all = shadow_rows(rq.conn, 500)?;
    let rows: Vec<&Value> = all.iter().filter(|r| {
        f.label.as_deref().map_or(true, |l| r["label"] == l) && (!f.disagree || r["disagree"] == true) && (!f.unvoted || r["verdict"].is_null())
    }).collect();
    Ok(html! {
        div id="compare-content" style="display:contents" {
            (summary(rq, &all, f, false))
            div id="compare-filters" {
                button id="only-disagree" class=(if f.disagree { "toggle small on" } else { "toggle small" })
                    hx-get=(href(f, f.label.as_deref(), !f.disagree, f.unvoted)) hx-target="#compare-content" hx-swap="outerHTML" { "Disagreements only" }
                button id="only-unvoted" class=(if f.unvoted { "toggle small on" } else { "toggle small" })
                    hx-get=(href(f, f.label.as_deref(), f.disagree, !f.unvoted)) hx-target="#compare-content" hx-swap="outerHTML" { "Not voted yet" }
                span id="compare-count" { (rows.len()) " of " (all.len()) " answers" }
            }
            div id="compare-wrap" {
                ul id="compare-list" {
                    @if all.is_empty() {
                        div class="compare-empty" { "No shadow yet. Enable both engines in the top bar, then send a call." }
                    } @else {
                        li class="cmp head" {
                            @for c in ["Gap", "Question", "Text", "Jev", rq.second_label(), "Who is right"] { div class="col" { (c) } }
                        }
                        @for r in &rows { (item(rq, r, f)) }
                    }
                }
            }
        }
    })
}

#[derive(Default)]
struct Tally {
    rows: usize,
    disagree: usize,
    votes: BTreeMap<String, usize>,
}

/// One line per question label: how often the engines disagree and how the votes went. A click on a
/// line filters the list on that label, a second click clears the filter.
fn summary(rq: &Req, all: &[Value], f: &Filter, oob: bool) -> Markup {
    let mut by_label: Vec<(String, Tally)> = vec![];
    for r in all {
        let label = r["label"].as_str().unwrap_or("").to_string();
        let i = by_label.iter().position(|(l, _)| *l == label).unwrap_or_else(|| { by_label.push((label.clone(), Tally::default())); by_label.len() - 1 });
        let t = &mut by_label[i].1;
        t.rows += 1;
        if r["disagree"] == true { t.disagree += 1; }
        if let Some(v) = r["verdict"].as_str() { *t.votes.entry(v.to_string()).or_default() += 1; }
    }
    by_label.sort_by(|a, b| b.1.rows.cmp(&a.1.rows));
    html! {
        div id="compare-summary" hx-swap-oob=[oob.then_some("true")] {
            table {
                tr { @for h in ["Question", "Answers", "Disagree", "Jev right", &format!("{} right", rq.second_label()), "Tie", "Neither", "To vote"] { th { (h) } } }
                @for (label, t) in &by_label {
                    @let v = |k: &str| t.votes.get(k).copied().unwrap_or(0);
                    @let voted = v("typesafe") + v("second") + v("tie") + v("neither");
                    @let on = f.label.as_deref() == Some(label.as_str());
                    tr class=(if on { "on" } else { "" })
                        hx-get=(href(f, if on { None } else { Some(label) }, f.disagree, f.unvoted)) hx-target="#compare-content" hx-swap="outerHTML" {
                        td { (label) }
                        td { (t.rows) }
                        td { (t.disagree) " (" ((100.0 * t.disagree as f64 / t.rows as f64).round()) "%)" }
                        td { (v("typesafe")) } td { (v("second")) } td { (v("tie")) } td { (v("neither")) }
                        td { (t.rows - voted) }
                    }
                }
            }
        }
    }
}

fn score(side: &Value) -> Markup {
    let answer = &side["answer"];
    let cell = render_cell(answer);
    let model = side["model"].as_str().filter(|m| !m.is_empty());
    html! {
        div class="cmp-score" {
            @if answer["type"] == "noul" { (swatch(answer["noul"].as_f64().unwrap_or(0.0))) }
            div class=(if cell.flagged { "cmp-val flag" } else { "cmp-val" }) title={(cell.text) @if let Some(m) = model { " · " (m) }} { (cell.text) }
        }
    }
}

fn votes(rq: &Req, r: &Value, label: Option<&str>) -> Markup {
    let current = r["verdict"].as_str();
    let second = rq.second_label().to_string();
    let buttons = [
        ("typesafe", "Jev".to_string(), "Jev is closer to the truth".to_string()),
        ("second", second.clone(), format!("{second} is closer to the truth")),
        ("tie", "Tie".into(), "Both are as right".into()),
        ("neither", "Neither".into(), "Both are wrong".into()),
    ];
    html! {
        div class="cmp-votes" role="radiogroup" aria-label={"Who is right on " (r["questionId"].as_str().unwrap_or(""))} {
            @for (verdict, word, title) in &buttons {
                @let on = current == Some(*verdict);
                button class=(if on { "small on" } else { "small" }) data-verdict=(verdict) role="radio" aria-checked=(on.to_string())
                    title={(title) ". Click again to clear."}
                    hx-post="/ui/vote" hx-target="closest .cmp-votes" hx-swap="outerHTML"
                    hx-vals=(json!({
                        "callId": r["callId"], "questionId": r["questionId"], "label": label.unwrap_or(""),
                        "verdict": if on { "" } else { verdict },
                    }).to_string()) { (word) }
            }
        }
    }
}

fn item(rq: &Req, r: &Value, f: &Filter) -> Markup {
    let gap = r["gap"].as_f64().unwrap_or(0.0);
    let ts = r["ts"].as_str().unwrap_or("");
    html! {
        li class=(if r["disagree"] == true { "cmp disagree" } else { "cmp" }) data-cmp {
            div class="cmp-gap" {
                div class="cmp-gap-val" { (fixed(gap, 2)) }
                div class="cmp-gap-bar" style={"width:" ((gap * 100.0).round()) "%"} {}
            }
            div {
                div class="cmp-label" { (r["label"].as_str().unwrap_or("")) }
                div class="cmp-where" { "call " (r["callId"]) " · " (r["caller"].as_str().unwrap_or("")) " · " time data-ts=(ts) { (ts.get(11..19).unwrap_or(ts)) } }
            }
            div class="cmp-text" { (preview(r["text"].as_str().unwrap_or(""))) }
            (score(&r["typesafe"]))
            (score(&r["second"]))
            (votes(rq, r, f.label.as_deref()))
            div class="cmp-detail"
                hx-get={"/ui/compare/detail?callId=" (r["callId"]) "&questionId=" (urlenc(r["questionId"].as_str().unwrap_or("")))}
                hx-trigger="click from:closest .cmp once" hx-swap="innerHTML" {}
        }
    }
}

const PREVIEW_CHARS: usize = 400;

/// The row shows two lines of the text, so the whole of a very long text is not sent with the list.
fn preview(text: &str) -> String {
    if text.chars().count() <= PREVIEW_CHARS { return text.to_string(); }
    format!("{}…", text.chars().take(PREVIEW_CHARS).collect::<String>())
}

/// The open row: the full text, the question, and who answered.
fn detail(rq: &Req) -> Out {
    let call_id: i64 = rq.q("callId").and_then(|c| c.parse().ok()).unwrap_or(0);
    let qid = rq.q("questionId").unwrap_or("");
    let rows = shadow_rows_of(rq.conn, 1, Some(call_id))?;
    let Some(r) = rows.iter().find(|r| r["questionId"] == qid) else { return Ok(None) };
    let subject = r["subject"].as_str().filter(|s| !s.is_empty()).unwrap_or("state");
    Ok(Some(html_reply(html! {
        div class="merged-key" { (subject) }
        div class="merged-text" { (r["text"].as_str().unwrap_or("")) }
        @if let Some(i) = r["instructions"].as_str().filter(|i| !i.is_empty()) { pre class="raw-json" { (i) } }
        div class="meta" {
            "Jev: " (r["typesafe"]["model"].as_str().unwrap_or("?")) " · " (rq.second_label()) ": " (r["second"]["model"].as_str().unwrap_or("?"))
            " · " (r["questionId"].as_str().unwrap_or(""))
        }
    })))
}

// A vote updates its row and the summary in place, so the list keeps its scroll position.
fn vote(rq: &Req) -> Out {
    let call_id: i64 = rq.f("callId").and_then(|c| c.parse().ok()).unwrap_or(0);
    let qid = rq.f("questionId").unwrap_or("");
    let verdict = rq.f("verdict").filter(|v| !v.is_empty());
    let saved = save_verdict(rq.conn, &json!(call_id), &json!(qid), &verdict.map(|v| json!(v)).unwrap_or(Value::Null));
    let all = shadow_rows(rq.conn, 500)?;
    let row = all.iter().find(|r| r["callId"] == call_id && r["questionId"] == qid);
    let f = Filter { label: rq.f("label").filter(|l| !l.is_empty()).map(String::from), disagree: false, unvoted: false };
    let markup = html! {
        @if let Some(r) = row { (votes(rq, r, f.label.as_deref())) }
        (summary(rq, &all, &f, true))
    };
    let reply = html_reply(markup);
    Ok(Some(match saved {
        Ok(()) => reply,
        Err(m) => with_header(reply, toast_header("Vote not saved", &m, true)),
    }))
}
