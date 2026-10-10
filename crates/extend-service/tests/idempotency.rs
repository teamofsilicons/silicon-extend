//! Retry stored successful responses without repeating work or weakening current authorization.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};

async fn post(env: &Env, token: &str, path: &str, key: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}{path}", env.base))
        .bearer_auth(token)
        .header("idempotency-key", key)
        .json(body)
        .send()
        .await
        .unwrap()
}

async fn assert_replay(response: reqwest::Response, expected: &Value) {
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap_or_default());
    assert_eq!(response.headers()["idempotency-replayed"], "true");
    assert_eq!(response.json::<Value>().await.unwrap(), *expected);
}

#[tokio::test]
async fn session_retries_replay_after_start_end_and_disconnect_but_still_check_access() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (device, credential) = pair(&env, &alice, DeviceOs::Android, "Phone", &["si:chef"]).await;
    let app = App::connect(&env, &credential, hello(DeviceOs::Android, "1.1.0")).await;
    let body = json!({"type": "session", "data": {"device_id": device}});
    let first = post(&env, &chef, "/api/v2/sessions", "session-retry", &body).await;
    assert_eq!(first.status(), 201);
    let first = first.json::<Value>().await.unwrap();
    let session_id = first["data"]["session_id"].as_str().unwrap();
    assert_replay(
        post(&env, &chef, "/api/v2/sessions", "session-retry", &body).await,
        &first,
    )
    .await;
    let fresh = post(&env, &chef, "/api/v2/sessions", "fresh-session-key", &body).await;
    assert_eq!(fresh.status(), 409);
    assert_eq!(fresh.json::<Value>().await.unwrap()["data"]["code"], "device_in_use");
    env.v2(&chef).end_session(session_id).await.unwrap();
    assert_replay(
        post(&env, &chef, "/api/v2/sessions", "session-retry", &body).await,
        &first,
    )
    .await;
    drop(app);
    eventually("device disconnect", || async {
        !env.state.hub.is_connected(&("extend".into(), device.clone())).await
    })
    .await;
    assert_replay(
        post(&env, &chef, "/api/v2/sessions", "session-retry", &body).await,
        &first,
    )
    .await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.sessions WHERE device_id = $1")
        .bind(&device)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "retries must not create another session");
    sqlx::query("DELETE FROM extend.device_access WHERE device_id = $1 AND silicon_id = $2")
        .bind(&device)
        .bind(uuid("si:chef"))
        .execute(&env.pool)
        .await
        .unwrap();
    let revoked = post(&env, &chef, "/api/v2/sessions", "session-retry", &body).await;
    assert_eq!(
        revoked.status(),
        404,
        "a stored response does not bypass revoked device access"
    );
}

#[tokio::test]
async fn attachment_retry_replays_after_host_disconnect_without_another_attach() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (host, credential) = pair(&env, &alice, DeviceOs::Macos, "Mac", &[]).await;
    let app = App::connect(&env, &credential, hello(DeviceOs::Macos, "1.1.0")).await;
    let path = format!("/api/v2/devices/{host}/attachments");
    let body = json!({"type": "attachment", "data": {"os": "tvos", "name": "TV", "address": "192.0.2.1"}});
    let first = post(&env, &alice, &path, "attachment-retry", &body).await;
    assert_eq!(first.status(), 201, "{}", first.text().await.unwrap_or_default());
    let first = first.json::<Value>().await.unwrap();
    app.wait("attach", |_| true).await;
    assert_replay(post(&env, &alice, &path, "attachment-retry", &body).await, &first).await;
    assert_eq!(app.of("attach").len(), 1);
    drop(app);
    eventually("host disconnect", || async {
        !env.state.hub.is_connected(&("extend".into(), host.clone())).await
    })
    .await;
    assert_replay(post(&env, &alice, &path, "attachment-retry", &body).await, &first).await;
    let fresh = post(&env, &alice, &path, "fresh-attachment-key", &body).await;
    assert_eq!(fresh.status(), 503);
    assert_eq!(fresh.json::<Value>().await.unwrap()["data"]["code"], "device_offline");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.devices WHERE host_device_id = $1")
        .bind(&host)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "retries must not attach another device");
}

#[tokio::test]
async fn report_retries_do_not_consume_new_report_quota() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let body = json!({"type": "report", "data": {"message": "Owned retry fixture", "client_version": "1.1.0"}});
    let first = post(&env, &alice, "/api/v2/reports", "report-retry", &body).await;
    assert_eq!(first.status(), 202);
    let first = first.json::<Value>().await.unwrap();
    for _ in 0..12 {
        let replay = post(&env, &alice, "/api/v2/reports", "report-retry", &body).await;
        assert_eq!(replay.status(), 202, "{}", replay.text().await.unwrap_or_default());
        assert_eq!(replay.headers()["idempotency-replayed"], "true");
        assert_eq!(replay.json::<Value>().await.unwrap(), first);
    }
    for i in 1..10 {
        let fresh = post(&env, &alice, "/api/v2/reports", &format!("new-report-{i}"), &body).await;
        assert_eq!(fresh.status(), 202);
    }
    let limited = post(&env, &alice, "/api/v2/reports", "eleventh-report", &body).await;
    assert_eq!(limited.status(), 429);
    assert_eq!(limited.json::<Value>().await.unwrap()["data"]["code"], "rate_limited");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.reports")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(count, 10);
}

#[tokio::test]
async fn wake_retries_replay_after_muting_without_another_notification() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (device, _) = pair(&env, &alice, DeviceOs::Android, "Phone", &["si:chef"]).await;
    let path = format!("/api/v2/devices/{device}/wake-requests");
    let body = json!({"type": "wake_request", "data": {"reason": "Owned wake retry"}});
    let first = post(&env, &chef, &path, "wake-retry", &body).await;
    assert_eq!(first.status(), 201, "{}", first.text().await.unwrap_or_default());
    let first = first.json::<Value>().await.unwrap();
    let (status, settings) = api(
        &env,
        "PUT",
        &format!("/api/v2/devices/{device}/wake-settings"),
        &alice,
        Some(json!({"type": "wake_settings", "data": {"muted": true}})),
    )
    .await;
    assert_eq!(status, 200, "{settings}");
    let ting = env.state.local_ting.clone().unwrap();
    let sent = ting.sent.lock().await.len();
    assert_replay(post(&env, &chef, &path, "wake-retry", &body).await, &first).await;
    let fresh = post(&env, &chef, &path, "fresh-wake-key", &body).await;
    assert_eq!(fresh.status(), 409);
    assert_eq!(fresh.json::<Value>().await.unwrap()["data"]["code"], "conflict");
    assert_eq!(ting.sent.lock().await.len(), sent);
}
