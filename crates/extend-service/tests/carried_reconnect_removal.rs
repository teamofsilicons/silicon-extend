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
        &format!("/api/v2/devices/{host}/attachments"),
        token,
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
    let (host, credential) = pair(&env, &alice, DeviceOs::Macos, "Mac", &[]).await;
    let app = App::connect(&env, &credential, hello(DeviceOs::Macos, "1.1.0")).await;
    let (other_host, other_credential) = pair_another(&env, &credential, &bob, &[]).await;
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
    grant(&env, &bob, &alias, "si:sous").await;
    let (status, started) = session(&env, &sous, &alias).await;
    assert_eq!(status, 201, "{started}");
    let sid = started["data"]["session_id"].as_str().unwrap();
    other_app.wait("session_started", |f| f["session_id"] == sid).await;

    drop(app);
    eventually("Alice's exact host disconnected", || async {
        !env.state.hub.is_connected(&("extend".into(), host.clone())).await
    })
    .await;
    for (id, token) in [(&removed, &alice), (&private_removed, &bob)] {
        // Physical revocation (native/scheduler path) still reconciles carried tombstones.
        let actor = env.state.accounts.authenticate(token).await.unwrap().actor();
        extend_service::domain::unpair(
            &env.state,
            &extend_service::db::World::production(),
            id,
            extend_protocol::model::EndReason::PairRevoked,
            &actor,
        )
        .await
        .unwrap();
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
        &format!("/api/v2/sessions/{sid}/commands"),
        &sous,
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
    let (status, body) = api(&env, "GET", &format!("/api/v2/sessions/{sid}"), &sous, None).await;
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

/// Removing a computer takes its instance lock; a device attached through it that committed while
/// the removal waited for that lock (so after the removal listed the computer's carried devices)
/// still leaves with it, instead of staying paired through a removed computer.
#[tokio::test]
async fn removing_a_host_includes_an_attachment_committing_while_it_waits_for_the_lock() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (host, _) = pair(&env, &alice, DeviceOs::Macos, "Mac", &[]).await;
    let instance = instance_of(&env, &host).await;
    let mut attaching = env.pool.begin().await.unwrap();
    extend_service::domain::lock_instances(&mut attaching, &extend_service::db::World::production(), &[instance])
        .await
        .unwrap();
    sqlx::query("INSERT INTO extend.devices (device_id, owner_id, name, os, host_device_id) VALUES ('1a2b3c4d', $2, 'TV', 'tvos', $1)")
        .bind(&host)
        .bind(uuid("c:alice"))
        .execute(&mut *attaching)
        .await
        .unwrap();
    let base = env.base.clone();
    let removing = tokio::spawn(async move {
        reqwest::Client::new()
            .delete(format!("{base}/api/v2/devices/{host}"))
            .bearer_auth(alice)
            .send()
            .await
            .unwrap()
    });
    eventually("the removal waiting for the attachment's host lock", || async {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'
               AND query LIKE '%device_instances%' AND query LIKE '%FOR NO KEY UPDATE%')",
        )
        .fetch_one(&env.pool)
        .await
        .unwrap()
    })
    .await;
    attaching.commit().await.unwrap();
    assert_eq!(removing.await.unwrap().status().as_u16(), 204);
    let live: Vec<String> =
        sqlx::query_scalar("SELECT device_id FROM extend.devices WHERE removed_at IS NULL ORDER BY device_id")
            .fetch_all(&env.pool)
            .await
            .unwrap();
    assert!(
        live.is_empty(),
        "the newly committed attachment leaves with its host: {live:?}"
    );
    let reason: Option<String> =
        sqlx::query_scalar("SELECT removed_reason FROM extend.devices WHERE device_id = '1a2b3c4d'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(reason.as_deref(), Some("device_removed"));
}
