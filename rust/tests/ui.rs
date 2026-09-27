//! The server-rendered pages and their HTMX fragments.
mod common;
use common::*;
use serde_json::{json, Value};

fn html(env: &Env, method: &str, path: &str, form: &str) -> (u16, String, Vec<(String, String)>) {
    let headers = vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())];
    let r = jev_proxy::routes::handle(&env.app, method, path, &headers, form.as_bytes());
    (r.status, String::from_utf8_lossy(&r.body).to_string(), r.headers)
}

fn seed(env: &Env) -> i64 {
    env.upstream.reply(200, OK);
    call_id(&env.post_h("/v1/systemone", &[("X-Jev-Caller", "<b>john</b>")], json!({"state": {"item_1": "hi <script>"}, "questions": {"item_1_q": {"type": "noul"}}})))
}

#[test]
fn the_three_pages_render() {
    let env = Env::new();
    seed(&env);
    for (path, marker) in [("/", "id=\"call-list\""), ("/requests", "id=\"request-bar\""), ("/compare", "id=\"compare-summary\"")] {
        let (status, body, _) = html(&env, "GET", path, "");
        assert_eq!(200, status, "{path}");
        assert!(body.contains(marker), "{path} lacks {marker}");
        assert!(body.contains("/static/htmx.min.js"));
    }
}

#[test]
fn recorded_text_is_escaped() {
    let env = Env::new();
    let id = seed(&env);
    let (_, list, _) = html(&env, "GET", "/", "");
    assert!(list.contains("&lt;b&gt;john&lt;/b&gt;") && !list.contains("<b>john</b>"));
    let (_, drawer, _) = html(&env, "GET", &format!("/ui/call/{id}"), "");
    assert!(drawer.contains("hi &lt;script&gt;") && !drawer.contains("hi <script>"));
    let (_, json_tab, _) = html(&env, "GET", &format!("/ui/call/{id}?tab=json"), "");
    assert!(!json_tab.contains("<script>"));
}

#[test]
fn the_caller_filter_narrows_the_list() {
    let env = Env::new();
    seed(&env);
    env.upstream.reply(200, OK);
    env.post_h("/v1/systemone", &[("X-Jev-Caller", "ui")], json!({"state": "other"}));
    let (_, body, _) = html(&env, "GET", "/ui/traffic?caller=ui&hide=0", "");
    assert_eq!(1, body.matches("class=\"who\"").count());
}

#[test]
fn engine_toggles_refuse_to_disable_the_last_engine() {
    let env = Env::new();
    let (_, _, headers) = html(&env, "POST", "/ui/engines/toggle/typesafe", "");
    assert!(headers.iter().any(|(k, v)| k == "HX-Trigger" && v.contains("Engines not saved")));
    html(&env, "POST", "/ui/engines/toggle/second", "");
    html(&env, "POST", "/ui/engines/serve/second", "");
    assert_eq!(json!({"engines": ["typesafe", "second"], "served": "second"}), {
        let v = env.get("/api/settings").1;
        json!({"engines": v["engines"], "served": v["served"]})
    });
}

#[test]
fn a_vote_is_saved_and_cleared_through_the_fragment() {
    let env = Env::new();
    set_both(&env);
    env.upstream.reply(200, r#"{"answers":{"item_1_q":{"type":"noul","noul":0.9}}}"#);
    let id = call_id(&env.post("/v1/systemone", json!({"state": {"item_1": "hi"}, "questions": {"item_1_q": {"type": "noul"}}})));
    let _ = env.shadow(id, "second", Some("done"));
    let (_, body, _) = html(&env, "POST", "/ui/vote", &format!("callId={id}&questionId=item_1_q&verdict=tie&label="));
    assert!(body.contains("small on") && body.contains("id=\"compare-summary\""));
    assert_eq!("tie", env.get("/api/shadow-compare").1["rows"][0]["verdict"]);
    html(&env, "POST", "/ui/vote", &format!("callId={id}&questionId=item_1_q&verdict=&label="));
    assert!(env.get("/api/shadow-compare").1["rows"][0]["verdict"].is_null());
}

fn set_both(env: &Env) {
    env.second.reply(200, SECOND_OK_ITEM);
    jev_proxy::db::save_settings(&env.db(), &json!(["typesafe", "second"]), &json!("typesafe")).unwrap();
}
const SECOND_OK_ITEM: &str = r#"{"model":"second","answers":{"item_1_q":{"type":"noul","noul":0.1}}}"#;

#[test]
fn the_editor_adds_items_and_keeps_the_json_in_step() {
    let env = Env::new();
    let (_, _, h) = html(&env, "POST", "/ui/requests/new", "");
    let id: i64 = h.iter().find(|(k, _)| k == "HX-Redirect").unwrap().1.rsplit('=').next().unwrap().parse().unwrap();
    let form = "name=R&model=jev-latest&override=0&tab=body&item_key=item_1&item_text=hello&q_suffix=is_kiss&q_instructions=Short+%7Bref%7D%3F&q_true=y&q_false=n";
    let (_, body, _) = html(&env, "POST", &format!("/ui/requests/{id}/edit?op=add-item"), form);
    assert_eq!(2, body.matches("name=\"item_key\"").count());
    assert!(body.contains("item_2_is_kiss") && body.contains("Short `item_1`?"));
    html(&env, "POST", &format!("/ui/requests/{id}/save"), form);
    let saved = env.get(&format!("/api/requests/{id}")).1;
    assert_eq!(json!([{"key": "item_1", "text": "hello"}]), saved["items"]);
    assert!(saved["rawJsonOverride"].is_null());
}

#[test]
fn send_renders_the_matrix_and_records_a_ui_call() {
    let env = Env::new();
    let (_, _, h) = html(&env, "POST", "/ui/requests/new", "");
    let id = h.iter().find(|(k, _)| k == "HX-Redirect").unwrap().1.rsplit('=').next().unwrap().to_string();
    env.upstream.reply(200, r#"{"answers":{"item_1_is_kiss":{"type":"noul","noul":0.2}}}"#);
    let form = "name=R&model=jev-latest&override=0&tab=body&item_key=item_1&item_text=hi&q_suffix=is_kiss&q_instructions=x&q_true=&q_false=";
    let (_, body, _) = html(&env, "POST", &format!("/ui/requests/{id}/send"), form);
    assert!(body.contains("cell crit") && body.contains("0.20"));
    assert_eq!("ui", env.get("/api/calls").1[0]["caller"]);
}

#[test]
fn an_invalid_json_override_is_refused_without_a_call() {
    let env = Env::new();
    let (_, _, h) = html(&env, "POST", "/ui/requests/new", "");
    let id = h.iter().find(|(k, _)| k == "HX-Redirect").unwrap().1.rsplit('=').next().unwrap().to_string();
    let (_, _, headers) = html(&env, "POST", &format!("/ui/requests/{id}/send"), "name=R&override=1&json=%7Bnope");
    assert!(headers.iter().any(|(k, v)| k == "HX-Trigger" && v.contains("Invalid JSON")));
    assert!(env.upstream.bodies().is_empty());
    let _: Value = Value::Null;
}

#[test]
fn an_unset_second_engine_is_greyed_and_can_only_be_switched_off() {
    let env = Env::with(|c| c.second_base_url = String::new());
    let (_, page, _) = html(&env, "GET", "/", "");
    assert!(page.contains("chip unset"), "the second chip is shown greyed");
    let (_, _, headers) = html(&env, "POST", "/ui/engines/toggle/second", "");
    assert!(headers.iter().any(|(k, v)| k == "HX-Trigger" && v.contains("SECOND_BASE_URL")));
    assert_eq!(json!(["typesafe"]), env.get("/api/settings").1["engines"]);
    // Already on, as a stale setting can leave it: it can still be switched off.
    jev_proxy::db::save_settings(&env.db(), &json!(["typesafe", "second"]), &json!("typesafe")).unwrap();
    html(&env, "POST", "/ui/engines/toggle/second", "");
    assert_eq!(json!(["typesafe"]), env.get("/api/settings").1["engines"]);
    let (_, _, headers) = html(&env, "POST", "/ui/engines/serve/second", "");
    assert!(headers.iter().any(|(k, v)| k == "HX-Trigger" && v.contains("SECOND_BASE_URL")));
}
