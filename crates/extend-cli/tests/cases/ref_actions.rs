use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn provider_response(body: Value) -> Resp {
    Resp {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.to_string().into_bytes(),
    }
}

fn fake(stale: bool, low_confidence: bool, fail_action: bool) -> Fake {
    let snapshots = AtomicUsize::new(0);
    Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/auth/me") => Some(ok("me", me("si:chef"))),
        ("GET", "/api/v1/sessions/a3f") => Some(ok(
            "session",
            session("a3f", "active", &["snapshot", "click", "fill", "get"]),
        )),
        ("POST", "/api/v1/sessions/a3f/commands") => {
            let mut result = command_result(json!([]), json!([]));
            result["command"] = r.body["data"]["command"].clone();
            if result["command"] == "snapshot" {
                let n = snapshots.fetch_add(1, Ordering::SeqCst);
                result["output"] = json!({"refsGeneration":10+n,"nodes":[
                    {"ref":"e1","type":"Button","label":if stale && n>0 {"Delete"}else{"Save"}},
                    {"ref":"e2","type":"TextField","label":"Email","value":""}
                ]});
            } else if fail_action {
                result["ok"] = json!(false);
                result["error"] = json!({"code":"stale_ref","message":"fixture rejected the ref"});
            }
            Some(ok("command_result", result))
        }
        ("POST", "/jev") => {
            assert_eq!(
                r.headers.get("authorization").map(String::as_str),
                Some("Bearer model-only-key")
            );
            let operation = if r.body["questions"].get("fill_target").is_some() {
                "fill"
            } else {
                "click"
            };
            let target = if operation == "fill" { "@e2" } else { "@e1" };
            let answers:serde_json::Map<String,Value> = r.body["questions"].as_object().unwrap().iter().map(|(name,q)| {
                let selected = if name=="operation" {operation}else{target};
                let selected = if q["criteria"].get(selected).is_some() {selected}else{"BLOCKED"};
                let probabilities:serde_json::Map<String,Value> = q["criteria"].as_object().unwrap().keys()
                    .map(|k|(k.clone(),json!(if k==selected {1.0}else{0.0}))).collect();
                (name.clone(),json!({"choice":selected,"confidence":if low_confidence {0.4}else{1.0},"probabilities":probabilities}))
            }).collect();
            Some(provider_response(json!({"model":"fixture-jev","answers":answers})))
        }
        ("POST", "/llm") => Some(provider_response(
            json!({"model":"fixture-llm","choices":[{"message":{"content":"{\"operation\":\"click\",\"target\":\"@e1\"}"}}]}),
        )),
        _ => None,
    })
}

fn cli(name: &str, fake: &Fake) -> Cli {
    Cli::new(name, &fake.url)
        .signed_in("si:chef")
        .connected("a3f", &["snapshot", "click", "fill", "get"])
        .env("TYPESAFE_API_KEY", "model-only-key")
        .env("EXTEND_JEV_URL", &format!("{}/jev", fake.url))
        .env("EXTEND_REF_LLM_KEY", "baseline-only-key")
        .env("EXTEND_REF_LLM_URL", &format!("{}/llm", fake.url))
        .env("EXTEND_REF_LLM_MODEL", "fixture-llm")
}

#[test]
fn ref_act_executes_once_with_fresh_generation_and_preserves_literal_text() {
    let f = fake(false, false, false);
    let out = cli("ref-fill", &f).run(&["act", "Fill the email field", "--text", "alice@example.test", "--json"]);
    assert!(out.status.success(), "{} {}", stdout(&out), stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "executed");
    let calls = f.requests("POST", "/api/v1/sessions/a3f/commands");
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].body["data"]["args"], json!(["-i", "--force-full"]));
    assert_eq!(calls[2].body["data"]["command"], "fill");
    assert_eq!(calls[2].body["data"]["args"], json!(["@e2~s11", "alice@example.test"]));
    assert!(!f.requests("POST", "/jev")[0].body.to_string().contains("test-access"));
}

#[test]
fn ref_act_refuses_flag_like_text_before_screen_capture_or_inference() {
    let f = fake(false, false, false);
    let out = cli("ref-fill-flag", &f).run(&["act", "Fill Email", "--text", "--json", "--json"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("device parsers"),
        "{} {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(f.requests("POST", "/api/v1/sessions/a3f/commands").is_empty());
    assert!(f.requests("POST", "/jev").is_empty());
}

#[test]
fn ref_act_dry_run_does_not_execute_or_claim_successful_action() {
    let f = fake(false, false, false);
    let out = cli("ref-dry", &f).run(&["act", "Click Save", "--dry-run", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "selected");
    assert!(doc["result"].is_null());
    assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), 1);
}

#[test]
fn ref_act_changed_screen_and_low_confidence_never_execute() {
    for (name, stale, low, expected, count) in [
        ("ref-stale", true, false, "stale", 2),
        ("ref-low", false, true, "blocked", 1),
    ] {
        let f = fake(stale, low, false);
        let out = cli(name, &f).run(&["act", "Click Save", "--json"]);
        assert!(!out.status.success());
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["status"], expected);
        assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), count);
    }
}

#[test]
fn ref_act_explicit_fallback_is_measured_and_device_failure_is_not_retried() {
    let f = fake(false, true, true);
    let out = cli("ref-fallback", &f).run(&["act", "Click Save", "--fallback", "llm", "--json"]);
    assert!(!out.status.success());
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "execution_failed");
    assert_eq!(doc["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(doc["decision"]["provider"], "llm");
    assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), 3);
    assert_eq!(f.requests("POST", "/llm").len(), 1);
}
