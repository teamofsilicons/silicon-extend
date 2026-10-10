//! Extend against a real Silicon Accounts (the local development stack, never production), through
//! the official client: real JWKS, real access tokens, introspection, lookups and sign-out.
//!
//! Skipped unless the stack's values are given. With the shared local stack (accounts on
//! http://localhost:9590, its API on http://127.0.0.1:9589) and a fresh Carbon and Silicon signed
//! in to `extend` (the migration testkit's `mint.mts app-signin --app extend --exchange`, `silicon`,
//! `slt` and `app-token`):
//!
//! ```sh
//! EXTEND_REAL_APP_SECRET=sa_app_extend_… EXTEND_REAL_CARBON_TOKEN=… EXTEND_REAL_SILICON_TOKEN=… \
//!   cargo test -p extend-service --test real_accounts -- --nocapture
//! ```
//!
//! It signs the Silicon out of Extend at the end (its own sign-in only).

mod common;

use common::*;
use extend_protocol::DeviceOs;
use extend_service::config::{AccountsMode, TingMode};
use serde_json::json;

struct Stack {
    url: String,
    api_url: String,
    secret: String,
    carbon: String,
    silicon: String,
}

fn stack() -> Option<Stack> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let stack = Stack {
        url: var("EXTEND_REAL_ACCOUNTS_URL").unwrap_or_else(|| "http://localhost:9590".into()),
        api_url: var("EXTEND_REAL_ACCOUNTS_API_URL").unwrap_or_else(|| "http://127.0.0.1:9589".into()),
        secret: var("EXTEND_REAL_APP_SECRET")?,
        carbon: var("EXTEND_REAL_CARBON_TOKEN")?,
        silicon: var("EXTEND_REAL_SILICON_TOKEN")?,
    };
    for u in [&stack.url, &stack.api_url] {
        let host = url::Url::parse(u)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        assert!(
            ["localhost", "127.0.0.1", "::1"].contains(&host.as_str()),
            "this lane only runs against a local Silicon Accounts, not {u}"
        );
    }
    Some(stack)
}

#[tokio::test]
async fn extend_signs_in_with_a_real_silicon_accounts() {
    let Some(s) = stack() else {
        eprintln!("skipped: set EXTEND_REAL_APP_SECRET, EXTEND_REAL_CARBON_TOKEN and EXTEND_REAL_SILICON_TOKEN");
        return;
    };
    let (url, api_url, secret) = (s.url.clone(), s.api_url.clone(), s.secret.clone());
    let env = start_config(move |c| {
        c.accounts = AccountsMode::Sdk { app_secret: secret };
        c.accounts_url = url;
        c.accounts_api_url = api_url;
        c.ting = TingMode::Local;
    })
    .await;

    // Who the tokens belong to, from the real keys and a real lookup.
    let (st, carbon) = api(&env, "GET", "/api/v2/me", &s.carbon, None).await;
    assert_eq!((st, carbon["data"]["type"].as_str()), (200, Some("carbon")), "{carbon}");
    let (st, silicon) = api(&env, "GET", "/api/v2/me", &s.silicon, None).await;
    assert_eq!(
        (st, silicon["data"]["type"].as_str()),
        (200, Some("silicon")),
        "{silicon}"
    );
    assert_eq!(
        silicon["data"]["custodian"]["uuid"], carbon["data"]["uuid"],
        "{silicon}"
    );
    let silicon_id = silicon["data"]["id"].as_str().unwrap().to_owned();
    eprintln!("signed in: {} and {silicon_id}", carbon["data"]["id"]);

    // Ids resolve through Silicon Accounts; unknown ones are refused.
    let (st, found) = api(
        &env,
        "GET",
        &format!("/api/v2/accounts/lookup?id={silicon_id}"),
        &s.carbon,
        None,
    )
    .await;
    assert_eq!((st, &found["data"]["uuid"]), (200, &silicon["data"]["uuid"]), "{found}");
    let (st, _) = api(
        &env,
        "GET",
        "/api/v2/accounts/lookup?id=si:nobody-extend-lane-0",
        &s.carbon,
        None,
    )
    .await;
    assert_eq!(st, 422);

    // The Carbon pairs a device for the Silicon (sensitive: introspected), and the Silicon uses it.
    let (d, cred) = pair(&env, &s.carbon, DeviceOs::Android, "Lane phone", &[&silicon_id]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (st, sess) = session(&env, &s.silicon, &d).await;
    assert_eq!(st, 201, "{sess}");
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    let (st, r) = api(
        &env,
        "POST",
        &format!("/api/v2/sessions/{sid}/commands"),
        &s.silicon,
        Some(json!({"type": "command", "data": {"command": "snapshot", "args": []}})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    // The custodian sees it.
    let (_, mine) = api(&env, "GET", "/api/v2/silicons", &s.carbon, None).await;
    let summary = mine["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == silicon_id.as_str())
        .cloned();
    assert_eq!(
        summary.as_ref().and_then(|v| v["looked_after"].as_bool()),
        Some(true),
        "{mine}"
    );

    // The Silicon signs out of Extend: Silicon Accounts revokes that sign-in, and its session ends.
    let (st, e) = api(&env, "POST", "/api/v2/auth/logout", &s.silicon, None).await;
    assert_eq!(st, 204, "{e}");
    let (_, ended) = api(&env, "GET", &format!("/api/v2/sessions/{sid}"), &s.carbon, None).await;
    assert_eq!(ended["data"]["end_reason"], "silicon_logged_out", "{ended}");
    // Silicon Accounts now says the token is no longer active.
    let (st, e) = session(&env, &s.silicon, &d).await;
    assert_eq!((st, e["data"]["code"].as_str()), (401, Some("token_expired")), "{e}");
}
