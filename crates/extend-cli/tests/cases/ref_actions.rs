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
    fake_with_managed_failure(stale, low_confidence, fail_action, None)
}

fn fake_with_managed_failure(
    stale: bool,
    low_confidence: bool,
    fail_action: bool,
    managed_failure: Option<&'static str>,
) -> Fake {
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
                    {"ref":"e1","type":"Button","label":if stale && n%2==1 {"Delete"}else{"Save"}},
                    {"ref":"e2","type":"TextField","label":"Email","value":""}
                ]});
            } else if fail_action {
                result["ok"] = json!(false);
                result["error"] = json!({"code":"stale_ref","message":"fixture rejected the ref",
                    "details":{"expectedLength":12,"actualLength":36,"privateValue":"do-not-echo"}});
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
        ("POST", "/api/v1/sessions/a3f/ref-selection") => {
            if let Some(failure) = managed_failure {
                return Some(match failure {
                    "http" => err(503, "service_unavailable", "Managed Jev unavailable"),
                    "old-service" => err(404, "not_found", "Route unavailable"),
                    "timeout" => {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        provider_response(json!({}))
                    }
                    "invalid-ref" | "invalid-operation" | "low-accepted" | "wrong-provider" | "missing-confidence" => {
                        let mut decision = json!({"provider":"jev","model":"fixture-managed-jev",
                            "accepted":true,"operation":"click","target":"@e1","confidence":1.0,
                            "reason":"fixture","model_ms":12.0,"usage":{}});
                        match failure {
                            "invalid-ref" => decision["target"] = json!("@e999"),
                            "invalid-operation" => decision["operation"] = json!("scroll"),
                            "low-accepted" => decision["confidence"] = json!(0.3),
                            "wrong-provider" => decision["provider"] = json!("llm"),
                            _ => decision["confidence"] = Value::Null,
                        }
                        ok("ref_selection", decision)
                    }
                    _ => provider_response(json!({"type":"ref_selection","data":{}})),
                });
            }
            let operation = if r.body["data"]["has_text"] == true {
                "fill"
            } else {
                "click"
            };
            Some(ok(
                "ref_selection",
                json!({"provider":"jev","model":"fixture-managed-jev",
                "accepted":!low_confidence,"operation":operation,"target":if operation=="fill" {"@e2"}else{"@e1"},
                "confidence":if low_confidence {0.4}else{1.0},"reason":"fixture","model_ms":12.0,"usage":{}}),
            ))
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

/// Selects the unique Search candidate, so a changed ref binding must reach the model again.
fn scripted_screens(screens: Vec<Value>, action_output: Value) -> Fake {
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
                result["output"] = screens[n.min(screens.len() - 1)].clone();
                result["output"]["refsGeneration"] = json!(10 + n);
            } else {
                result["output"] = action_output.clone();
            }
            Some(ok("command_result", result))
        }
        ("POST", "/jev") | ("POST", "/api/v1/sessions/a3f/ref-selection") => {
            let managed = r.path_only().ends_with("/ref-selection");
            let (elements, has_text) = if managed {
                let elements: serde_json::Map<String, Value> = r.body["data"]["snapshot"]["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|node| (format!("@{}", node["ref"].as_str().unwrap()), node.clone()))
                    .collect();
                (elements, r.body["data"]["has_text"] == true)
            } else {
                (
                    r.body["state"]["elements"].as_object().unwrap().clone(),
                    r.body["state"]["caller_supplied_fill_text"] == true,
                )
            };
            let matches: Vec<_> = elements
                .iter()
                .filter(|(_, node)| node["label"] == "Search" && (!has_text || node["type"] == "TextField"))
                .map(|(reference, _)| reference.as_str())
                .collect();
            let operation = if matches.len() != 1 {
                "BLOCKED"
            } else if has_text {
                "fill"
            } else {
                "click"
            };
            let target = if operation == "BLOCKED" { "BLOCKED" } else { matches[0] };
            if managed {
                Some(ok(
                    "ref_selection",
                    json!({"provider":"jev","model":"fixture-managed-jev",
                    "accepted":operation!="BLOCKED","operation":operation,
                    "target":if operation=="BLOCKED" {Value::Null}else{json!(target)},
                    "confidence":1.0,"reason":"fixture","model_ms":12.0,"usage":{}}),
                ))
            } else {
                let answers: serde_json::Map<String, Value> = r.body["questions"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, q)| {
                        let selected = if name == "operation" { operation } else { target };
                        let selected = if q["criteria"].get(selected).is_some() {
                            selected
                        } else {
                            "BLOCKED"
                        };
                        let probabilities: serde_json::Map<String, Value> = q["criteria"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .map(|key| (key.clone(), json!(if key == selected { 1.0 } else { 0.0 })))
                            .collect();
                        (
                            name.clone(),
                            json!({"choice":selected,"confidence":1.0,"probabilities":probabilities}),
                        )
                    })
                    .collect();
                Some(provider_response(json!({"model":"fixture-jev","answers":answers})))
            }
        }
        _ => None,
    })
}

#[test]
fn ref_act_reselects_after_layout_settles_and_uses_the_new_ref_binding() {
    let before = json!({"nodes":[{"ref":"e1","type":"Button","label":"Search"}]});
    let after = json!({"nodes":[
        {"ref":"e1","type":"Button","label":"Delete"},
        {"ref":"e2","type":"Button","label":"Search"}
    ]});
    for managed in [false, true] {
        let f = scripted_screens(vec![before.clone(), after.clone(), after.clone()], json!({}));
        let c = cli("ref-reselect-bound-ref", &f).env("TYPESAFE_API_KEY", if managed { "" } else { "model-only-key" });
        let out = c.run(&["act", "Click Search", "--json"]);
        assert!(out.status.success(), "{} {}", stdout(&out), stderr(&out));
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["status"], "executed");
        assert_eq!(doc["selection_rounds"], 2);
        assert_eq!(doc["attempts"][0]["target"], "@e1");
        assert_eq!(doc["attempts"][0]["selection_round"], 1);
        assert_eq!(doc["attempts"][1]["target"], "@e2");
        assert_eq!(doc["attempts"][1]["selection_round"], 2);
        assert_eq!(doc["revalidations"][0]["stable"], false);
        assert_eq!(
            doc["revalidations"][0]["changes"]["changed_refs"],
            json!([{"ref":"@e1","fields":["label"]}])
        );
        assert_eq!(doc["revalidations"][0]["changes"]["added_refs"], json!(["@e2"]));
        assert!(!doc["revalidations"].to_string().contains("Delete"));
        assert_eq!(doc["revalidations"][1]["stable"], true);
        assert!(doc["timings"]["selection_ms"].as_f64().unwrap() > 0.0);
        assert!(doc["timings"]["revalidate_ms"].as_f64().unwrap() > 0.0);
        let commands = f.requests("POST", "/api/v1/sessions/a3f/commands");
        assert_eq!(commands.len(), 4);
        assert!(
            commands[..3]
                .iter()
                .all(|request| request.body["data"]["command"] == "snapshot")
        );
        assert_eq!(commands[3].body["data"]["args"], json!(["@e2~s12"]));
        assert_eq!(
            f.requests(
                "POST",
                if managed {
                    "/api/v1/sessions/a3f/ref-selection"
                } else {
                    "/jev"
                }
            )
            .len(),
            2
        );
    }
}

#[test]
fn ref_act_changed_target_or_new_ambiguity_is_reconsidered_before_any_action() {
    let before = json!({"nodes":[{"ref":"e1","type":"Button","label":"Search"}]});
    for after in [
        json!({"nodes":[{"ref":"e1","type":"Button","label":"Delete"}]}),
        json!({"nodes":[{"ref":"e1","type":"Button","label":"Search"},
            {"ref":"e2","type":"Button","label":"Search"}]}),
    ] {
        let f = scripted_screens(vec![before.clone(), after], json!({}));
        let out = cli("ref-reselect-ambiguity", &f).run(&["act", "Click Search", "--json"]);
        assert!(!out.status.success());
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["status"], "blocked");
        assert_eq!(doc["selection_rounds"], 2);
        assert_eq!(doc["decision"]["operation"], "BLOCKED");
        assert_eq!(f.requests("POST", "/jev").len(), 2);
        let commands = f.requests("POST", "/api/v1/sessions/a3f/commands");
        assert_eq!(commands.len(), 2);
        assert!(
            commands
                .iter()
                .all(|request| request.body["data"]["command"] == "snapshot")
        );
    }
}

#[test]
fn ref_act_unrelated_changes_can_settle_but_never_skip_model_reselection() {
    let before = json!({"nodes":[{"ref":"e1","type":"Button","label":"Search"},
        {"ref":"e2","type":"Button","label":"Carousel one"}]});
    let mut after = before.clone();
    after["nodes"][1]["label"] = json!("Carousel two");
    let f = scripted_screens(vec![before, after.clone(), after], json!({}));
    let out = cli("ref-reselect-unrelated", &f).run(&["act", "Click Search", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "executed");
    assert_eq!(doc["decision"]["target"], "@e1");
    assert_eq!(f.requests("POST", "/jev").len(), 2);
    assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), 4);
}

#[test]
fn ref_act_accepted_fill_without_readback_is_unverified_and_never_retried() {
    let screen = json!({"nodes":[{"ref":"e1","type":"TextField","label":"Search","value":""}]});
    let output = json!({"verified":false,"verification":"unavailable","verificationReason":"accessibility_prompt",
        "warning":"The app's accessibility tree still reports its search prompt."});
    for json_mode in [true, false] {
        let f = scripted_screens(vec![screen.clone()], output.clone());
        let mut args = vec!["act", "Fill Search", "--text", "itc sunfeast"];
        if json_mode {
            args.push("--json");
        }
        let out = cli("ref-unverified-fill", &f).run(&args);
        assert!(out.status.success(), "{} {}", stdout(&out), stderr(&out));
        if json_mode {
            let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
            assert_eq!(doc["status"], "executed_unverified");
            assert_eq!(doc["ok"], true);
            assert_eq!(doc["result"]["output"], output);
            assert!(doc["normal_ref_fallback"].is_null());
        } else {
            assert!(stdout(&out).contains("executed_unverified: fill @e1"));
            assert!(stdout(&out).contains("could not verify"));
            assert!(stdout(&out).contains("accessibility tree still reports its search prompt"));
            assert!(stdout(&out).contains("before retrying"));
        }
        assert_eq!(f.requests("POST", "/jev").len(), 1);
        let commands = f.requests("POST", "/api/v1/sessions/a3f/commands");
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[2].body["data"]["command"], "fill");
    }
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
    assert!(f.requests("POST", "/api/v1/sessions/a3f/ref-selection").is_empty());
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
        ("ref-stale", true, false, "stale", 3),
        ("ref-low", false, true, "blocked", 1),
    ] {
        let f = fake(stale, low, false);
        let out = cli(name, &f).run(&["act", "Click Save", "--json"]);
        assert!(!out.status.success());
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["status"], expected);
        assert_eq!(doc["normal_ref_fallback"]["mode"], "normal_refs");
        assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), count);
        if stale {
            assert_eq!(doc["selection_rounds"], 2);
            assert_eq!(doc["revalidations"].as_array().unwrap().len(), 2);
            assert_eq!(f.requests("POST", "/jev").len(), 2, "reselection must stay bounded");
        }
        let regular = cli(&format!("{name}-regular"), &f).run(&["click", "@e1", "--json"]);
        assert!(regular.status.success(), "{}", stderr(&regular));
    }
}

#[test]
fn ref_act_explicit_fallback_is_measured_and_device_failure_is_not_retried() {
    let f = fake(false, true, true);
    let out = cli("ref-fallback", &f).run(&["act", "Click Save", "--fallback", "llm", "--json"]);
    assert!(!out.status.success());
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "execution_failed");
    assert!(doc["normal_ref_fallback"].is_null());
    assert_eq!(doc["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(doc["decision"]["provider"], "llm");
    assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), 3);
    assert_eq!(f.requests("POST", "/llm").len(), 1);
}

#[test]
fn ref_act_text_failure_reports_device_error_and_requires_inspection_before_retry() {
    let f = fake(false, false, true);
    let out = cli("ref-device-failure-text", &f).run(&[
        "act",
        "Fill Email",
        "--text",
        "alice@example.test",
        "--fallback",
        "llm",
    ]);
    assert!(!out.status.success());
    let text = stdout(&out);
    assert!(text.contains("execution_failed: fill @e2"), "{text}");
    assert!(
        text.contains("Device error [stale_ref]: fixture rejected the ref"),
        "{text}"
    );
    assert!(text.contains("may already have changed the device"), "{text}");
    assert!(text.contains("expected length 12, observed length 36"), "{text}");
    assert!(!text.contains("do-not-echo"), "{text}");
    assert!(
        text.contains("before retrying; do not repeat the action automatically"),
        "{text}"
    );
    let calls = f.requests("POST", "/api/v1/sessions/a3f/commands");
    assert_eq!(calls.len(), 3);
    assert_eq!(calls.iter().filter(|r| r.body["data"]["command"] == "fill").count(), 1);
    assert!(f.requests("POST", "/llm").is_empty());
}

#[test]
fn ref_act_provider_failures_leave_normal_commands_working() {
    for failure in ["http", "malformed", "timeout"] {
        let f = fake(false, false, false);
        let model = Fake::start(move |_| {
            if failure == "timeout" {
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            Some(Resp {
                status: if failure == "http" { 503 } else { 200 },
                headers: vec![],
                body: b"not json".to_vec(),
            })
        });
        let c = cli(&format!("ref-fail-{failure}"), &f).env("EXTEND_JEV_URL", &format!("{}/jev", model.url));
        let out = c.run(&["act", "Click Save", "--timeout", "1000", "--json"]);
        assert!(!out.status.success(), "{failure}");
        let doc: Value = serde_json::from_str(&stderr(&out)).unwrap();
        assert_eq!(
            doc["error"]["details"]["normal_ref_fallback"]["mode"], "normal_refs",
            "{doc}"
        );
        assert_eq!(doc["error"]["details"]["action_executed"], false);
        let calls = f.requests("POST", "/api/v1/sessions/a3f/commands");
        assert!(calls.iter().all(|r| r.body["data"]["command"] == "snapshot"));
        let count = model.requests("POST", "/jev").len();
        for argv in [vec!["snapshot", "-i", "--json"], vec!["click", "@e1", "--json"]] {
            let out = c.run(&argv);
            assert!(out.status.success(), "{failure}: {}", stderr(&out));
        }
        assert_eq!(model.requests("POST", "/jev").len(), count);
        assert!(f.requests("POST", "/api/v1/sessions/a3f/ref-selection").is_empty());
    }
}

#[test]
fn ref_act_explicit_llm_fallback_recovers_from_jev_http_failure() {
    let f = fake(false, false, false);
    let model = Fake::start(|_| {
        Some(Resp {
            status: 503,
            headers: vec![],
            body: vec![],
        })
    });
    let out = cli("ref-http-fallback", &f)
        .env("EXTEND_JEV_URL", &format!("{}/jev", model.url))
        .run(&["act", "Click Save", "--fallback", "llm", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "executed");
    assert_eq!(doc["decision"]["provider"], "llm");
    assert_eq!(model.requests("POST", "/jev").len(), 1);
    assert_eq!(f.requests("POST", "/llm").len(), 1);
}

#[test]
fn ref_act_managed_default_uses_extend_auth_and_keeps_literal_text_local() {
    for key in [None, Some(""), Some("   ")] {
        let f = fake(false, false, false);
        let mut c = Cli::new("ref-managed", &f.url)
            .signed_in("si:chef")
            .connected("a3f", &["snapshot", "click", "fill", "get"])
            // A personal endpoint override without a key must not redirect the managed request.
            .env("EXTEND_JEV_URL", "http://127.0.0.1:1/unused");
        if let Some(key) = key {
            c = c.env("TYPESAFE_API_KEY", key);
        }
        let out = c.run(&[
            "act",
            "Fill Email",
            "--text",
            "private@example.test",
            "--threshold",
            "0.8",
            "--json",
        ]);
        assert!(out.status.success(), "{} {}", stdout(&out), stderr(&out));
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["status"], "executed");
        assert_eq!(doc["decision"]["model"], "fixture-managed-jev");
        let requests = f.requests("POST", "/api/v1/sessions/a3f/ref-selection");
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-access")
        );
        assert_eq!(request.headers.get("x-org-id").map(String::as_str), Some("acme"));
        assert_eq!(request.body["type"], "ref_selection");
        assert_eq!(request.body["data"]["instruction"], "Fill Email");
        assert_eq!(request.body["data"]["threshold"], 0.8);
        assert_eq!(request.body["data"]["has_text"], true);
        assert_eq!(request.body["data"]["snapshot"]["nodes"][0]["ref"], "e1");
        assert!(!request.body.to_string().contains("private@example.test"));
        let commands = f.requests("POST", "/api/v1/sessions/a3f/commands");
        assert_eq!(commands.len(), 3);
        assert_eq!(
            commands[2].body["data"]["args"],
            json!(["@e2~s11", "private@example.test"])
        );
        assert!(f.requests("POST", "/jev").is_empty());
    }
}

#[test]
fn ref_act_managed_failures_preserve_normal_refs_and_explicit_llm_fallback() {
    for failure in ["http", "old-service", "malformed", "timeout"] {
        let f = fake_with_managed_failure(false, false, false, Some(failure));
        let c = cli(&format!("ref-managed-{failure}"), &f).env("TYPESAFE_API_KEY", "");
        let out = c.run(&["act", "Click Save", "--timeout", "1000", "--json"]);
        assert!(!out.status.success(), "{failure}");
        let doc: Value = serde_json::from_str(&stderr(&out)).unwrap();
        assert_eq!(doc["error"]["details"]["action_executed"], false, "{doc}");
        assert_eq!(
            doc["error"]["details"]["normal_ref_fallback"]["mode"], "normal_refs",
            "{doc}"
        );
        if failure == "http" {
            assert_eq!(doc["error"]["code"], "service_unavailable");
            assert_eq!(doc["error"]["request_id"], "req-0192");
        }
        assert!(
            f.requests("POST", "/api/v1/sessions/a3f/commands")
                .iter()
                .all(|r| r.body["data"]["command"] == "snapshot")
        );
        assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/ref-selection").len(), 1);
        for argv in [vec!["snapshot", "-i", "--json"], vec!["click", "@e1", "--json"]] {
            let out = c.run(&argv);
            assert!(out.status.success(), "{failure}: {}", stderr(&out));
        }
        assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/ref-selection").len(), 1);
        assert!(f.requests("POST", "/jev").is_empty());
    }
    let f = fake_with_managed_failure(false, false, false, Some("http"));
    let out = cli("ref-managed-llm-fallback", &f).env("TYPESAFE_API_KEY", "").run(&[
        "act",
        "Click Save",
        "--fallback",
        "llm",
        "--json",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "executed");
    assert_eq!(doc["decision"]["provider"], "llm");
    assert_eq!(doc["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(f.requests("POST", "/llm").len(), 1);
}

#[test]
fn ref_act_managed_rejects_invalid_threshold_before_capture() {
    for threshold in ["NaN", "inf", "-0.1", "1.1"] {
        let f = fake(false, false, false);
        let out = cli("ref-managed-threshold", &f).env("TYPESAFE_API_KEY", "").run(&[
            "act",
            "Click Save",
            "--threshold",
            threshold,
            "--json",
        ]);
        assert!(!out.status.success(), "{threshold}");
        assert!(f.requests("POST", "/api/v1/sessions/a3f/commands").is_empty());
        assert!(f.requests("POST", "/api/v1/sessions/a3f/ref-selection").is_empty());
    }
}

#[test]
fn ref_act_invalid_managed_decisions_fail_before_dry_run_or_execution_and_allow_fallback() {
    for failure in [
        "invalid-ref",
        "invalid-operation",
        "low-accepted",
        "wrong-provider",
        "missing-confidence",
    ] {
        for dry in [false, true] {
            let f = fake_with_managed_failure(false, false, false, Some(failure));
            let mut args = vec!["act", "Click Save", "--json"];
            if dry {
                args.push("--dry-run");
            }
            let out = cli("ref-managed-invalid", &f).env("TYPESAFE_API_KEY", "").run(&args);
            assert!(!out.status.success(), "{failure} dry={dry}");
            let doc: Value = serde_json::from_str(&stderr(&out)).unwrap();
            assert_eq!(doc["error"]["details"]["action_executed"], false, "{doc}");
            assert_eq!(
                doc["error"]["details"]["normal_ref_fallback"]["mode"], "normal_refs",
                "{doc}"
            );
            let commands = f.requests("POST", "/api/v1/sessions/a3f/commands");
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].body["data"]["command"], "snapshot");
        }
    }
    let f = fake_with_managed_failure(false, false, false, Some("invalid-ref"));
    let out = cli("ref-invalid-managed-llm-fallback", &f)
        .env("TYPESAFE_API_KEY", "")
        .run(&["act", "Click Save", "--fallback", "llm", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["status"], "executed");
    assert_eq!(doc["decision"]["provider"], "llm");
    assert_eq!(f.requests("POST", "/llm").len(), 1);
}

#[test]
fn ref_act_managed_stale_blocked_and_dry_run_never_execute() {
    for (name, stale, low, dry, expected, count) in [
        ("managed-stale", true, false, false, "stale", 3),
        ("managed-low", false, true, false, "blocked", 1),
        ("managed-dry", false, false, true, "selected", 1),
    ] {
        let f = fake(stale, low, false);
        let mut args = vec!["act", "Click Save", "--json"];
        if dry {
            args.push("--dry-run");
        }
        let out = cli(name, &f).env("TYPESAFE_API_KEY", "").run(&args);
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["status"], expected);
        assert_eq!(out.status.success(), dry);
        assert_eq!(f.requests("POST", "/api/v1/sessions/a3f/commands").len(), count);
        assert_eq!(
            f.requests("POST", "/api/v1/sessions/a3f/ref-selection").len(),
            if stale { 2 } else { 1 }
        );
        assert!(
            f.requests("POST", "/api/v1/sessions/a3f/commands")
                .iter()
                .all(|r| r.body["data"]["command"] == "snapshot")
        );
    }
}
