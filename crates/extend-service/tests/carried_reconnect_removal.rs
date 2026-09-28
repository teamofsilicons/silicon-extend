//! A reconnect must withdraw carried pairs removed while their exact host was offline, without
//! withdrawing another Carbon's alias or revealing its live session. Real HTTP, WS and PostgreSQL.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};

async fn attach(env: &Env, token: &str, host: &str, name: &str) -> String {
    let (status, body) = api(
        env,
        "POST",
        &format!("/api/v1/devices/{host}/attachments"),
        token,
        None,
        Some(json!({"type": "attachment", "data": {"os": "tvos", "name": name}})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    body["data"]["device_id"].as_str().unwrap().to_owned()
}

fn report(id: &str, key: &str) -> Value {
    json!({"type": "attached", "device_id": id, "online": true,
        "capabilities": ["input.remote", "nav.system"], "missing": [],
        "setup": {"state": "complete", "steps": []}, "hardware_key": key})
}

async fn removal_is_reconciled(app_version: &str) {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let sous = login(&env, "si:sous").await;
    let (host, credential) = pair(&env, &alice, Some("acme"), DeviceOs::Macos, "Mac", &[]).await;
    let app = App::connect(&env, &credential, hello(DeviceOs::Macos, "1.1.0")).await;
    let (other_host, other_credential) = pair_another(&env, &credential, &bob, Some("acme"), &[]).await;
    let other_app = App::connect(&env, &other_credential, hello(DeviceOs::Macos, "1.1.0")).await;

    let removed = attach(&env, &alice, &host, "Alice's removed TV").await;
    let retained = attach(&env, &alice, &host, "Alice's retained TV").await;
    let alias = attach(&env, &bob, &other_host, "Bob's live alias").await;
    let private_removed = attach(&env, &bob, &other_host, "Bob's removed TV").await;
    app.send(report(&removed, "shared-tv"));
    app.send(report(&retained, "other-tv"));
    eventually("Alice's hardware report applied", || async {
        let key: Option<String> = sqlx::query_scalar("SELECT hardware_key FROM extend.devices WHERE device_id = $1")
            .bind(&removed)
            .fetch_one(&env.pool)
            .await
            .unwrap();
        key.as_deref() == Some("shared-tv")
    })
    .await;
    other_app.send(report(&alias, "shared-tv"));
    eventually("the carried aliases linked", || async {
        instance_of(&env, &removed).await == instance_of(&env, &alias).await
    })
    .await;
    grant(&env, &bob, &alias, "si:sous", "acme").await;
    let (status, started) = session(&env, &sous, "acme", &alias).await;
    assert_eq!(status, 201, "{started}");
    let sid = started["data"]["session_id"].as_str().unwrap();
    other_app.wait("session_started", |f| f["session_id"] == sid).await;

    drop(app);
    eventually("Alice's exact host disconnected", || async {
        !env.state.hub.is_connected(&("extend".into(), host.clone())).await
    })
    .await;
    for (id, token) in [(&removed, &alice), (&private_removed, &bob)] {
        let (status, body) = api(&env, "DELETE", &format!("/api/v1/devices/{id}"), token, None, None).await;
        assert_eq!(status, 204, "{body}");
    }
    other_app
        .wait("attach", |f| f["device_id"] == private_removed && f["removed"] == true)
        .await;
    other_app.clear();

    // App::connect's per-connection Pong fence is after the entire service greeting. There is no
    // sleep or retry here that could hide a missing tombstone or a late, cross-pair announcement.
    let reconnected = App::connect(&env, &credential, hello(DeviceOs::Macos, app_version)).await;
    let mut attached: Vec<_> = reconnected
        .of("attach")
        .into_iter()
        .map(|f| {
            (
                f["device_id"].as_str().unwrap().to_owned(),
                f["removed"].as_bool().unwrap(),
            )
        })
        .collect();
    attached.sort();
    let mut expected = vec![(removed.clone(), true), (retained.clone(), false)];
    expected.sort();
    assert_eq!(attached, expected, "the greeting must reconcile only this host's pairs");
    assert!(
        reconnected.of("session_started").is_empty(),
        "a removed alias must not announce its sibling's session"
    );
    assert!(reconnected.of("session_ended").is_empty());

    // The other Carbon's session is still usable through its original socket after the removal
    // and reconnect, even though the removed alias belongs to the same physical device.
    let (status, body) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/commands"),
        &sous,
        Some("acme"),
        Some(json!({"type": "command", "data": {"command": "home", "args": []}})),
    )
    .await;
    assert_eq!(status, 200, "live sibling command: {body}");
    assert_eq!(body["data"]["ok"], true, "{body}");
    assert!(
        other_app.of("attach").is_empty(),
        "another host's greeting cannot alter this pair"
    );
    assert!(other_app.of("session_ended").is_empty());
    assert!(!other_app.is_closed());
    let (status, body) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["state"], "active", "{body}");
}

#[tokio::test]
async fn removed_carried_pair_is_withdrawn_from_reconnecting_1_0_host() {
    removal_is_reconciled("1.0.0").await;
}

#[tokio::test]
async fn removed_carried_pair_is_withdrawn_from_reconnecting_1_1_host() {
    removal_is_reconciled("1.1.0").await;
}
