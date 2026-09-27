//! Second upstream, shadows, settings, Shadow Compare and schema migrations.
mod common;
use common::*;
use jev_proxy::compare::{disagrees, gap, save_verdict, shadow_rows};
use jev_proxy::db::{get_settings, open, save_settings};
use jev_proxy::proxy::{execute_call, write_shadow, Resolved};
use serde_json::{json, Value};

fn hi() -> Value {
    json!({"state": "hi"})
}
fn set_engines(env: &Env, engines: Value, served: &str) {
    save_settings(&env.db(), &engines, &json!(served)).unwrap();
}

// ---- model rewrite and auth per engine ----

#[test]
fn second_gets_no_authorization_and_its_own_model_and_the_row_keeps_the_original_body() {
    let env = Env::with(|c| c.second_model = "second".into());
    env.second.reply(200, SECOND_OK);
    let sent = br#"{"state":"hi","model":"jev-latest","questions":{"q":{}}}"#;
    let r = execute_call(&env.db(), "/v1/systemone", sent, "t", &env.app.config, "second", false).unwrap();
    assert_eq!((200, "second"), (r.status, r.upstream.as_str()));
    assert_eq!(vec![None], env.second.auth());
    assert_eq!("second", serde_json::from_slice::<Value>(&env.second.bodies()[0]).unwrap()["model"]);
    let (upstream, request): (String, String) = env.db().query_row("SELECT upstream, request_body FROM calls", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(("second".to_string(), String::from_utf8_lossy(sent).to_string()), (upstream, request));
}

#[test]
fn an_unchanged_body_is_sent_byte_for_byte() {
    let env = Env::new();
    env.upstream.reply(200, JEV_OK);
    let sent = br#"{"model":"jev-latest",  "state":"hi"}"#;
    execute_call(&env.db(), "/v1/systemone", sent, "t", &env.app.config, "typesafe", false).unwrap();
    assert_eq!(vec![sent.to_vec()], env.upstream.bodies());
}

#[test]
fn the_same_body_on_both_engines_gives_two_cache_entries() {
    let env = Env::new();
    env.upstream.reply(200, JEV_OK);
    env.second.reply(200, SECOND_OK);
    let body = br#"{"state":"hi","model":"jev-latest","questions":{"q":{}}}"#;
    execute_call(&env.db(), "/v1/systemone", body, "t", &env.app.config, "typesafe", false).unwrap();
    let second = execute_call(&env.db(), "/v1/systemone", body, "t", &env.app.config, "second", false).unwrap();
    assert!(!second.cached);
    assert_eq!(2, env.db().query_row("SELECT COUNT(*) FROM cache", [], |r| r.get::<_, i64>(0)).unwrap());
}

// ---- engine routes ----

#[test]
fn served_second_needs_no_typesafe_key() {
    let env = Env::with(|c| c.api_key = String::new());
    set_engines(&env, json!(["second"]), "second");
    env.second.reply(200, SECOND_OK);
    let r = env.post("/v1/systemone", hi());
    assert_eq!(200, r.status);
    assert_eq!(("second", ""), (header(&r, "X-Jev-Upstream").as_str(), header(&r, "X-Jev-Shadows").as_str()));
}

#[test]
fn served_typesafe_without_a_key_gets_the_500() {
    let env = Env::with(|c| c.api_key = String::new());
    let r = env.post("/v1/systemone", hi());
    assert_eq!(500, r.status);
    assert!(String::from_utf8_lossy(&r.body).contains("TYPESAFE_API_KEY"));
}

#[test]
fn a_second_that_is_down_gives_a_502_naming_it() {
    let env = Env::with(|c| c.second_base_url = "http://127.0.0.1:1".into());
    set_engines(&env, json!(["second"]), "second");
    let r = env.post("/v1/systemone", hi());
    assert_eq!(502, r.status);
    assert!(String::from_utf8_lossy(&r.body).contains("Second unreachable"));
    assert_eq!(("0", "second"), (header(&r, "X-Jev-Upstream-Status").as_str(), header(&r, "X-Jev-Upstream").as_str()));
}

#[test]
fn second_runs_as_a_shadow_of_typesafe() {
    let env = Env::new();
    set_engines(&env, json!(["typesafe", "second"]), "typesafe");
    env.upstream.reply(200, JEV_OK);
    env.second.reply(200, SECOND_OK);
    let r = env.post("/v1/systemone", hi());
    assert_eq!((200, JEV_OK.to_string()), (r.status, String::from_utf8(r.body.clone()).unwrap()));
    assert_eq!("second", header(&r, "X-Jev-Shadows"));
    let row = env.shadow(call_id(&r), "second", Some("done")).unwrap();
    assert_eq!(("done".to_string(), Some(200), Some(SECOND_OK.to_string())), (row.0, row.1, row.2));
}

#[test]
fn a_typesafe_shadow_with_no_key_is_skipped() {
    let env = Env::with(|c| c.api_key = String::new());
    set_engines(&env, json!(["typesafe", "second"]), "second");
    env.second.reply(200, SECOND_OK);
    let r = env.post("/v1/systemone", hi());
    let row = env.shadow(call_id(&r), "typesafe", None).unwrap();
    assert_eq!("skipped", row.0);
    assert!(row.3.unwrap().contains("TYPESAFE_API_KEY"));
}

#[test]
fn a_full_shadow_queue_skips() {
    let env = Env::with(|c| c.max_pending_shadows = 0);
    set_engines(&env, json!(["typesafe", "second"]), "typesafe");
    env.upstream.reply(200, JEV_OK);
    let r = env.post("/v1/systemone", hi());
    assert_eq!("skipped", env.shadow(call_id(&r), "second", None).unwrap().0);
    assert!(env.second.bodies().is_empty());
}

#[test]
fn run_on_second_stores_the_shadow_and_the_served_engine_is_refused() {
    let env = Env::new();
    env.upstream.reply(200, JEV_OK);
    let id = call_id(&env.post("/v1/systemone", hi()));
    env.second.reply(200, SECOND_OK);
    let r = env.post(&format!("/api/calls/{id}/run"), json!({"provider": "second"}));
    assert_eq!(200, r.status);
    let shadows = body_json(&r)["shadows"].clone();
    assert_eq!(json!(["second", "done", SECOND_OK]), json!([shadows[0]["provider"], shadows[0]["state"], shadows[0]["responseBody"]]));
    assert_eq!(400, env.post(&format!("/api/calls/{id}/run"), json!({"provider": "typesafe"})).status);
    assert_eq!(400, env.post(&format!("/api/calls/{id}/run"), json!({"provider": "gpt"})).status);
    assert_eq!(404, env.post("/api/calls/999/run", json!({"provider": "second"})).status);
}

#[test]
fn second_status_reports_a_down_server_and_a_healthy_one() {
    let down = Env::with(|c| c.second_base_url = "http://127.0.0.1:1".into());
    let s = down.get("/api/second").1;
    assert_eq!((false, 0), (s["up"].as_bool().unwrap(), s["pendingShadows"].as_i64().unwrap()));
    assert!(!s["error"].is_null());
    let up = Env::new().get("/api/second").1;
    assert_eq!((true, json!(["m"])), (up["up"].as_bool().unwrap(), up["loaded"].clone()));
}

// ---- settings ----

#[test]
fn settings_default_to_typesafe_only_and_round_trip_in_provider_order() {
    let env = Env::new();
    assert_eq!(("typesafe", 1), (get_settings(&env.db()).unwrap().served.as_str(), get_settings(&env.db()).unwrap().engines.len()));
    save_settings(&env.db(), &json!(["second", "typesafe"]), &json!("second")).unwrap();
    let s = get_settings(&env.db()).unwrap();
    assert_eq!((vec!["typesafe".to_string(), "second".to_string()], "second".to_string()), (s.engines, s.served));
}

#[test]
fn settings_reject_invalid_input() {
    let env = Env::new();
    for (engines, served) in [(json!([]), json!(null)), (json!(["typesafe", "gpt"]), json!("typesafe")), (json!(["typesafe"]), json!("second"))] {
        assert!(save_settings(&env.db(), &engines, &served).is_err());
    }
    assert_eq!(400, env.request("PUT", "/api/settings", &[], &json!({"engines": [], "served": "typesafe"})).status);
}

#[test]
fn settings_only_offer_the_second_engine_once_configured() {
    let env = Env::with(|c| c.second_base_url = String::new());
    assert_eq!(json!(["typesafe"]), env.get("/api/settings").1["providers"]);
    assert_eq!(json!(["typesafe", "second"]), Env::new().get("/api/settings").1["providers"]);
}

#[test]
fn the_api_key_is_masked_in_settings() {
    let env = Env::with(|c| c.api_key = "apikey_0000abcdefghijklmnwxyz".into());
    assert_eq!("apikey_0000...wxyz", env.get("/api/settings").1["api_key_masked"]);
}

// ---- migrations ----

#[test]
fn a_calls_table_without_upstream_gets_the_column_and_old_rows_stay_typesafe() {
    let dir = std::env::temp_dir().join(format!("jev-old-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("jev.db");
    {
        let old = rusqlite::Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, caller TEXT NOT NULL, path TEXT NOT NULL, \
             model TEXT, request_body TEXT NOT NULL, response_body TEXT, status INTEGER NOT NULL, elapsed_ms INTEGER, \
             cached INTEGER NOT NULL DEFAULT 0, body_hash TEXT NOT NULL, input_tokens INTEGER, output_tokens INTEGER, \
             attempts INTEGER NOT NULL DEFAULT 1, error TEXT);
             INSERT INTO calls (ts, caller, path, request_body, status, body_hash) VALUES ('x', 'old', '/v1/systemone', '{}', 200, 'h');",
        ).unwrap();
    }
    let conn = open(&path).unwrap();
    let row: (String, String) = conn.query_row("SELECT caller, upstream FROM calls", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(("old".to_string(), "typesafe".to_string()), row);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn open_is_idempotent_and_keeps_existing_rows() {
    let env = Env::new();
    env.db().execute("INSERT INTO requests (name, created_at, updated_at) VALUES ('keep me', 'x', 'x')", []).unwrap();
    let names: Vec<String> = env.db().prepare("SELECT name FROM requests").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(vec!["keep me".to_string()], names);
}

// ---- Shadow Compare ----

fn noul(v: f64) -> Value {
    json!({"type": "noul", "noul": v})
}

fn insert_call(env: &Env, request: Value, answers: Value) -> i64 {
    let conn = env.db();
    conn.execute(
        "INSERT INTO calls (ts, caller, path, request_body, response_body, status, body_hash, upstream) \
         VALUES ('2026-09-23T10:00:00Z', 'john', '/v1/systemone', ?, ?, 200, 'h', 'typesafe')",
        rusqlite::params![request.to_string(), json!({"model": "jev-1.13.0", "answers": answers}).to_string()],
    ).unwrap();
    conn.last_insert_rowid()
}

fn resolved(answers: Value, status: i64) -> Resolved {
    Resolved {
        status, elapsed_ms: 1, cached: false, attempts: 1, error: None, body_hash: String::new(), model: Value::Null,
        input_tokens: None, output_tokens: None,
        body: json!({"model": "second-rl-agent", "routing": {"model": "english"}, "answers": answers}).to_string(),
    }
}

#[test]
fn gap_noul_choice_and_mismatch() {
    assert!((gap(&noul(0.8), &noul(0.3)).unwrap() - 0.5).abs() < 1e-9);
    assert!(disagrees(&noul(0.8), &noul(0.3)));
    assert!(!disagrees(&noul(0.8), &noul(0.6)));
    let (a, b) = (json!({"type": "choice", "choice": "peer"}), json!({"type": "choice", "choice": "curt"}));
    assert_eq!(Some(1.0), gap(&a, &b));
    assert!(disagrees(&a, &b));
    assert_eq!(None, gap(&noul(0.5), &json!({"type": "choice", "choice": "x"})));
    assert_eq!(None, gap(&noul(0.5), &Value::Null));
}

#[test]
fn gap_score_is_scaled_by_the_legend() {
    let legend = json!({"1": "a", "2": "b", "3": "c"});
    let (a, b) = (json!({"type": "score", "score": 1, "legend": legend}), json!({"type": "score", "score": 3, "legend": legend}));
    assert_eq!(Some(1.0), gap(&a, &b));
}

#[test]
fn shadow_rows_one_per_question_biggest_gap_first() {
    let env = Env::new();
    let request = json!({"state": {"item_1": "Ship it", "item_2": "Long text"},
        "questions": {"item_1_is_kiss": {"instructions": "Short?"}, "item_2_is_kiss": {"instructions": "Short?"}}});
    let id = insert_call(&env, request, json!({"item_1_is_kiss": noul(0.9), "item_2_is_kiss": noul(0.2)}));
    write_shadow(&env.db(), id, "second", "done", Some(&resolved(json!({"item_1_is_kiss": noul(0.8), "item_2_is_kiss": noul(0.9)}), 200)), None).unwrap();
    let rows = shadow_rows(&env.db(), 500).unwrap();
    assert_eq!(json!(["item_2_is_kiss", "item_1_is_kiss"]), json!(rows.iter().map(|r| r["questionId"].clone()).collect::<Vec<_>>()));
    assert_eq!(json!(["is_kiss", "item_2", "Long text", true]), json!([rows[0]["label"], rows[0]["subject"], rows[0]["text"], rows[0]["disagree"]]));
    assert_eq!("second-rl-agent · english", rows[0]["second"]["model"]);
    assert_eq!("Short?", rows[0]["instructions"]);
}

#[test]
fn shadow_rows_a_question_on_no_key_gets_the_whole_state() {
    let env = Env::new();
    let id = insert_call(&env, json!({"state": {"message": "Hi team"}, "questions": {"proposes_fix": {}}}), json!({"proposes_fix": noul(0.1)}));
    write_shadow(&env.db(), id, "second", "done", Some(&resolved(json!({"proposes_fix": noul(0.7)}), 200)), None).unwrap();
    assert_eq!("Hi team", shadow_rows(&env.db(), 500).unwrap()[0]["text"]);
}

#[test]
fn shadow_rows_a_pending_or_failed_shadow_gives_no_row() {
    let env = Env::new();
    let id = insert_call(&env, json!({"state": "x", "questions": {"q": {}}}), json!({"q": noul(0.1)}));
    write_shadow(&env.db(), id, "second", "pending", None, None).unwrap();
    assert!(shadow_rows(&env.db(), 500).unwrap().is_empty());
    write_shadow(&env.db(), id, "second", "done", Some(&resolved(json!({"q": noul(0.9)}), 422)), None).unwrap();
    assert!(shadow_rows(&env.db(), 500).unwrap().is_empty());
}

#[test]
fn a_verdict_is_saved_replaced_and_cleared() {
    let env = Env::new();
    let id = insert_call(&env, json!({"state": "x", "questions": {"q": {}}}), json!({"q": noul(0.1)}));
    write_shadow(&env.db(), id, "second", "done", Some(&resolved(json!({"q": noul(0.9)}), 200)), None).unwrap();
    save_verdict(&env.db(), &json!(id), &json!("q"), &json!("second")).unwrap();
    save_verdict(&env.db(), &json!(id), &json!("q"), &json!("typesafe")).unwrap();
    assert_eq!("typesafe", shadow_rows(&env.db(), 500).unwrap()[0]["verdict"]);
    save_verdict(&env.db(), &json!(id), &json!("q"), &Value::Null).unwrap();
    assert!(shadow_rows(&env.db(), 500).unwrap()[0]["verdict"].is_null());
}

#[test]
fn an_unknown_verdict_or_call_is_refused() {
    let env = Env::new();
    let id = insert_call(&env, json!({"state": "x", "questions": {}}), json!({}));
    assert!(save_verdict(&env.db(), &json!(id), &json!("q"), &json!("gpt")).is_err());
    assert!(save_verdict(&env.db(), &json!(id + 1), &json!("q"), &json!("second")).is_err());
}

#[test]
fn vote_then_read_back_through_the_routes() {
    let env = Env::new();
    let id = insert_call(&env, json!({"state": "x", "questions": {"q": {}}}), json!({"q": noul(0.1)}));
    write_shadow(&env.db(), id, "second", "done", Some(&resolved(json!({"q": noul(0.9)}), 200)), None).unwrap();
    assert_eq!(200, env.post("/api/verdicts", json!({"callId": id, "questionId": "q", "verdict": "neither"})).status);
    assert_eq!("neither", env.get("/api/shadow-compare").1["rows"][0]["verdict"]);
    assert_eq!(400, env.post("/api/verdicts", json!({"callId": id, "questionId": "q", "verdict": "both"})).status);
}
