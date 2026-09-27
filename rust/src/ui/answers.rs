//! Answer rendering shared by the Traffic drawer and the Requests output.
use super::{upstream_word, Req};
use maud::{html, Markup, PreEscaped};
use serde_json::Value;

/// Groups digits with a plain space so the value stays locale-independent.
pub fn format_number(n: Option<i64>) -> String {
    let Some(n) = n else { return "—".into() };
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

pub fn fixed(v: f64, digits: usize) -> String {
    format!("{v:.digits$}")
}

pub struct Cell {
    pub text: String,
    pub ratio: Option<f64>,
    pub flagged: bool,
}

/// One answer as display text plus an optional 0..1 fill ratio. Only `noul` flags red: 0.5 is a
/// real threshold there, and means nothing on a `choice` confidence or a `score` value.
pub fn render_cell(a: &Value) -> Cell {
    let f = |k: &str| a[k].as_f64().unwrap_or(f64::NAN);
    match a["type"].as_str() {
        Some("noul") => Cell { text: fixed(f("noul"), 2), ratio: Some(f("noul")), flagged: f("noul") < 0.5 },
        Some("choice") => Cell {
            text: format!("{} ({})", a["choice"].as_str().unwrap_or(""), fixed(f("confidence"), 2)),
            ratio: Some(f("confidence")),
            flagged: false,
        },
        Some("score") => {
            let level = a["legend"][format!("{}", f("score").round() as i64)].as_str().unwrap_or("");
            let count = a["legend"].as_object().map_or(0, |l| l.len()).max(1);
            let confidence = if a["confidence"].is_number() { format!(" · confidence {}", fixed(f("confidence"), 2)) } else { String::new() };
            Cell { text: format!("{} {level}{confidence}", fixed(f("score"), 2)), ratio: Some(f("score") / ((count as f64) - 1.0).max(1.0)), flagged: false }
        }
        _ if a.is_null() => Cell { text: String::new(), ratio: None, flagged: false },
        _ => Cell { text: a.to_string(), ratio: None, flagged: false },
    }
}

pub fn status_word(status: Option<i64>) -> String {
    match status {
        Some(0) => "UNREACHABLE".into(),
        Some(s) => s.to_string(),
        None => "undefined".into(),
    }
}

/// `meta`: status, elapsedMs, inputTokens, outputTokens, cached, upstream, attempts.
pub fn meta_line(rq: &Req, meta: &Value) -> String {
    let elapsed = meta["elapsedMs"].as_f64().unwrap_or(0.0) / 1000.0;
    let mut parts = vec![
        status_word(meta["status"].as_i64()),
        format!("{} s", fixed(elapsed, 1)),
        format!("{} in / {} out", format_number(meta["inputTokens"].as_i64()), format_number(meta["outputTokens"].as_i64())),
        if meta["cached"].as_bool().unwrap_or(false) { "HIT".into() } else { "MISS".into() },
    ];
    if let Some(u) = meta["upstream"].as_str().filter(|u| !u.is_empty()) {
        parts.insert(3, upstream_word(rq, u));
    }
    let attempts = meta["attempts"].as_i64().unwrap_or(1);
    if attempts > 1 {
        parts.push(format!("{attempts} attempts"));
    }
    parts.join(" · ")
}

/// One square of the NO-to-YES greyscale: light for no, dark for yes.
pub fn swatch(noul: f64) -> Markup {
    let l = (92.0 - noul.clamp(0.0, 1.0) * 80.0).round();
    html! { span class="swatch" style={"background:hsl(0 0% " (l) "%)"} title={(fixed(noul, 2)) " on the NO to YES scale"} {} }
}

/// The model that answered, plus the checkpoint its router picked, when the upstream reports one.
pub fn answered_by(body: &Value) -> String {
    let Some(model) = body["model"].as_str().filter(|m| !m.is_empty()) else { return String::new() };
    match body["routing"]["model"].as_str().filter(|m| !m.is_empty()) {
        Some(c) => format!("{model} · {c}"),
        None => model.to_string(),
    }
}

pub fn answer_row(id: &str, answer: &Value, model: &str, shadow_note: Option<&str>) -> Markup {
    let cell = render_cell(answer);
    let label = match answer["type"].as_str() {
        Some(t) => format!("{id} · {t}"),
        None => id.to_string(),
    };
    let ratio = cell.ratio.filter(|r| !r.is_nan());
    html! {
        div class=(if shadow_note.is_some() { "ans shadow" } else { "ans" }) {
            div class="q" { (label) }
            div class=(if cell.flagged { "v flag" } else { "v" }) {
                @if answer["type"] == "noul" { (swatch(answer["noul"].as_f64().unwrap_or(0.0))) }
                @if let (Some(note), true) = (shadow_note, answer.is_null()) { (note) } @else { (cell.text) }
                @if !model.is_empty() && !answer.is_null() { div class="model" { "(" (model) ")" } }
            }
            @if let Some(r) = ratio {
                div class=(if cell.flagged { "bar flag" } else { "bar" }) style={"width:" (r.clamp(0.0, 1.0) * 100.0) "%"} {}
            }
        }
    }
}

/// Pretty JSON with one span per token, built from the value itself, so nothing is re-parsed.
pub fn highlight_json(v: &Value) -> Markup {
    let mut out = String::new();
    write_value(&mut out, v, 0);
    PreEscaped(out)
}

fn write_value(out: &mut String, v: &Value, depth: usize) {
    match v {
        Value::Null => span(out, "null", "null"),
        Value::Bool(b) => span(out, "bool", &b.to_string()),
        Value::Number(n) => span(out, "num", &n.to_string()),
        Value::String(_) => span(out, "str", &v.to_string()),
        Value::Array(items) => write_list(out, depth, ('[', ']'), items.iter().map(|item| (None, item))),
        Value::Object(map) => write_list(out, depth, ('{', '}'), map.iter().map(|(k, item)| (Some(k), item))),
    }
}

/// An array or an object: one entry per line, indented two spaces per level, like `to_string_pretty`.
fn write_list<'a>(out: &mut String, depth: usize, (open, close): (char, char), entries: impl ExactSizeIterator<Item = (Option<&'a String>, &'a Value)>) {
    out.push(open);
    if entries.len() == 0 {
        out.push(close);
        return;
    }
    for (i, (key, value)) in entries.enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('\n');
        out.push_str(&"  ".repeat(depth + 1));
        if let Some(key) = key {
            span(out, "key", &Value::from(key.as_str()).to_string());
            out.push_str(": ");
        }
        write_value(out, value, depth + 1);
    }
    out.push('\n');
    out.push_str(&"  ".repeat(depth));
    out.push(close);
}

fn span(out: &mut String, class: &str, text: &str) {
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    out.push_str(&format!("<span class=\"j-{class}\">{escaped}</span>"));
}

#[cfg(test)]
mod tests {
    use super::highlight_json;
    use serde_json::json;

    fn strip_tags(html: &str) -> String {
        let mut text = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => text.push(c),
                _ => {}
            }
        }
        text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
    }

    #[test]
    fn reads_like_to_string_pretty_once_the_tags_are_gone() {
        let v = json!({"a": [1, -2.5, true, null, {}], "b": {"c": "x \"<b>\" & y"}, "d": []});
        assert_eq!(serde_json::to_string_pretty(&v).unwrap(), strip_tags(&highlight_json(&v).into_string()));
    }

    #[test]
    fn escapes_markup_and_tags_each_token() {
        let html = highlight_json(&json!({"k": "<script>"})).into_string();
        assert!(!html.contains("<script>"));
        assert!(html.contains(r#"<span class="j-key">"k"</span>: <span class="j-str">"&lt;script&gt;"</span>"#));
    }
}

pub fn parse(text: Option<&str>) -> Option<Value> {
    serde_json::from_str(text.filter(|t| !t.is_empty())?).ok()
}
