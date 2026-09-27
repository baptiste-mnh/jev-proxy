//! The Traffic view: stat tiles, caller filter, and the call stream.
use super::answers::*;
use super::drawer::urlenc;
use super::{html_reply, upstream_word, Out, Req};
use crate::routes::{calls_list, stats_json, Fail};
use maud::{html, Markup};
use serde_json::Value;

struct Filter {
    caller: Option<String>,
    hide: bool,
    open: Option<i64>,
}

fn filter(rq: &Req) -> Filter {
    Filter {
        caller: rq.q("caller").filter(|c| !c.is_empty()).map(String::from),
        hide: rq.q("hide") == Some("1"),
        open: rq.q("open").and_then(|o| o.parse().ok()),
    }
}

pub fn page(rq: &Req) -> Result<Markup, Fail> {
    let f = filter(rq);
    Ok(html! {
        section id="traffic-view" {
            (top(rq, &f)?)
            div id="stream-wrap" {
                (call_list(rq, &f)?)
                aside id="drawer" hidden {}
            }
        }
    })
}

pub fn route(rq: &Req) -> Out {
    if rq.method == "GET" && rq.path == "/ui/traffic" {
        let f = filter(rq);
        return Ok(Some(html_reply(html! { (top(rq, &f)?) (call_list_oob(rq, &f)?) })));
    }
    Ok(None)
}

const CALLERS_PARAM: &str = "caller";

fn top(rq: &Req, f: &Filter) -> Result<Markup, Fail> {
    let s = stats_json(rq.conn, 7)?;
    let n = |k: &str| s[k].as_i64().unwrap_or(0);
    let calls = n("calls");
    let tiles: Vec<(&str, String, String, bool)> = vec![
        ("Calls", format_number(Some(calls)), format!("{} real · {} cached", n("real"), n("cached")), false),
        ("Cache", if calls > 0 { format!("{}%", (100.0 * n("cached") as f64 / calls as f64).round()) } else { "—".into() }, format!("{} avoided", n("cached")), false),
        ("Tokens in", format_number(Some(n("inputTokens"))), format!("{} saved", format_number(Some(n("savedInputTokens")))), false),
        ("Tokens out", format_number(Some(n("outputTokens"))), format!("{} saved", format_number(Some(n("savedOutputTokens")))), false),
        ("Errors", format_number(Some(n("errors"))), if n("errors") > 0 { "non-2xx responses".into() } else { "none".into() }, n("errors") > 0),
    ];
    let callers: Vec<&str> = s["callers"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let href = |caller: Option<&str>, hide: bool| {
        format!("/ui/traffic?{}={}&hide={}{}", CALLERS_PARAM, caller.map(urlenc).unwrap_or_default(), if hide { 1 } else { 0 },
            f.open.map(|o| format!("&open={o}")).unwrap_or_default())
    };
    let chip = |label: &str, caller: Option<&str>| html! {
        span class=(if f.caller.as_deref() == caller { "chip on" } else { "chip" }) data-caller=(caller.unwrap_or(""))
            hx-get=(href(caller, f.hide)) hx-target="#traffic-top" hx-swap="outerHTML" { (label) }
    };
    Ok(html! {
        div id="traffic-top"
            hx-get={"/ui/traffic?caller=" (f.caller.as_deref().map(urlenc).unwrap_or_default()) "&hide=" (if f.hide { 1 } else { 0 })}
            hx-trigger="calls-changed from:body" hx-target="#traffic-top" hx-swap="outerHTML"
            hx-vals="js:{open: (document.querySelector('#drawer:not([hidden])')||{dataset:{}}).dataset.id||''}" {
            div id="stat-row" {
                @for (label, value, sub, crit) in &tiles {
                    div class="stat" {
                        div class="lab" { (label) }
                        div class=(if *crit { "val crit" } else { "val" }) { (value) }
                        div class="sub" { (sub) }
                    }
                }
            }
            div id="filter-bar" {
                span id="caller-chips" {
                    (chip("all", None))
                    @for c in &callers { (chip(c, Some(c))) }
                }
                button id="hide-hits" class=(if f.hide { "toggle small on" } else { "toggle small" })
                    hx-get=(href(f.caller.as_deref(), !f.hide)) hx-target="#traffic-top" hx-swap="outerHTML" { "Hide cache hits" }
            }
        }
    })
}

fn call_list_oob(rq: &Req, f: &Filter) -> Result<Markup, Fail> {
    call_list_with(rq, f, true)
}

fn call_list(rq: &Req, f: &Filter) -> Result<Markup, Fail> {
    call_list_with(rq, f, false)
}

fn call_list_with(rq: &Req, f: &Filter, oob: bool) -> Result<Markup, Fail> {
    let rows: Vec<Value> = calls_list(rq.conn, f.caller.as_deref(), 200)?.into_iter().filter(|r| !f.hide || r["cached"] != true).collect();
    Ok(html! {
        ul id="call-list" hx-swap-oob=[oob.then_some("true")] {
            li class="call head" {
                @for c in ["Tool", "Questions", "Status", "Tokens"] { div class="col" { (c) } }
            }
            @for r in &rows { (call_item(rq, r, f.open)) }
        }
    })
}

const PREVIEW_LABELS: usize = 4;

/// Line one says which checks ran, line two what text was checked.
fn call_preview(r: &Value) -> (String, String) {
    let shape = &r["shape"];
    let labels: Vec<&str> = shape["labels"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let snippet = shape["snippet"].as_str().unwrap_or("");
    if labels.is_empty() {
        return (r["path"].as_str().unwrap_or("").to_string(), snippet.to_string());
    }
    let subjects = shape["subjects"].as_i64().unwrap_or(0);
    let many = subjects > 1;
    let count = if many { format!("{subjects} items · ") } else { String::new() };
    // With several items the snippet is one of them, so it says which, and how many it hides.
    let sub = match shape["firstSubject"].as_str() {
        Some(first) if many && !snippet.is_empty() => format!("{first}: {snippet} · +{} more", subjects - 1),
        _ => snippet.to_string(),
    };
    let more = if labels.len() > PREVIEW_LABELS { format!(" +{}", labels.len() - PREVIEW_LABELS) } else { String::new() };
    (format!("{count}{}{more}", labels[..labels.len().min(PREVIEW_LABELS)].join(", ")), sub)
}

fn call_item(rq: &Req, r: &Value, open: Option<i64>) -> Markup {
    let id = r["id"].as_i64().unwrap_or(0);
    let ts = r["ts"].as_str().unwrap_or("");
    let (main, sub) = call_preview(r);
    let status = r["status"].as_i64().unwrap_or(0);
    let ok = (200..300).contains(&status);
    let upstream = r["upstream"].as_str().unwrap_or("typesafe");
    let cached = r["cached"] == true;
    html! {
        li class=(if open == Some(id) { "call sel" } else { "call" }) data-id=(id)
            hx-get={"/ui/call/" (id)} hx-target="#drawer" hx-swap="outerHTML" {
            div {
                div class="who" { (r["caller"].as_str().unwrap_or("")) }
                div class="when" { time data-ts=(ts) { (ts.get(11..19).unwrap_or(ts)) } }
            }
            div class="prev" {
                div class="prev-main" { (main) }
                @if !sub.is_empty() { div class="prev-sub" { (sub) } }
            }
            div class="status" {
                span class=(if upstream == "second" { "pill second" } else { "pill jev" }) { (upstream_word(rq, upstream)) }
                @for s in r["shadows"].as_array().into_iter().flatten() {
                    @let provider = s["provider"].as_str().unwrap_or("");
                    span class={"pill shadow " (if provider == "second" { "second" } else { "jev" })}
                        title=(match s["status"].as_i64() { Some(st) => format!("{} · {st}", s["state"].as_str().unwrap_or("")), None => s["state"].as_str().unwrap_or("").to_string() }) {
                        "+" (upstream_word(rq, provider))
                    }
                }
                span class=(if cached { "pill hit" } else { "pill miss" }) { (if cached { "HIT" } else { "MISS" }) }
                span class=(if ok { "code ok" } else { "code err" }) { (if status == 0 { "UNREACHABLE".to_string() } else { status.to_string() }) }
            }
            div class="rt" {
                span { (format_number(r["inputTokens"].as_i64())) "↑ " (format_number(r["outputTokens"].as_i64())) "↓" }
                span { (r["elapsedMs"].as_i64().map(|m| m.to_string()).unwrap_or_else(|| "null".into())) " ms" }
            }
        }
    }
}
