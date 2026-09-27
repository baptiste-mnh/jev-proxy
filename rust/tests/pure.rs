//! Hash, cache, decompose and call shape: no network.
mod common;
use common::Env;
use jev_proxy::db::{cache_get, cache_put};
use jev_proxy::decompose::{compose, decompose};
use jev_proxy::proxy::canonical_hash;
use jev_proxy::shape::{call_shape, question_ids};
use serde_json::{json, Value};

// ---- canonical hash ----

#[test]
fn hash_key_order_does_not_matter() {
    let a = json!({"state": "hi", "model": "jev-latest", "questions": {}});
    let b = json!({"questions": {}, "model": "jev-latest", "state": "hi"});
    assert_eq!(canonical_hash("/v1/systemone", &a), canonical_hash("/v1/systemone", &b));
}

#[test]
fn hash_model_and_path_change_it() {
    let (a, b) = (json!({"state": "hi", "model": "jev-latest"}), json!({"state": "hi", "model": "jev-1.13.0"}));
    assert_ne!(canonical_hash("/v1/systemone", &a), canonical_hash("/v1/systemone", &b));
    assert_ne!(canonical_hash("/v1/systemone", &a), canonical_hash("/v1/other", &a));
}

#[test]
fn hash_state_may_be_a_string_or_an_array() {
    for state in [json!("plain text"), json!(["a", "b"]), json!({"nested": {"deep": 1}})] {
        assert_eq!(64, canonical_hash("/v1/systemone", &json!({"state": state})).len());
    }
}

#[test]
fn hash_array_order_matters() {
    assert_ne!(
        canonical_hash("/v1/systemone", &json!({"state": ["a", "b"]})),
        canonical_hash("/v1/systemone", &json!({"state": ["b", "a"]}))
    );
}

/// Values computed by the Python implementation, so an existing cache stays valid.
#[test]
fn hash_matches_the_python_implementation() {
    let body = json!({"state": "héllo \"q\"\n", "model": "jev-latest", "questions": {"b": 1, "a": [true, null, 2.5]}});
    assert_eq!(
        "b178b513fb70f93bd029ea92845572e7df68ce56be4d9b55b486d106257abc26",
        canonical_hash("/v1/systemone", &body)
    );
}

// ---- cache ----

#[test]
fn cache_hit_within_ttl_and_miss_when_absent() {
    let env = Env::new();
    let conn = env.db();
    cache_put(&conn, "h1", r#"{"answers":{}}"#, 200).unwrap();
    assert_eq!(Some((r#"{"answers":{}}"#.to_string(), 200)), cache_get(&conn, "h1", 3600).unwrap());
    assert_eq!(None, cache_get(&conn, "nope", 3600).unwrap());
}

#[test]
fn cache_expired_entry_is_not_served_and_is_dropped() {
    let env = Env::new();
    let conn = env.db();
    cache_put(&conn, "h1", "{}", 200).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(None, cache_get(&conn, "h1", 1).unwrap());
    assert_eq!(0, conn.query_row("SELECT COUNT(*) FROM cache", [], |r| r.get::<_, i64>(0)).unwrap());
}

#[test]
fn cache_never_stores_a_non_2xx() {
    let env = Env::new();
    let conn = env.db();
    for status in [422, 429, 500, 529] {
        assert!(!cache_put(&conn, &format!("h{status}"), "{}", status).unwrap());
        assert_eq!(None, cache_get(&conn, &format!("h{status}"), 3600).unwrap());
    }
}

#[test]
fn cache_put_replaces_an_existing_entry() {
    let env = Env::new();
    let conn = env.db();
    cache_put(&conn, "h1", r#"{"v":1}"#, 200).unwrap();
    cache_put(&conn, "h1", r#"{"v":2}"#, 200).unwrap();
    assert_eq!(r#"{"v":2}"#, cache_get(&conn, "h1", 3600).unwrap().unwrap().0);
}

// ---- decompose ----

fn question() -> Value {
    json!({"suffix": "sounds_human", "instructions": "Does {ref} read as human?", "criteria_true": "yes", "criteria_false": "no"})
}
fn items() -> Vec<Value> {
    vec![json!({"key": "item_1", "text": "first"}), json!({"key": "item_10", "text": "tenth"})]
}
fn form_body() -> Value {
    compose(&items(), &[question()], &json!("jev-latest"))
}

#[test]
fn decompose_round_trips_a_form_built_body() {
    assert_eq!(Some((items(), vec![question()])), decompose(&form_body()));
}

#[test]
fn decompose_rejects_a_question_on_one_item_only() {
    let mut body = form_body();
    body["questions"].as_object_mut().unwrap().shift_remove("item_10_sounds_human");
    assert_eq!(None, decompose(&body));
}

#[test]
fn decompose_rejects_diverging_instructions_across_items() {
    let mut body = form_body();
    body["questions"]["item_10_sounds_human"]["instructions"] = json!("Other?");
    assert_eq!(None, decompose(&body));
}

#[test]
fn decompose_rejects_a_non_noul_question() {
    let mut body = form_body();
    body["questions"]["item_1_sounds_human"]["type"] = json!("choice");
    assert_eq!(None, decompose(&body));
}

#[test]
fn decompose_rejects_a_text_state() {
    assert_eq!(None, decompose(&json!({"state": "a", "questions": {}})));
}

#[test]
fn decompose_rejects_a_body_missing_criteria() {
    let mut body = compose(&items()[..1], &[question()], &json!("jev-latest"));
    body["questions"]["item_1_sounds_human"].as_object_mut().unwrap().shift_remove("criteria");
    assert_eq!(None, decompose(&body));
}

// ---- call shape ----

fn shape(body: Value) -> Value {
    call_shape(&body.to_string())
}

#[test]
fn shape_dedups_labels_across_items() {
    let s = shape(json!({
        "state": {"item_1": "first  text", "item_10": "tenth"},
        "questions": {"item_1_is_kiss": {}, "item_1_sounds_human": {}, "item_10_is_kiss": {}, "item_10_sounds_human": {}},
    }));
    assert_eq!(2, s["subjects"]);
    assert!(s["subject"].is_null());
    assert_eq!(json!(["is_kiss", "sounds_human"]), s["labels"]);
    assert_eq!("first text", s["snippet"]);
}

#[test]
fn shape_splits_on_the_last_double_underscore_without_a_state_key() {
    let s = shape(json!({
        "state": {"context": "ctx", "sections": "s"},
        "questions": {"intro__two__is_kiss": {}, "intro__two__needs_glossary": {}},
    }));
    assert_eq!("intro__two", s["subject"]);
    assert_eq!(json!(["is_kiss", "needs_glossary"]), s["labels"]);
}

#[test]
fn shape_snippet_is_the_first_asked_subject_inside_a_container() {
    let s = shape(json!({
        "state": {"context": "ctx", "sections": {"intro__one": "not asked", "intro__two": "asked"}},
        "questions": {"intro__two__is_kiss": {}, "intro__two__needs_glossary": {}},
    }));
    assert_eq!("intro__two", s["firstSubject"]);
    assert_eq!("asked", s["snippet"]);
}

#[test]
fn shape_an_object_item_asked_by_its_key_is_one_subject_not_a_container() {
    let s = shape(json!({
        "state": {"item_a": {"text": "first", "other_version": "old"}, "item_b": {"text": "second", "other_version": "old"}},
        "questions": {"item_a__is_kiss": {}, "item_b__is_kiss": {}},
    }));
    assert_eq!(2, s["subjects"]);
    assert_eq!("item_a", s["firstSubject"]);
    assert_eq!(json!(["is_kiss"]), s["labels"]);
    assert!(s["snippet"].as_str().unwrap().starts_with("text: first"));
}

#[test]
fn shape_truncates_a_long_snippet() {
    let s = shape(json!({"state": {"item_1": "x".repeat(500)}, "questions": {"item_1_q": {}}}));
    let snippet = s["snippet"].as_str().unwrap();
    assert!(snippet.ends_with('…'));
    assert!(snippet.chars().count() <= 141);
}

#[test]
fn shape_malformed_body_gives_an_empty_shape() {
    assert_eq!(0, call_shape("not json")["subjects"]);
    assert_eq!(json!([]), call_shape("[1, 2]")["labels"]);
}

#[test]
fn question_ids_empty_for_a_malformed_body_or_without_questions() {
    assert!(question_ids("not json").is_empty());
    assert!(question_ids(r#"{"state":"a"}"#).is_empty());
}
