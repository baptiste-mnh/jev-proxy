//! Server-rendered UI: full pages under `/`, `/requests`, `/compare`, HTMX fragments under `/ui/`.
mod answers;
mod compare_view;
mod drawer;
mod requests;
mod traffic;

use crate::db::{get_settings, Settings};
use crate::routes::{App, Fail, Reply};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use rusqlite::Connection;
use serde_json::{json, Value};

pub struct Req<'a> {
    pub app: &'a App,
    pub conn: &'a Connection,
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a [(String, String)],
    /// The urlencoded form body, in order: repeated names keep their order.
    pub form: Vec<(String, String)>,
}

impl Req<'_> {
    pub fn q(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    pub fn f(&self, name: &str) -> Option<&str> {
        self.form.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.form.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect()
    }
    pub fn second_label(&self) -> &str {
        &self.app.config.second_label
    }
}

pub type Out = Result<Option<Reply>, Fail>;

pub fn html_reply(m: Markup) -> Reply {
    Reply { status: 200, headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())], body: m.into_string().into_bytes() }
}

/// An HTMX response header that raises a `toast` event the page turns into a tile.
pub fn toast_header(title: &str, message: &str, error: bool) -> (String, String) {
    ("HX-Trigger".into(), json!({"toast": {"title": title, "message": message, "level": if error { "error" } else { "info" }}}).to_string())
}

pub fn with_header(mut r: Reply, h: (String, String)) -> Reply {
    r.headers.push(h);
    r
}

pub fn handle(rq: &Req) -> Out {
    let method = rq.method;
    match (method, rq.path) {
        ("GET", "/") => Ok(Some(html_reply(page(rq, "traffic", traffic::page(rq)?)?))),
        ("GET", "/requests") => Ok(Some(html_reply(page(rq, "requests", requests::page(rq)?)?))),
        ("GET", "/compare") => Ok(Some(html_reply(page(rq, "compare", compare_view::page(rq)?)?))),
        ("GET", "/favicon.ico") => Ok(Some(Reply { status: 204, headers: vec![], body: vec![] })),
        (_, p) if p.starts_with("/ui/") => {
            let r = traffic::route(rq)?;
            let r = match r { Some(r) => Some(r), None => drawer::route(rq)? };
            let r = match r { Some(r) => Some(r), None => compare_view::route(rq)? };
            let r = match r { Some(r) => Some(r), None => requests::route(rq)? };
            let r = match r { Some(r) => Some(r), None => route(rq)? };
            Ok(r)
        }
        _ => Ok(None),
    }
}

/// Engine switches and the second-upstream status: the top bar.
fn route(rq: &Req) -> Out {
    if rq.method == "POST" {
        if let Some(p) = rq.path.strip_prefix("/ui/engines/toggle/") {
            let s = get_settings(rq.conn)?;
            // An engine with no upstream can be switched off, never on.
            if !configured(rq, p) && !s.engines.iter().any(|e| e == p) {
                return refuse(rq, &s, "SECOND_BASE_URL is not set, so this engine cannot run.");
            }
            let engines: Vec<String> = if s.engines.iter().any(|e| e == p) {
                s.engines.iter().filter(|e| *e != p).cloned().collect()
            } else {
                s.engines.iter().cloned().chain([p.to_string()]).collect()
            };
            let served = if engines.contains(&s.served) { s.served.clone() } else { engines.first().cloned().unwrap_or_default() };
            return save_and_render(rq, engines, served);
        }
        if let Some(p) = rq.path.strip_prefix("/ui/engines/serve/") {
            let s = get_settings(rq.conn)?;
            if !configured(rq, p) {
                return refuse(rq, &s, "SECOND_BASE_URL is not set, so this engine cannot answer.");
            }
            return save_and_render(rq, s.engines, p.to_string());
        }
    }
    if rq.method == "GET" && rq.path == "/ui/second-status" {
        return Ok(Some(html_reply(second_status(rq))));
    }
    Ok(None)
}

fn save_and_render(rq: &Req, engines: Vec<String>, served: String) -> Out {
    let saved = crate::db::save_settings(rq.conn, &json!(engines), &json!(served));
    let (settings, header) = match saved {
        Ok(s) => (s, None),
        // The server validates the setting, so a refused change shows its reason and the stored setting.
        Err(m) => (get_settings(rq.conn)?, Some(toast_header("Engines not saved", &m, true))),
    };
    let r = html_reply(engines_bar(rq, &settings));
    Ok(Some(match header { Some(h) => with_header(r, h), None => r }))
}

/// Whether an engine has an upstream to call. TypeSafe always has one.
fn configured(rq: &Req, provider: &str) -> bool {
    provider != "second" || !rq.app.config.second_base_url.is_empty()
}

fn refuse(rq: &Req, settings: &Settings, message: &str) -> Out {
    Ok(Some(with_header(html_reply(engines_bar(rq, settings)), toast_header("Engines not saved", message, true))))
}

pub fn providers(rq: &Req) -> Vec<&'static str> {
    crate::db::PROVIDERS.iter().copied().filter(|p| *p != "second" || !rq.app.config.second_base_url.is_empty()).collect()
}

pub fn upstream_word(rq: &Req, upstream: &str) -> String {
    if upstream == "second" { rq.second_label().to_uppercase() } else { "JEV".into() }
}

fn engines_bar(rq: &Req, s: &Settings) -> Markup {
    let key = crate::routes::mask_key(&rq.app.config.api_key);
    html! {
        div id="engines" {
            span class="lab" { "Engines" }
            span id="engine-chips" {
                @for p in crate::db::PROVIDERS {
                    @let on = s.engines.iter().any(|e| e == p);
                    @let ready = configured(rq, p);
                    @let title = match (ready, on) {
                        (false, true) => "SECOND_BASE_URL is not set: every call on it fails. Click to disable.",
                        (false, false) => "SECOND_BASE_URL is not set. Set it and restart to use this engine.",
                        (true, true) => "Enabled: every call runs on it. Click to disable.",
                        (true, false) => "Disabled. Click to run every call on it too.",
                    };
                    span class={"chip" (if on { " on" } else { "" }) (if ready { "" } else { " unset" })}
                        role="checkbox" aria-checked=(on.to_string()) aria-disabled=(if ready || on { "false" } else { "true" }) title=(title)
                        hx-post={"/ui/engines/toggle/" (p)} hx-target="#engines" hx-swap="outerHTML" {
                        (upstream_word(rq, p))
                        @if p == "typesafe" {
                            span class="billed" title="TypeSafe bills every call it answers, served or shadow." { "billed" }
                        }
                    }
                }
            }
            span class="lab" { "Serve" }
            span id="serve-seg" class="seg" {
                @for p in &s.engines {
                    @let on = *p == s.served;
                    button class={(if on { "on" } else { "" }) (if configured(rq, p) { "" } else { " unset" })}
                        disabled[!configured(rq, p) && !on] role="radio" aria-checked=(on.to_string())
                        title=(if on { "Served: the caller gets this answer." } else { "Click to serve this engine's answer." })
                        hx-post={"/ui/engines/serve/" (p)} hx-target="#engines" hx-swap="outerHTML" { (upstream_word(rq, p)) }
                }
            }
            (second_status(rq))
            span class="lab" { "Key" }
            code id="api-key" title="TYPESAFE_API_KEY loaded at server start" { (if key.is_empty() { "missing".to_string() } else { key }) }
        }
    }
}

/// A second upstream can load its models after it starts, so the top bar says whether it can answer yet.
fn second_status(rq: &Req) -> Markup {
    let c = &rq.app.config;
    if c.second_base_url.is_empty() {
        return html! { span id="second-status" class="second-status" hidden hx-get="/ui/second-status" hx-trigger="every 10s" hx-swap="outerHTML" {} };
    }
    let s = crate::proxy::second_health(c);
    let up = s["up"].as_bool().unwrap_or(false);
    let pending: i64 = rq.conn.query_row("SELECT COUNT(*) FROM shadows WHERE provider = 'second' AND state = 'pending'", [], |r| r.get(0)).unwrap_or(0);
    let pending_txt = if pending > 0 { format!(" · {pending} pending") } else { String::new() };
    let title = if up {
        let loaded: Vec<&str> = s["loaded"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        format!("{} · loaded: {}", c.second_base_url, if loaded.is_empty() { "n/a".to_string() } else { loaded.join(", ") })
    } else {
        format!("{} unreachable: {}", c.second_base_url, s["error"].as_str().unwrap_or(""))
    };
    html! {
        span id="second-status" class={"second-status " (if up { "up" } else { "down" })} title=(title)
            hx-get="/ui/second-status" hx-trigger="every 10s" hx-swap="outerHTML" {
            span class="dot" {}
            (format!("{} {}{}", c.second_label, if up { "up" } else { "down" }, pending_txt))
        }
    }
}

fn page(rq: &Req, active: &str, content: Markup) -> Result<Markup, Fail> {
    let settings = get_settings(rq.conn)?;
    let nav = |id: &str, href: &str, label: &str| html! {
        a id=(id) class=(if id.ends_with(active) { "on" } else { "" }) href=(href) { (label) }
    };
    Ok(html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                title { "JEV Proxy" }
                link rel="stylesheet" href="/static/style.css";
                script src="/static/htmx.min.js" {}
                script src="/static/ui.js" defer {}
            }
            body hx-boost="true" {
                header id="topbar" {
                    span id="brand" { span class="brand-mark" { "JEV" } " Proxy" }
                    nav {
                        (nav("view-traffic", "/", "Traffic"))
                        (nav("view-requests", "/requests", "Requests"))
                        (nav("view-compare", "/compare", "Shadow Compare"))
                    }
                    span id="window-label" { "last 7 days" }
                    (engines_bar(rq, &settings))
                }
                (content)
                div id="toasts" aria-live="polite" {}
            }
        }
    })
}

pub fn raw(s: &str) -> PreEscaped<String> {
    PreEscaped(s.to_string())
}
