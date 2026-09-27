//! Android TV element clicks: HTTP gating and command lists agree, without inventing mouse/touch
//! capabilities or exposing the command to an older app whose own gate would refuse it.
mod common;

// Decode the live response using the released 1.0 enum and resource definitions. In particular,
// its commands are strings and can already carry `click`; no capability value is being added.
#[allow(dead_code)]
#[path = "../../extend-protocol/tests/compat_1_0/capability.rs"]
mod capability;
#[allow(dead_code)]
#[path = "../../extend-protocol/tests/compat_1_0/model.rs"]
mod legacy_model;
pub use extend_protocol::{TEST_DEVICE_LIMIT, ids};

use common::*;
use extend_protocol::DeviceOs;
use serde_json::json;

#[tokio::test]
async fn tv_click_routes_only_to_apps_with_the_element_click_gate() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    for (version, supported) in [("1.0.0", false), ("1.0.2", false), ("1.1.0", true)] {
        let (id, credential) = pair(&env, &alice, Some("acme"), DeviceOs::AndroidTv, "TV", &["si:chef"]).await;
        let mut h = hello(DeviceOs::AndroidTv, version);
        // A legacy TV with accessibility but no ADB or accessibility D-pad still has element
        // actions. Requiring input.remote would needlessly exclude it.
        h["capabilities"] = json!(["screen.read", "input.text", "nav.system", "apps.launch"]);
        let app = App::connect(&env, &credential, h).await;
        let (status, d) = api(&env, "GET", &format!("/api/v1/devices/{id}"), &chef, Some("acme"), None).await;
        assert_eq!(status, 200, "{d}");
        let old: legacy_model::Device = serde_json::from_value(d["data"].clone()).expect("1.0 device reader");
        let commands = old.commands.unwrap();
        assert_eq!(commands.iter().any(|c| c == "click"), supported, "app {version}: {d}");
        for unsupported in ["hover", "press", "scroll", "swipe", "gesture", "longpress"] {
            assert!(!commands.iter().any(|c| c == unsupported), "{unsupported}: {d}");
        }
        let (status, session) = session(&env, &chef, "acme", &id).await;
        assert_eq!(status, 201, "{session}");
        let sid = session["data"]["session_id"].as_str().unwrap();
        app.clear();
        let (status, result) = api(
            &env,
            "POST",
            &format!("/api/v1/sessions/{sid}/commands"),
            &chef,
            Some("acme"),
            Some(json!({"type":"command", "data":{"command":"click", "args":["@e2"]}})),
        )
        .await;
        assert_eq!(status, if supported { 200 } else { 422 }, "app {version}: {result}");
        assert_eq!(app.of("command").len(), usize::from(supported));
        if supported {
            assert_eq!(app.of("command")[0]["command"], "click");
            assert_eq!(app.of("command")[0]["args"], json!(["@e2"]));
        } else {
            assert_eq!(result["data"]["code"], "unsupported_on_device");
        }
    }
}

#[tokio::test]
async fn tv_click_disappears_and_is_refused_when_accessibility_disconnects() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (id, credential) = pair(&env, &alice, Some("acme"), DeviceOs::AndroidTv, "TV", &["si:chef"]).await;
    let app = App::connect(&env, &credential, hello(DeviceOs::AndroidTv, "1.1.0")).await;
    let (_, before) = api(&env, "GET", &format!("/api/v1/devices/{id}"), &chef, Some("acme"), None).await;
    assert!(before["data"]["commands"].as_array().unwrap().contains(&json!("click")));
    let (status, session) = session(&env, &chef, "acme", &id).await;
    assert_eq!(status, 201, "{session}");
    let sid = session["data"]["session_id"].as_str().unwrap();
    let mut h = hello(DeviceOs::AndroidTv, "1.1.0");
    // ADB's remote keys/screenshots alone are insufficient to resolve accessibility elements.
    h["capabilities"] = json!(["input.remote", "screen.capture", "adb"]);
    h["missing"] = json!([{"capability":"screen.read", "reason":"Accessibility disconnected"}]);
    app.send(h);
    eventually("click disappears after accessibility disconnects", || async {
        let (_, d) = api(&env, "GET", &format!("/api/v1/devices/{id}"), &chef, Some("acme"), None).await;
        !d["data"]["commands"].as_array().unwrap().contains(&json!("click"))
    })
    .await;
    app.clear();
    let (status, result) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/commands"),
        &chef,
        Some("acme"),
        Some(json!({"type":"command", "data":{"command":"click", "args":["@e2"]}})),
    )
    .await;
    assert_eq!(status, 422, "{result}");
    assert_eq!(result["data"]["details"]["needs_any_of"], json!(["screen.read"]));
    assert!(
        result["data"]["message"]
            .as_str()
            .unwrap()
            .contains("Accessibility disconnected")
    );
    assert!(app.of("command").is_empty());
}
