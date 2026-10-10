//! Retired model selection cannot run; ordinary snapshot and ref commands still reach the device.
mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::json;

#[tokio::test]
async fn removed_selection_route_returns_not_found_without_affecting_device_commands() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (device, credential) = pair(&env, &alice, DeviceOs::Android, "Phone", &["si:chef"]).await;
    let app = App::connect(&env, &credential, hello(DeviceOs::Android, "1.1.0")).await;
    let (status, started) = session(&env, &chef, &device).await;
    assert_eq!(status, 201, "{started}");
    let id = started["data"]["session_id"].as_str().unwrap();
    let path = format!("/api/v2/sessions/{id}");

    for token in [&chef, "invalid-token"] {
        let (status, answer) = api(
            &env,
            "POST",
            &format!("{path}/ref-selection"),
            token,
            Some(json!({"type":"ref_selection","data":{
                "snapshot":{"nodes":[{"ref":"e1","type":"Button","label":"Save"}]},
                "instruction":"Click Save","has_text":false,"threshold":0.7
            }})),
        )
        .await;
        assert_eq!(status, 404, "{answer}");
    }
    assert!(
        app.of("command").is_empty(),
        "The removed route must not send device input"
    );

    for (command, args) in [
        ("snapshot", vec![]),
        ("click", vec!["@e1"]),
        ("fill", vec!["@e2", "search text"]),
    ] {
        let (status, answer) = api(
            &env,
            "POST",
            &format!("{path}/commands"),
            &chef,
            Some(json!({"type":"command","data":{"command":command,"args":args}})),
        )
        .await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["data"]["ok"], true, "{answer}");
        let sent = app.wait("command", |frame| frame["command"] == command).await;
        assert_eq!(sent["args"], json!(args));
    }
    assert_eq!(app.of("command").len(), 3);
}
