//! Silicon Accounts' events (`POST /webhooks/accounts`): signatures and deduplication, and what
//! each event does in Extend (crate::lifecycle).

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// A raw delivery: the body as given, signed with `secret` at `ts`.
async fn post_raw(env: &Env, body: &str, ts: i64, signature: Option<String>) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .post(format!("{}/webhooks/accounts", env.base))
        .header("content-type", "application/json")
        .header("x-accounts-timestamp", ts.to_string());
    if let Some(sig) = signature {
        r = r.header("x-accounts-signature", sig);
    }
    let resp = r.body(body.to_owned()).send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn sign(secret: &str, ts: i64, body: &str) -> String {
    silicon_accounts_client::sign_webhook(secret, ts, body.as_bytes())
}

/// What Extend has cached about an account: (id, display name).
async fn cached(env: &Env, who: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT id, display_name FROM extend.accounts WHERE uuid = $1")
        .bind(uuid(who))
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

async fn events_recorded(env: &Env, event_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM extend.accounts_events WHERE event_id = $1")
        .bind(event_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn deliveries_are_verified_before_anything_is_read() {
    let env = start().await;
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    let body =
        json!({"event_id": "evt_ping_1", "type": "ping", "occurred_at": now_rfc3339(), "app_id": "extend", "data": {}})
            .to_string();
    // A good signature.
    assert_eq!(
        post_raw(&env, &body, ts, Some(sign(WEBHOOK_SECRET, ts, &body))).await.0,
        204
    );
    // Another secret, no signature, a changed body, a stale or future timestamp: 401.
    let refused = [
        (
            "another secret",
            body.clone(),
            ts,
            Some(sign("whsec_someone_else", ts, &body)),
        ),
        ("no signature", body.clone(), ts, None),
        (
            "a changed body",
            body.replace("ping", "pong"),
            ts,
            Some(sign(WEBHOOK_SECRET, ts, &body)),
        ),
        (
            "six minutes old",
            body.clone(),
            ts - 360,
            Some(sign(WEBHOOK_SECRET, ts - 360, &body)),
        ),
        (
            "six minutes ahead",
            body.clone(),
            ts + 360,
            Some(sign(WEBHOOK_SECRET, ts + 360, &body)),
        ),
    ];
    for (what, b, at, sig) in refused {
        let (s, e) = post_raw(&env, &b, at, sig).await;
        assert_eq!(
            (s, e["data"]["code"].as_str()),
            (401, Some("unauthorized")),
            "{what}: {e}"
        );
    }
    // Signed, but not an event: 400.
    for bad in [
        "not json".to_owned(),
        json!({"type": "account.updated"}).to_string(),
        json!({"event_id": "evt_bad", "type": "account.id_changed", "data": "not an object"}).to_string(),
    ] {
        let (s, e) = post_raw(&env, &bad, ts, Some(sign(WEBHOOK_SECRET, ts, &bad))).await;
        assert_eq!(
            (s, e["data"]["code"].as_str()),
            (400, Some("invalid_input")),
            "{bad}: {e}"
        );
    }
}

#[tokio::test]
async fn each_event_is_applied_once_and_unknown_types_are_acknowledged() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let (s, _) = api(&env, "GET", "/api/v2/me", &chef, None).await;
    assert_eq!(s, 200);
    // chef changes its id in Silicon Accounts, which tells Extend.
    env.accounts().update(&uuid("si:chef"), |a| a.id = "si:chef-two".into());
    let event = uuid::Uuid::now_v7().to_string();
    let changed =
        |new_id: &str| json!({"uuid": uuid("si:chef"), "kind": "silicon", "old_id": "si:chef", "new_id": new_id});
    assert_eq!(
        deliver_raw(&env, &event, "account.id_changed", changed("si:chef-two")).await,
        204
    );
    // A retry (the same event id) changes nothing, even with other data.
    assert_eq!(
        deliver_raw(&env, &event, "account.id_changed", changed("si:chef-three")).await,
        204
    );
    assert_eq!(events_recorded(&env, &event).await, 1);
    assert_eq!(cached(&env, "si:chef").await.0, "si:chef-two");
    let (_, me) = api(&env, "GET", "/api/v2/me", &chef, None).await;
    assert_eq!(me["data"]["id"], "si:chef-two", "{me}");
    // Types Extend doesn't act on are acknowledged.
    for t in ["silicon.stk_rotated", "account.something_new", "ping"] {
        assert_eq!(deliver(&env, t, json!({"uuid": uuid("si:chef")})).await, 204, "{t}");
    }
}

#[tokio::test]
async fn id_changes_show_everywhere_and_older_tokens_dont_undo_them() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (d, _) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:chef"]).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let status = deliver(
        &env,
        "account.id_changed",
        json!({"uuid": uuid("si:chef"), "kind": "silicon", "old_id": "si:chef", "new_id": "si:head-chef"}),
    )
    .await;
    assert_eq!(status, 204);
    // chef's token from before the change still carries si:chef: it signs in, but the id stays.
    let (_, me) = api(&env, "GET", "/api/v2/me", &chef, None).await;
    assert_eq!(me["data"]["id"], "si:head-chef", "{me}");
    let (_, access) = api(&env, "GET", &format!("/api/v2/devices/{d}/access"), &alice, None).await;
    let grant = &access["data"]["items"][0];
    assert_eq!(
        (grant["silicon_id"].as_str(), grant["silicon_uuid"].as_str()),
        (Some("si:head-chef"), Some(uuid("si:chef").as_str())),
        "{access}"
    );
    // Lookups by the old id find nothing; by the new one, the same account.
    env.accounts()
        .update(&uuid("si:chef"), |a| a.id = "si:head-chef".into());
    let (s, a) = api(&env, "GET", "/api/v2/accounts/lookup?id=si:head-chef", &alice, None).await;
    assert_eq!(
        (s, a["data"]["uuid"].as_str()),
        (200, Some(uuid("si:chef").as_str())),
        "{a}"
    );
}

#[tokio::test]
async fn account_updates_apply_in_version_order() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (s, _) = api(&env, "GET", "/api/v2/me", &alice, None).await;
    assert_eq!(s, 200);
    env.accounts()
        .update(&uuid("c:alice"), |a| a.display_name = "Alice Liddell".into());
    let account = |name: &str, version: i64| {
        json!({"uuid": uuid("c:alice"), "changed": ["display_name"], "account": {
            "uuid": uuid("c:alice"), "kind": "carbon", "id": "c:alice", "display_name": name,
            "pfp_url": "http://127.0.0.1/pfp/alice", "version": version}})
    };
    assert_eq!(deliver(&env, "account.updated", account("Alice Liddell", 5)).await, 204);
    let (_, me) = api(&env, "GET", "/api/v2/me", &alice, None).await;
    assert_eq!(me["data"]["display_name"], "Alice Liddell", "{me}");
    // An older version arriving late is dropped.
    assert_eq!(deliver(&env, "account.updated", account("Old Alice", 4)).await, 204);
    assert_eq!(cached(&env, "c:alice").await.1.as_deref(), Some("Alice Liddell"));
}

#[tokio::test]
async fn a_new_custodian_ends_the_access_the_previous_one_gave() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let carol = login(&env, "c:carol").await;
    let chef = login(&env, "si:chef").await;
    let (mine, cred) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:chef"]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (bobs, _) = pair(&env, &bob, DeviceOs::Linux, "Bob's box", &["si:chef"]).await;
    let (s, sess) = session(&env, &chef, &mine).await;
    assert_eq!(s, 201, "{sess}");
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    // alice hands chef over to carol.
    env.accounts()
        .update(&uuid("si:chef"), |a| a.custodian = Some(uuid("c:carol")));
    let status = deliver(
        &env,
        "silicon.custodian_changed",
        json!({"uuid": uuid("si:chef"), "from": {"uuid": uuid("c:alice"), "id": "c:alice"},
               "to": {"uuid": uuid("c:carol"), "id": "c:carol"}}),
    )
    .await;
    assert_eq!(status, 204);
    // The access alice gave chef as its custodian ends, and with it the session.
    let (_, access) = api(&env, "GET", &format!("/api/v2/devices/{mine}/access"), &alice, None).await;
    assert_eq!(access["data"]["items"], json!([]), "{access}");
    let (_, ended) = api(&env, "GET", &format!("/api/v2/sessions/{sid}"), &alice, None).await;
    assert_eq!(ended["data"]["end_reason"], "access_removed", "{ended}");
    // bob's grant stays; his device's log says the custodian changed.
    let (_, access) = api(&env, "GET", &format!("/api/v2/devices/{bobs}/access"), &bob, None).await;
    assert_eq!(access["data"]["items"][0]["silicon_id"], "si:chef", "{access}");
    // carol looks after chef now; alice doesn't.
    let (_, carols) = api(&env, "GET", "/api/v2/silicons", &carol, None).await;
    assert!(
        carols["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "si:chef"),
        "{carols}"
    );
    let (s, e) = api(&env, "GET", "/api/v2/silicons/si:chef/grants", &alice, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (403, Some("no_access")), "{e}");
    let (s, g) = api(&env, "GET", "/api/v2/silicons/si:chef/grants", &carol, None).await;
    assert_eq!(
        (s, g["data"]["items"][0]["device_id"].as_str()),
        (200, Some(bobs.as_str())),
        "{g}"
    );
}

#[tokio::test]
async fn a_deleted_carbons_pairs_end_and_others_history_keeps_a_placeholder() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:chef"]).await;
    let app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, sess) = session(&env, &chef, &d).await;
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    // She turned Extend's notifications on in Ting.
    let (s, t) = api(&env, "PUT", "/api/v2/ting-registration", &alice, None).await;
    assert_eq!(s, 200, "{t}");
    let enrolled = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM extend.ting_enrolments WHERE account_uuid = $1")
            .bind(uuid("c:alice"))
            .fetch_one(&env.pool)
            .await
            .unwrap()
    };
    assert_eq!(enrolled().await, 1);
    assert_eq!(
        deliver(&env, "account.deleted", json!({"uuid": uuid("c:alice")})).await,
        204
    );
    assert_eq!(enrolled().await, 0, "her enrolment record goes with her");
    // Her pair is unpaired; the device app is told.
    app.wait("unpaired", |_| true).await;
    let removed: Option<String> = sqlx::query_scalar("SELECT removed_reason FROM extend.devices WHERE device_id = $1")
        .bind(&d)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(removed.as_deref(), Some("device_removed"));
    // Her sign-ins are refused.
    let (s, _) = api(&env, "GET", "/api/v2/me", &alice, None).await;
    assert_eq!(s, 401);
    // chef's session ended, and chef's view names a deleted account, never her old id.
    let (_, ended) = api(&env, "GET", "/api/v2/sessions", &chef, None).await;
    let item = ended["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == sid.as_str())
        .unwrap()
        .clone();
    assert_eq!(item["state"], "ended", "{item}");
    assert_eq!(cached(&env, "c:alice").await, (String::new(), None));
}

#[tokio::test]
async fn a_deleted_silicons_access_ends_and_its_requests_are_withdrawn() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:chef"]).await;
    let a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    a.send(awake_frame(false, Some("screen_off"), None, uuid::Uuid::new_v4(), 1));
    eventually("asleep", || async {
        api(&env, "GET", &format!("/api/v2/devices/{d}"), &alice, None).await.1["data"]["awake"] == false
    })
    .await;
    let (s, w) = wake(&env, &chef, &d, "the OTP").await;
    assert_eq!(s, 201, "{w}");
    assert_eq!(
        deliver(&env, "account.deleted", json!({"uuid": uuid("si:chef")})).await,
        204
    );
    let (_, access) = api(&env, "GET", &format!("/api/v2/devices/{d}/access"), &alice, None).await;
    assert_eq!(access["data"]["items"], json!([]), "{access}");
    let (_, wakes) = api(&env, "GET", &format!("/api/v2/devices/{d}/wake-requests"), &alice, None).await;
    let item = &wakes["data"]["items"][0];
    assert_eq!(item["state"], "withdrawn", "{wakes}");
    assert!(item["from"].as_str().unwrap().starts_with("deleted account"), "{item}");
    let (s, _) = api(&env, "GET", "/api/v2/me", &chef, None).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn removing_extends_access_ends_what_a_carbon_runs_and_keeps_their_devices() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let sous = login(&env, "si:sous").await;
    let (d, cred) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:sous"]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, sess) = session(&env, &sous, &d).await;
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    assert_eq!(
        deliver(&env, "membership.access_removed", json!({"uuid": uuid("c:alice")})).await,
        204
    );
    let (_, ended) = api(&env, "GET", &format!("/api/v2/sessions/{sid}"), &sous, None).await;
    assert_eq!(ended["data"]["end_reason"], "access_removed", "{ended}");
    // Her old tokens are refused; her device and grants stay for her next sign-in.
    let (s, _) = api(&env, "GET", "/api/v2/devices", &alice, None).await;
    assert_eq!(s, 401);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let again = login(&env, "c:alice").await;
    let (_, access) = api(&env, "GET", &format!("/api/v2/devices/{d}/access"), &again, None).await;
    assert_eq!(access["data"]["items"][0]["silicon_id"], "si:sous", "{access}");
}

#[tokio::test]
async fn retired_webhook_subjects_and_nested_custodians_cannot_return() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    assert_eq!(api(&env, "GET", "/api/v2/me", &chef, None).await.0, 200);
    sqlx::query("INSERT INTO extend.accounts_uuid128_map(old_uuid,new_uuid,kind,mapping_sha256) VALUES('OldRetired','c750a68a-1bc2-4b3f-888e-0349c9d7289a','carbon','test')").execute(&env.pool).await.unwrap();
    for (kind, data) in [
        ("membership.signed_out", json!({"uuid":"OldRetired"})),
        (
            "silicon.custodian_changed",
            json!({"uuid":uuid("si:chef"),"to":{"uuid":"OldRetired","id":"c:retired"}}),
        ),
        (
            "account.updated",
            json!({"uuid":uuid("si:chef"),"account":{"uuid":uuid("si:chef"),"kind":"silicon","id":"si:chef","version":999,"custodian":{"uuid":"OldRetired","id":"c:retired"}}}),
        ),
    ] {
        assert_eq!(deliver(&env, kind, data).await, 204);
    }
    let current: Option<String> = sqlx::query_scalar("SELECT custodian_uuid FROM extend.accounts WHERE uuid=$1")
        .bind(uuid("si:chef"))
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(current, Some(uuid("c:alice")));
    let recreated: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM extend.accounts WHERE uuid='OldRetired')")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert!(!recreated);
}
