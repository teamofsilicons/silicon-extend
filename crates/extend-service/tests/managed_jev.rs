//! Managed ref selection is session-authorized, isolated per world and optional for commands.
mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

fn request() -> Value {
    json!({"type":"ref_selection","data":{
        "snapshot":{"nodes":[{"ref":"e1","type":"Button","label":"Save"}]},
        "instruction":"Click Save","has_text":false,"threshold":0.7
    }})
}

async fn connected(env: &Env) -> (String, String, String, App) {
    let alice = login(env, "c:alice").await;
    let chef = login(env, "si:chef").await;
    let (device, credential) = pair(
        env,
        &alice,
        Some("acme"),
        DeviceOs::Android,
        "Phone",
        &["si:chef", "si:sous"],
    )
    .await;
    let app = App::connect(env, &credential, hello(DeviceOs::Android, "1.1.0")).await;
    let (status, session) = session(env, &chef, "acme", &device).await;
    assert_eq!(status, 201, "{session}");
    let id = session["data"]["session_id"].as_str().unwrap();
    (alice, chef, format!("/api/v1/sessions/{id}"), app)
}

async fn select(env: &Env, session: &str, token: &str, body: Value) -> (u16, Value) {
    api(
        env,
        "POST",
        &format!("{session}/ref-selection"),
        token,
        Some("acme"),
        Some(body),
    )
    .await
}

async fn normal_command(env: &Env, session: &str, token: &str) {
    let (status, answer) = api(
        env,
        "POST",
        &format!("{session}/commands"),
        token,
        Some("acme"),
        Some(json!({"type":"command","data":{"command":"click","args":["@e1"]}})),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["data"]["ok"], true);
}

#[tokio::test]
async fn only_the_current_session_silicon_can_select_and_selection_never_executes() {
    let mock = jev::JevMock::start().await;
    let env = start_configured(Default::default(), |c| c.jev = mock.config()).await;
    let (alice, chef, path, app) = connected(&env).await;
    let (status, answer) = select(&env, &path, &chef, request()).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["type"], "ref_selection");
    assert_eq!(answer["data"]["accepted"], true);
    assert_eq!(answer["data"]["target"], "@e1");
    assert!(app.of("command").is_empty());
    assert_eq!(mock.calls.lock().unwrap()[0].0, "Bearer managed-prod-fixture");

    let mut strict = request();
    strict["data"]["threshold"] = json!(0.95);
    let (status, answer) = select(&env, &path, &chef, strict).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["data"]["accepted"], false);
    assert_eq!(answer["data"]["reason"], "low_confidence");

    let sous = login(&env, "si:sous").await;
    for token in [&alice, &sous, "bad-token"] {
        let (status, _) = select(&env, &path, token, request()).await;
        assert!(status >= 400);
    }
    assert_eq!(mock.calls.lock().unwrap().len(), 2);
    let (status, _) = api(&env, "POST", &format!("{path}/end"), &chef, Some("acme"), None).await;
    assert!(status < 300);
    let (status, _) = select(&env, &path, &chef, request()).await;
    assert!(status >= 400);
    assert_eq!(mock.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_inputs_never_reach_provider_and_failures_leave_normal_commands_working() {
    let mock = jev::JevMock::start().await;
    let env = start_configured(Default::default(), |c| c.jev = mock.config()).await;
    let (_, chef, path, app) = connected(&env).await;
    for (key, value) in [
        ("threshold", json!(1.1)),
        ("instruction", json!("")),
        ("snapshot", json!({"nodes":[]})),
    ] {
        let mut invalid = request();
        invalid["data"][key] = value;
        let (status, answer) = select(&env, &path, &chef, invalid).await;
        assert!(status >= 400, "{answer}");
        assert_eq!(answer["data"]["code"], "invalid_input");
    }
    let mut large = request();
    large["data"]["snapshot"]["unused"] = json!("x".repeat(300_000));
    let (status, _) = select(&env, &path, &chef, large).await;
    assert!(status >= 400);
    assert!(mock.calls.lock().unwrap().is_empty());

    mock.status.store(503, Ordering::Relaxed);
    let (status, answer) = select(&env, &path, &chef, request()).await;
    assert_eq!(status, 503, "{answer}");
    assert_eq!(answer["data"]["code"], "service_unavailable");
    assert_eq!(answer["data"]["details"]["action_executed"], false);
    assert!(!answer.to_string().contains("secret-provider-error-body"));
    assert!(!answer.to_string().contains("managed-prod-fixture"));
    assert!(app.of("command").is_empty());
    normal_command(&env, &path, &chef).await;
    assert_eq!(app.of("command").len(), 1);
}

#[tokio::test]
async fn absent_managed_config_does_not_block_normal_ref_commands() {
    let env = start().await;
    let (_, chef, path, _app) = connected(&env).await;
    let (status, answer) = select(&env, &path, &chef, request()).await;
    assert_eq!(status, 503, "{answer}");
    assert_eq!(answer["data"]["code"], "service_unavailable");
    normal_command(&env, &path, &chef).await;
}

#[tokio::test]
async fn test_world_uses_only_test_key_and_missing_test_key_never_falls_back() {
    for configured in [true, false] {
        let mock = jev::JevMock::start().await;
        let env = start_configured(Default::default(), |c| {
            c.jev = mock.config();
            if !configured {
                c.jev.test_api_key = None;
            }
        })
        .await;
        let (_, secret) = open_test_env(&env).await;
        let alice = login_in(&env, &secret, "c:alice").await;
        let chef = login_in(&env, &secret, "si:chef").await;
        let (device, credential) = pair_in(&env, &secret, &alice, DeviceOs::Android, "Test phone")
            .await
            .unwrap();
        let app = App::connect(&env, &credential, hello(DeviceOs::Android, "1.1.0")).await;
        let (status, grant) = api_in(
            &env,
            &secret,
            "PUT",
            &format!("/api/v1/devices/{device}/access/si:chef?team=acme"),
            Some(&alice),
            Some("acme"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{grant}");
        let (status, started) = api_in(
            &env,
            &secret,
            "POST",
            "/api/v1/sessions",
            Some(&chef),
            Some("acme"),
            Some(json!({"type":"session","data":{"device_id":device}})),
        )
        .await;
        assert_eq!(status, 201, "{started}");
        let id = started["data"]["session_id"].as_str().unwrap();
        let (status, answer) = api_in(
            &env,
            &secret,
            "POST",
            &format!("/api/v1/sessions/{id}/ref-selection"),
            Some(&chef),
            Some("acme"),
            Some(request()),
        )
        .await;
        assert_eq!(status, if configured { 200 } else { 503 }, "{answer}");
        let calls = mock.calls.lock().unwrap();
        if configured {
            assert_eq!(calls[0].0, "Bearer managed-test-fixture");
        } else {
            assert!(calls.is_empty());
        }
        assert!(app.of("command").is_empty());
    }
}

#[tokio::test]
async fn a_device_without_snapshot_support_cannot_use_managed_ref_selection() {
    let mock = jev::JevMock::start().await;
    let env = start_configured(Default::default(), |c| c.jev = mock.config()).await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (device, credential) = pair(
        &env,
        &alice,
        Some("acme"),
        DeviceOs::Android,
        "Touch only",
        &["si:chef"],
    )
    .await;
    let mut capabilities = hello(DeviceOs::Android, "1.1.0");
    capabilities["capabilities"] = json!(["input.touch"]);
    let _app = App::connect(&env, &credential, capabilities).await;
    let (status, started) = session(&env, &chef, "acme", &device).await;
    assert_eq!(status, 201, "{started}");
    let id = started["data"]["session_id"].as_str().unwrap();
    let (_, answer) = select(&env, &format!("/api/v1/sessions/{id}"), &chef, request()).await;
    assert_eq!(answer["data"]["code"], "unsupported_on_device", "{answer}");
    assert!(mock.calls.lock().unwrap().is_empty());
}
