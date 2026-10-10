//! Real HTTP/WS regression: a reconnected host must report each carried device again before
//! it can be listed online or accept a new session, including released 1.0 hosts.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::json;

#[tokio::test]
async fn carried_online_requires_a_report_from_the_current_host_connection() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    for version in ["1.0.0", "1.1.0"] {
        let (host, credential) = pair(&env, &alice, DeviceOs::Macos, "Host", &[]).await;
        let old = App::connect(&env, &credential, hello(DeviceOs::Macos, version)).await;
        let (status, created) = api(
            &env,
            "POST",
            &format!("/api/v2/devices/{host}/attachments"),
            &alice,
            Some(json!({"type":"attachment", "data":{"os":"tvos", "name":"Carried TV"}})),
        )
        .await;
        assert_eq!(status, 201, "{created}");
        let child = created["data"]["device_id"].as_str().unwrap();
        grant(&env, &alice, child, "si:chef").await;
        let report = json!({"type":"attached", "device_id":child, "online":true,
            "capabilities":["input.remote", "nav.system"], "missing":[],
            "setup":{"state":"complete", "steps":[]}});
        old.send(report.clone());
        eventually("initial carried report online and ready", || async {
            let (_, view) = api(&env, "GET", &format!("/api/v2/devices/{child}"), &alice, None).await;
            view["data"]["online"] == true && view["data"]["state"] == "ready"
        })
        .await;

        // The helper fences the replacement Hello with a WebSocket Pong. No carried report is
        // sent by that connection until after the assertions below.
        let current = App::connect(&env, &credential, hello(DeviceOs::Macos, version)).await;
        let (status, view) = api(&env, "GET", &format!("/api/v2/devices/{child}"), &alice, None).await;
        assert_eq!(status, 200, "{view}");
        assert_eq!(
            view["data"]["online"], false,
            "{version}: prior socket's report must not survive replacement"
        );
        let (status, refused) = api(
            &env,
            "POST",
            "/api/v2/sessions",
            &chef,
            Some(json!({"type":"session", "data":{"device_id":child}})),
        )
        .await;
        assert_eq!(status, 503, "{refused}");
        assert_eq!(refused["data"]["code"], "device_offline", "{refused}");

        current.send(report);
        eventually("fresh carried report online", || async {
            let (_, view) = api(&env, "GET", &format!("/api/v2/devices/{child}"), &alice, None).await;
            view["data"]["online"] == true
        })
        .await;
        let (status, session) = api(
            &env,
            "POST",
            "/api/v2/sessions",
            &chef,
            Some(json!({"type":"session", "data":{"device_id":child}})),
        )
        .await;
        assert_eq!(status, 201, "{session}");
        current.wait("session_started", |f| f["target"] == child).await;
        drop(old);
        drop(current);
        eventually("host disconnect clears carried online state", || async {
            let (_, view) = api(&env, "GET", &format!("/api/v2/devices/{child}"), &alice, None).await;
            view["data"]["online"] == false
        })
        .await;
    }
}
