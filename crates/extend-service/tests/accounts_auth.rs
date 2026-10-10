//! Signing in with Silicon Accounts: the access tokens Extend accepts and refuses, the routes that
//! ask Silicon Accounts whether a sign-in is still active, and the account routes of API v1 that
//! 4.0 retired.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use extend_service::accounts::local::{LOCAL_KID, LocalAccounts};
use serde_json::{Value, json};

async fn me(env: &Env, token: &str) -> (u16, Value) {
    api(env, "GET", "/api/v2/me", token, None).await
}

#[tokio::test]
async fn an_access_token_from_silicon_accounts_says_who_is_calling() {
    let env = start().await;
    let (s, v) = me(&env, &login(&env, "si:chef").await).await;
    assert_eq!(s, 200, "{v}");
    let d = &v["data"];
    assert_eq!(
        (d["id"].as_str(), d["uuid"].as_str(), d["type"].as_str()),
        (Some("si:chef"), Some(uuid("si:chef").as_str()), Some("silicon"))
    );
    assert_eq!(
        (d["custodian"]["id"].as_str(), d["custodian"]["uuid"].as_str()),
        (Some("c:alice"), Some(uuid("c:alice").as_str()))
    );
    let (s, v) = me(&env, &login(&env, "c:alice").await).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["type"], "carbon");
    assert!(v["data"].get("custodian").is_none_or(Value::is_null), "{v}");
    // The actor every route sees: uuid, kind and current id, the sign-in family and its scopes.
    let p = env
        .state
        .accounts
        .authenticate(&login(&env, "si:chef").await)
        .await
        .unwrap();
    assert_eq!(
        (p.uuid.as_str(), p.is_silicon(), p.id.as_str(), p.scope.as_deref()),
        (uuid("si:chef").as_str(), true, "si:chef", Some("profile"))
    );
    assert!(p.family.as_deref().is_some_and(|f| f.starts_with("fam_")), "{p:?}");
    assert!(p.issued_at.is_some());
}

#[tokio::test]
async fn tokens_that_are_not_for_extend_now_are_refused() {
    let env = start().await;
    let local = env.accounts();
    let alice = local.ensure("c:alice", None).unwrap();
    // The same claims, signed by a key Silicon Accounts doesn't publish.
    let forger = LocalAccounts::new(&local.issuer, "extend");
    let forged_account = forger.ensure("c:alice", None).unwrap();
    let cases = [
        (
            "another app's",
            local.mint_with(&alice, 1800, "briefcase", &local.issuer, LOCAL_KID),
            "unauthorized",
            "issued to another app",
        ),
        (
            "another issuer's",
            local.mint_with(&alice, 1800, "extend", "https://accounts.example", LOCAL_KID),
            "unauthorized",
            "not issued by the Silicon Accounts",
        ),
        ("an expired", local.mint(&alice, -120), "token_expired", "expired"),
        (
            "an unknown key's",
            local.mint_with(&alice, 1800, "extend", &local.issuer, "retired-key-7"),
            "unauthorized",
            "refused",
        ),
        (
            "a forged",
            forger.mint(&forged_account, 1800),
            "unauthorized",
            "refused",
        ),
        ("a malformed", "not-a.jwt.token".to_owned(), "unauthorized", "refused"),
    ];
    for (what, token, code, says) in cases {
        let (s, e) = me(&env, &token).await;
        assert_eq!((s, e["data"]["code"].as_str()), (401, Some(code)), "{what} token: {e}");
        let message = e["data"]["message"].as_str().unwrap_or_default();
        assert!(message.contains(says), "{what} token: {message}");
        assert!(
            e["data"]["hint"].as_str().is_some_and(|h| !h.is_empty()),
            "{what} token says what to do: {e}"
        );
    }
    // No token at all.
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v2/me", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["data"]["code"], "not_signed_in");
    // An IAM-style bearer for the old Team header changes nothing.
    let (s, _) = api(&env, "GET", "/api/v2/me", "slt_old_iam_token", None).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn sensitive_routes_ask_silicon_accounts_whether_the_sign_in_is_still_active() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (d, _) = pair(&env, &alice, DeviceOs::Android, "Pixel", &[]).await;
    // alice signs out in Silicon Accounts; the event hasn't reached Extend, and the last answer
    // Extend cached (at most 30 s) is gone.
    env.accounts().sign_out(&uuid("c:alice"));
    env.state.accounts.forget(&uuid("c:alice")).await;
    // Reading needs only the token's signature.
    let (s, _) = api(&env, "GET", "/api/v2/devices", &alice, None).await;
    assert_eq!(s, 200);
    // Changing access, or removing a device, asks first.
    for (method, path) in [
        ("PUT", format!("/api/v2/devices/{d}/access/si:chef")),
        ("DELETE", format!("/api/v2/devices/{d}")),
    ] {
        let (s, e) = api(&env, method, &path, &alice, None).await;
        assert_eq!(
            (s, e["data"]["code"].as_str()),
            (401, Some("token_expired")),
            "{method} {path}: {e}"
        );
        assert!(
            e["data"]["message"].as_str().unwrap().contains("no longer active"),
            "{e}"
        );
    }
    // A new sign-in works at once.
    let again = login(&env, "c:alice").await;
    let (s, g) = api(
        &env,
        "PUT",
        &format!("/api/v2/devices/{d}/access/si:chef"),
        &again,
        None,
    )
    .await;
    assert_eq!(s, 200, "{g}");
}

#[tokio::test]
async fn ids_are_resolved_through_silicon_accounts_and_unknown_ones_refused() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (d, _) = pair(&env, &alice, DeviceOs::Android, "Pixel", &[]).await;
    let (s, a) = api(&env, "GET", "/api/v2/accounts/lookup?id=si:scout", &alice, None).await;
    assert_eq!(s, 200, "{a}");
    assert_eq!(
        (a["data"]["id"].as_str(), a["data"]["uuid"].as_str()),
        (Some("si:scout"), Some(uuid("si:scout").as_str()))
    );
    // A grant by uuid is the same grant.
    let (s, g) = api(
        &env,
        "PUT",
        &format!("/api/v2/devices/{d}/access/{}", uuid("si:scout")),
        &alice,
        None,
    )
    .await;
    assert_eq!((s, g["data"]["silicon_id"].as_str()), (200, Some("si:scout")), "{g}");
    for (given, says) in [
        ("si:nobody", "No Silicon si:nobody exists in Silicon Accounts"),
        ("c:bob", "c:bob is not a Silicon id"),
        ("not an id", "is not a Silicon id or uuid"),
    ] {
        let path = format!("/api/v2/devices/{d}/access/{}", given.replace(' ', "%20"));
        let (s, e) = api(&env, "PUT", &path, &alice, None).await;
        assert_eq!(
            (s, e["data"]["code"].as_str()),
            (422, Some("invalid_input")),
            "{given}: {e}"
        );
        assert!(e["data"]["message"].as_str().unwrap().contains(says), "{given}: {e}");
    }
    let (s, e) = api(&env, "GET", "/api/v2/accounts/lookup?id=c:nobody", &alice, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (422, Some("invalid_input")), "{e}");
}

#[tokio::test]
async fn the_account_routes_of_api_v1_are_retired_with_the_update_command() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    for (method, path) in [
        ("GET", "/api/v1/devices"),
        ("POST", "/api/v1/auth/login"),
        ("GET", "/api/v1/iam"),
        ("GET", "/api/v1/sessions/abc"),
        ("POST", "/api/v1/permissions"),
    ] {
        let (s, e) = api(&env, method, path, &alice, None).await;
        assert_eq!(
            (s, e["data"]["code"].as_str(), e["data"]["hint"].as_str()),
            (410, Some("api_version_sunset"), Some("silicon-apps update extend")),
            "{method} {path}: {e}"
        );
        assert_eq!(e["data"]["details"], json!({"retired": path, "use_api_version": 2}));
    }
    // The device wire stays on API v1.
    let (s, e) = device_api(&env, "GET", "/api/v1/device", "edc_not_paired").await;
    assert_eq!((s, e["data"]["code"].as_str()), (401, Some("unauthorized")), "{e}");
    // Silicon IAM's webhook went too.
    let r = reqwest::Client::new()
        .post(format!("{}/webhook/", env.base))
        .json(&json!({"type": "member.removed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 410);
}

#[tokio::test]
async fn retired_uuid_tokens_cannot_recreate_the_old_account() {
    let env = start().await;
    let token = login(&env, "c:alice").await;
    let old = uuid("c:alice");
    sqlx::query(
        "INSERT INTO extend.accounts_uuid128_map(old_uuid,new_uuid,kind,mapping_sha256) VALUES($1,$2,'carbon','test')",
    )
    .bind(&old)
    .bind("a750a68a-1bc2-4b3f-888e-0349c9d7289a")
    .execute(&env.pool)
    .await
    .unwrap();
    let (status, body) = me(&env, &token).await;
    assert_eq!(status, 401, "{body}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.accounts WHERE uuid=$1")
        .bind(old)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn retired_notification_bodies_are_refused_even_if_an_operation_retries() {
    let env = start().await;
    let body = json!({"type":"extend.request","recipient":"OldAccount","value":"frozen"});
    sqlx::query("INSERT INTO extend.accounts_uuid128_retired_bodies(body_sha256) VALUES(sha256(convert_to($1::jsonb::text,'UTF8')))").bind(&body).execute(&env.pool).await.unwrap();
    let attempt = extend_service::delivery::send(&env.state, &extend_service::db::World::production(), &body).await;
    assert!(attempt.disabled);
    assert!(!attempt.delivered && !attempt.tried);
    assert!(attempt.error.unwrap().contains("old notification is retired"));
}
