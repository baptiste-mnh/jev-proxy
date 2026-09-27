//! The proxy routes, upstream retry, call recording and the calls API, against a stub upstream.
mod common;
use common::*;
use jev_proxy::proxy::{execute_call, upstream_call};
use serde_json::{json, Value};
use std::cell::RefCell;

const BODY: &[u8] = br#"{"state":"hi","model":"jev-latest","questions":{}}"#;

fn hi() -> Value {
    json!({"state": "hi"})
}

// ---- upstream_call ----

fn call(env: &Env, body: &[u8]) -> (jev_proxy::proxy::UpstreamResult, Vec<u64>) {
    let slept = RefCell::new(vec![]);
    let r = upstream_call(&env.upstream.url, "/v1/systemone", body, "test-key", |s| slept.borrow_mut().push(s));
    (r, slept.into_inner())
}

#[test]
fn upstream_success_on_the_first_attempt() {
    let env = Env::new();
    env.upstream.reply(200, r#"{"model":"jev-1.13.0","answers":{}}"#);
    let (r, slept) = call(&env, BODY);
    assert_eq!((200, 1), (r.status, r.attempts));
    assert!(slept.is_empty());
}

#[test]
fn upstream_injects_the_bearer_key_and_forwards_the_body_unchanged() {
    let env = Env::new();
    env.upstream.reply(200, "{}");
    call(&env, br#"{"state":["a","b"]}"#);
    assert_eq!(vec![Some("Bearer test-key".to_string())], env.upstream.auth());
    assert_eq!(vec![br#"{"state":["a","b"]}"#.to_vec()], env.upstream.bodies());
}

#[test]
fn upstream_422_returns_at_once() {
    let env = Env::new();
    env.upstream.reply(422, r#"{"error":"bad field"}"#);
    let (r, _) = call(&env, BODY);
    assert_eq!((422, 1), (r.status, r.attempts));
    assert!(r.body.contains("bad field"));
}

#[test]
fn upstream_429_is_retried_twice_then_returned() {
    let env = Env::new();
    for _ in 0..3 {
        env.upstream.reply(429, "{}");
    }
    let (r, slept) = call(&env, BODY);
    assert_eq!((429, 3), (r.status, r.attempts));
    assert_eq!(vec![1, 2], slept);
}

#[test]
fn upstream_529_then_success() {
    let env = Env::new();
    env.upstream.reply(529, "{}");
    env.upstream.reply(200, r#"{"answers":{}}"#);
    let (r, _) = call(&env, BODY);
    assert_eq!((200, 2), (r.status, r.attempts));
}

#[test]
fn upstream_retry_after_overrides_the_backoff() {
    let env = Env::new();
    env.upstream.reply_retry_after(429, "{}", 7);
    env.upstream.reply(200, "{}");
    assert_eq!(vec![7], call(&env, BODY).1);
}

#[test]
fn upstream_unreachable_host_reports_an_error() {
    let r = upstream_call("http://127.0.0.1:1", "/v1/systemone", b"{}", "k", |_| {});
    assert_eq!(0, r.status);
    assert!(r.error.is_some());
}

// ---- execute_call ----

fn run(env: &Env, body: &[u8], bypass: bool) -> jev_proxy::proxy::CallResult {
    execute_call(&env.db(), "/v1/systemone", body, "john", &env.app.config, "typesafe", bypass).unwrap()
}

fn row(env: &Env, sql: &str) -> Vec<Value> {
    let conn = env.db();
    let mut stmt = conn.prepare(sql).unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| Ok((0..n).map(|i| match r.get_ref(i).unwrap() {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(i) => json!(i),
        rusqlite::types::ValueRef::Real(f) => json!(f),
        rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        rusqlite::types::ValueRef::Blob(_) => Value::Null,
    }).collect::<Vec<_>>())).unwrap().map(|r| json!(r.unwrap())).collect()
}

#[test]
fn execute_records_a_successful_call_with_its_usage() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    let result = run(&env, BODY, false);
    assert_eq!(200, result.status);
    assert!(!result.cached);
    let r = &row(&env, "SELECT caller, model, input_tokens, output_tokens, attempts, id FROM calls")[0];
    assert_eq!(&json!(["john", "jev-latest", 1, 2, 1, result.call_id]), r);
}

#[test]
fn execute_serves_a_second_identical_call_from_the_cache_and_flags_the_row() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    run(&env, BODY, false);
    let second = run(&env, BODY, false); // no second reply queued: a miss would be a 500
    assert!(second.cached);
    assert_eq!(serde_json::from_str::<Value>(OK).unwrap(), serde_json::from_str::<Value>(&second.body).unwrap());
    assert_eq!(json!([[0], [1]]), json!(row(&env, "SELECT cached FROM calls ORDER BY id")));
    assert_eq!(json!([[1]]), json!(row(&env, "SELECT attempts FROM calls WHERE id = 2")));
    assert_eq!(1, second.attempts);
}

#[test]
fn execute_bypass_cache_goes_upstream_and_refreshes() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    env.upstream.reply(200, OK);
    run(&env, BODY, false);
    assert!(!run(&env, BODY, true).cached);
    assert_eq!(2, env.upstream.bodies().len());
}

#[test]
fn execute_records_an_error_and_does_not_cache_it() {
    let env = Env::new();
    env.upstream.reply(422, r#"{"error":"bad"}"#);
    env.upstream.reply(422, r#"{"error":"bad"}"#);
    assert_eq!(422, run(&env, BODY, false).status);
    assert!(!run(&env, BODY, false).cached);
    assert_eq!(2, env.upstream.bodies().len());
}

#[test]
fn execute_records_an_unreachable_upstream_with_status_zero() {
    let env = Env::with(|c| c.base_url = "http://127.0.0.1:1".into());
    let result = run(&env, BODY, false);
    assert_eq!(0, result.status);
    assert!(!row(&env, "SELECT error FROM calls")[0][0].is_null());
}

#[test]
fn execute_distinct_malformed_bodies_hash_differently_and_do_not_share_the_cache() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    env.upstream.reply(200, OK);
    run(&env, b"not json at all", false);
    assert!(!run(&env, b"<html>error</html>", false).cached);
    let hashes = row(&env, "SELECT body_hash FROM calls ORDER BY id");
    assert_ne!(hashes[0], hashes[1]);
}

// ---- routes ----

#[test]
fn route_health() {
    let env = Env::new();
    assert_eq!((200, json!({"ok": true})), env.get("/api/health"));
}

#[test]
fn route_passthrough_returns_the_bare_body_with_correlation_headers() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    let r = env.post("/v1/systemone", hi());
    assert_eq!(200, r.status);
    assert!(body_json(&r).get("answers").is_some()); // no envelope
    assert_eq!("MISS", header(&r, "X-Jev-Cache"));
    assert_eq!("1", header(&r, "X-Jev-Attempts"));
    assert!(!header(&r, "X-Jev-Call-Id").is_empty());
}

#[test]
fn route_caller_header_is_recorded_and_defaults_to_unknown() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    env.upstream.reply(200, OK);
    env.post_h("/v1/systemone", &[("X-Jev-Caller", "john")], hi());
    env.post_h("/v1/systemone", &[], json!({"state": "other"}));
    assert_eq!(json!([["john"], ["unknown"]]), json!(row(&env, "SELECT caller FROM calls ORDER BY id")));
}

#[test]
fn route_upstream_422_is_mirrored() {
    let env = Env::new();
    env.upstream.reply(422, r#"{"error":"missing questions"}"#);
    let r = env.post("/v1/systemone", hi());
    assert_eq!(422, r.status);
    assert!(String::from_utf8_lossy(&r.body).contains("missing questions"));
}

#[test]
fn route_second_identical_call_reports_a_hit_and_the_bypass_header_a_miss() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    env.post("/v1/systemone", hi());
    assert_eq!("HIT", header(&env.post("/v1/systemone", hi()), "X-Jev-Cache"));
    env.upstream.reply(200, OK);
    assert_eq!("MISS", header(&env.post_h("/v1/systemone", &[("X-Jev-Cache", "0")], hi()), "X-Jev-Cache"));
}

#[test]
fn route_api_send_is_gone() {
    assert_eq!(404, Env::new().post("/api/send", json!({"json": {"state": "hi"}})).status);
}

#[test]
fn route_unreachable_upstream_carries_correlation_headers_and_matches_the_row() {
    let env = Env::with(|c| c.base_url = "http://127.0.0.1:1".into());
    let r = env.post("/v1/systemone", hi());
    assert_eq!(502, r.status);
    assert_eq!("1", header(&r, "X-Jev-Attempts"));
    assert_eq!("0", header(&r, "X-Jev-Upstream-Status"));
    assert_eq!(json!([[0]]), json!(row(&env, &format!("SELECT status FROM calls WHERE id = {}", call_id(&r)))));
}

#[test]
fn route_a_bad_json_body_is_a_400_not_a_crash() {
    let env = Env::new();
    let r = handle_raw(&env, "PUT", "/api/settings", b"{nope");
    assert_eq!(400, r.status);
}

fn handle_raw(env: &Env, method: &str, path: &str, body: &[u8]) -> jev_proxy::routes::Reply {
    jev_proxy::routes::handle(&env.app, method, path, &[], body)
}

#[test]
fn route_serves_static_files_and_refuses_a_path_outside() {
    let env = Env::new();
    std::fs::write(env.dir.join("index.html"), "<p>hi</p>").unwrap();
    std::fs::write(env.dir.join("app.js"), "1").unwrap();
    let r = env.request("GET", "/static/index.html", &[], &Value::Null);
    assert_eq!((200, "text/html"), (r.status, header(&r, "Content-Type").as_str()));
    assert_eq!("application/javascript", header(&env.request("GET", "/static/app.js", &[], &Value::Null), "Content-Type"));
    assert_eq!(404, env.request("GET", "/static/../../etc/passwd", &[], &Value::Null).status);
}

// ---- saved requests ----

#[test]
fn requests_can_be_created_updated_listed_and_deleted() {
    let env = Env::new();
    let created = body_json(&env.post("/api/requests", json!({"name": "mine"})));
    assert_eq!(("mine", "jev-latest", 3), (created["name"].as_str().unwrap(), created["model"].as_str().unwrap(), created["questions"].as_array().unwrap().len()));
    let id = created["id"].as_i64().unwrap();
    let updated = body_json(&env.request("PUT", &format!("/api/requests/{id}"), &[], &json!({"name": "renamed", "rawJsonOverride": "{}"})));
    assert_eq!(("renamed", Some("{}")), (updated["name"].as_str().unwrap(), updated["rawJsonOverride"].as_str()));
    let cleared = body_json(&env.request("PUT", &format!("/api/requests/{id}"), &[], &json!({"rawJsonOverride": null})));
    assert!(cleared["rawJsonOverride"].is_null());
    assert_eq!(1, env.get("/api/requests").1.as_array().unwrap().len());
    env.request("DELETE", &format!("/api/requests/{id}"), &[], &Value::Null);
    assert_eq!(0, env.get("/api/requests").1.as_array().unwrap().len());
    assert_eq!(404, env.request("PUT", "/api/requests/999", &[], &json!({})).status);
}

// ---- calls API ----

fn seed(env: &Env, caller: &str, state: &str) {
    env.upstream.reply(200, OK);
    env.post_h("/v1/systemone", &[("X-Jev-Caller", caller)], json!({"state": state, "model": "jev-latest", "questions": {"q": {"type": "noul", "instructions": "x?"}}}));
}

#[test]
fn calls_list_newest_first_without_the_bodies() {
    let env = Env::new();
    seed(&env, "john", "a");
    seed(&env, "ui", "b");
    let rows = env.get("/api/calls").1;
    assert_eq!(json!(["ui", "john"]), json!(rows.as_array().unwrap().iter().map(|r| r["caller"].clone()).collect::<Vec<_>>()));
    assert!(rows[0].get("requestBody").is_none() && rows[0].get("responseBody").is_none());
}

#[test]
fn calls_filter_by_caller() {
    let env = Env::new();
    seed(&env, "john", "a");
    seed(&env, "ui", "b");
    let rows = env.get("/api/calls?caller=john").1;
    assert_eq!(1, rows.as_array().unwrap().len());
}

#[test]
fn calls_detail_carries_both_bodies_and_404s() {
    let env = Env::new();
    seed(&env, "john", "a");
    let id = env.get("/api/calls").1[0]["id"].as_i64().unwrap();
    let detail = env.get(&format!("/api/calls/{id}")).1;
    assert!(serde_json::from_str::<Value>(detail["responseBody"].as_str().unwrap()).unwrap().get("answers").is_some());
    assert!(serde_json::from_str::<Value>(detail["requestBody"].as_str().unwrap()).unwrap().get("state").is_some());
    assert_eq!(404, env.get("/api/calls/99999").0);
}

#[test]
fn calls_unparseable_limit_falls_back_to_the_default() {
    let env = Env::new();
    seed(&env, "john", "a");
    let (status, rows) = env.get("/api/calls?limit=abc");
    assert_eq!((200, 1), (status, rows.as_array().unwrap().len()));
}

#[test]
fn stats_window_excludes_an_old_row() {
    let env = Env::new();
    seed(&env, "john", "a");
    env.db().execute("UPDATE calls SET ts = '2000-01-01T00:00:00Z'", []).unwrap();
    assert_eq!(0, env.get("/api/stats?days=7").1["calls"]);
}

#[test]
fn stats_saved_input_tokens_count_only_cached_rows() {
    let env = Env::new();
    seed(&env, "john", "a");
    seed(&env, "john", "a");
    let stats = env.get("/api/stats?days=7").1;
    assert_eq!((1, 1), (stats["cached"].as_i64().unwrap(), stats["savedInputTokens"].as_i64().unwrap()));
}

#[test]
fn stats_errors_count_status_0_and_422() {
    let env = Env::new();
    env.upstream.reply(422, r#"{"error":"bad"}"#);
    env.post("/v1/systemone", hi());
    env.db().execute(
        "INSERT INTO calls (ts, caller, path, model, request_body, status, body_hash) VALUES (?, 'john', '/v1/systemone', 'jev-latest', '{}', 0, 'x')",
        [jev_proxy::util::now_iso()],
    ).unwrap();
    assert_eq!(2, env.get("/api/stats?days=7").1["errors"]);
}

#[test]
fn stats_unparseable_or_negative_days_are_clamped() {
    let env = Env::new();
    seed(&env, "john", "a");
    let s = env.get("/api/stats?days=abc").1;
    assert_eq!((7, 1), (s["days"].as_i64().unwrap(), s["calls"].as_i64().unwrap()));
    let s = env.get("/api/stats?days=-1").1;
    assert_eq!((1, 1), (s["days"].as_i64().unwrap(), s["calls"].as_i64().unwrap()));
}

#[test]
fn promote_creates_an_editable_request_with_a_raw_override() {
    let env = Env::new();
    seed(&env, "john", "a");
    let id = env.get("/api/calls").1[0]["id"].as_i64().unwrap();
    let r = env.post(&format!("/api/calls/{id}/promote"), json!({}));
    assert_eq!(201, r.status);
    let created = body_json(&r);
    assert_eq!("jev-latest", created["model"]);
    assert!(!created["rawJsonOverride"].is_null());
}

#[test]
fn promote_recovers_items_and_questions_from_a_form_shaped_body() {
    let env = Env::new();
    env.upstream.reply(200, OK);
    env.post_h("/v1/systemone", &[("X-Jev-Caller", "john")], json!({
        "state": {"item_1": "hello"}, "model": "jev-latest",
        "questions": {"item_1_is_kiss": {"type": "noul", "instructions": "Is `item_1` short?", "criteria": {"true": "short", "false": "long"}}},
    }));
    let id = env.get("/api/calls").1[0]["id"].as_i64().unwrap();
    let created = body_json(&env.post(&format!("/api/calls/{id}/promote"), json!({})));
    assert_eq!(json!([{"key": "item_1", "text": "hello"}]), created["items"]);
    assert_eq!("is_kiss", created["questions"][0]["suffix"]);
    assert_eq!("Is {ref} short?", created["questions"][0]["instructions"]);
    assert!(created["rawJsonOverride"].is_null());
}

#[test]
fn promote_tolerates_a_non_json_response_body_and_404s_on_an_extra_segment() {
    let env = Env::new();
    env.db().execute(
        "INSERT INTO calls (ts, caller, path, model, request_body, response_body, status, elapsed_ms, body_hash) \
         VALUES (?, 'john', '/v1/systemone', 'jev-latest', '{\"state\":\"a\"}', '<html>not json</html>', 502, 12, 'x')",
        [jev_proxy::util::now_iso()],
    ).unwrap();
    let r = env.post("/api/calls/1/promote", json!({}));
    assert_eq!(201, r.status);
    assert!(body_json(&r)["lastResponse"]["body"].is_null());
    assert_eq!(404, env.post("/api/calls/1/2/promote", json!({})).status);
}

#[test]
fn route_serves_the_embedded_ui_without_a_static_dir() {
    let mut env = Env::new();
    env.app.static_dir = None;
    let htmx = env.request("GET", "/static/htmx.min.js", &[], &Value::Null);
    assert_eq!((200, "application/javascript"), (htmx.status, header(&htmx, "Content-Type").as_str()));
    assert_eq!("text/css", header(&env.request("GET", "/static/style.css", &[], &Value::Null), "Content-Type"));
    assert_eq!(404, env.request("GET", "/static/nope.js", &[], &Value::Null).status);
    assert_eq!(404, env.request("GET", "/static/../Cargo.toml", &[], &Value::Null).status);
}
