//! `auth::SignIn` against a stand-in Silicon Accounts: the device flow (pending, slow_down,
//! approval, denial, expiry), short-lived token exchange (each refusal), refresh rotation, revoke,
//! and that nothing is sent but `client_id`: no secret, no Authorization header, no token echoed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use silicon_extend_client::auth::{self, DeviceProgress, SignIn};
use silicon_extend_client::protocol::model::MemberKind;

#[derive(Default)]
struct Stub {
    /// Answers for successive device-code polls: "pending", "slow_down", "denied", "expired", "tokens".
    polls: Vec<&'static str>,
    /// Refresh tokens that still work, and what each rotates to.
    refresh: HashMap<String, String>,
    /// Every form posted, with whether it carried an Authorization header.
    forms: Vec<(String, HashMap<String, String>, bool)>,
    expires_in: u64,
    interval: u64,
}

type Shared = Arc<Mutex<Stub>>;

fn tokens(refresh: &str, kind: &str) -> Value {
    let (uuid, id) = if kind == "silicon" {
        ("aB3", "si:scout")
    } else {
        ("zQo", "c:ada")
    };
    let mut account = json!({"uuid": uuid, "membership_id": format!("extend:{uuid}"), "kind": kind, "id": id,
        "display_name": "Test Account", "pfp_url": "", "version": 2});
    if kind == "silicon" {
        account["custodian"] = json!({"uuid": "zQo", "id": "c:ada"});
    }
    json!({"access_token": format!("eyJ.access.{refresh}"), "token_type": "Bearer", "expires_in": 1800,
           "refresh_token": refresh, "refresh_token_expires_at": "2029-03-25T02:31:52.745Z",
           "scope": "profile", "membership_id": format!("extend:{uuid}"), "account": account})
}

fn oauth(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        axum::Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

fn form_of(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

async fn authorize(State(s): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut st = s.lock().unwrap();
    let f: HashMap<String, String> = serde_json::from_str::<HashMap<String, Value>>(&body)
        .map(|m| {
            m.into_iter()
                .map(|(k, v)| (k, v.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_else(|_| form_of(&body));
    st.forms
        .push(("authorize".into(), f, headers.contains_key("authorization")));
    axum::Json(json!({"device_code": "sad_stub_device_code", "user_code": "MVHB-KQAW",
        "verification_uri": "http://localhost/device", "verification_uri_complete": "http://localhost/device?code=MVHB-KQAW",
        "expires_in": st.expires_in, "interval": st.interval, "expires_at": "2026-10-10T10:10:00Z"}))
    .into_response()
}

async fn token(State(s): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let f = form_of(&body);
    let mut st = s.lock().unwrap();
    st.forms
        .push(("token".into(), f.clone(), headers.contains_key("authorization")));
    let bad = StatusCode::BAD_REQUEST;
    match f.get("grant_type").map(String::as_str) {
        Some("urn:ietf:params:oauth:grant-type:device_code") => {
            let next = if st.polls.is_empty() {
                "pending"
            } else {
                st.polls.remove(0)
            };
            match next {
                "pending" => oauth(
                    bad,
                    "authorization_pending",
                    "The Carbon hasn't approved this device code yet; keep polling every 5 seconds.",
                ),
                "slow_down" => oauth(bad, "slow_down", "Polling too fast."),
                "denied" => oauth(bad, "access_denied", "The Carbon denied it."),
                "expired" => oauth(bad, "expired_token", "The device code expired."),
                _ => axum::Json(tokens("sar_device_1", "carbon")).into_response(),
            }
        }
        Some("urn:silicon:params:oauth:grant-type:slt") => match f.get("slt").map(String::as_str) {
            Some("slt_good_token_1") => axum::Json(tokens("sar_slt_1", "silicon")).into_response(),
            Some("slt_used_token_1") => oauth(
                bad,
                "invalid_grant",
                "The short-lived token was already used at 2026-10-10T10:00:00Z; it works once.",
            ),
            Some("slt_expired_token") => oauth(
                bad,
                "invalid_grant",
                "The short-lived token expired at 2026-10-10T10:00:00Z (they last 120 seconds).",
            ),
            Some("slt_other_app_tok") => oauth(
                bad,
                "invalid_grant",
                "The short-lived token was issued for the app 'remind', not for 'extend'.",
            ),
            Some("slt_rotated_token") => oauth(
                bad,
                "invalid_grant",
                "The short-lived token was issued at 2026-10-10T09:59:00Z by a sign-in of si:rusty that ended when its custodian rotated its STK at 2026-10-10T10:00:00Z.",
            ),
            Some("slt_public_off_tk") => oauth(
                bad,
                "unauthorized_client",
                "A public client may use refresh_token and the device-code grant; the SLT grant needs public_client on.",
            ),
            _ => oauth(bad, "invalid_grant", "The short-lived token is not known."),
        },
        Some("refresh_token") => {
            let rt = f.get("refresh_token").cloned().unwrap_or_default();
            match st.refresh.remove(&rt) {
                Some(next) => axum::Json(tokens(&next, "carbon")).into_response(),
                None => oauth(
                    bad,
                    "invalid_grant",
                    "This refresh token was already used once. Presenting a used refresh token revokes the whole sign-in to protect the account, so this sign-in is now revoked; sign in again.",
                ),
            }
        }
        _ => oauth(bad, "unsupported_grant_type", "?"),
    }
}

async fn revoke(State(s): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let f = form_of(&body);
    s.lock()
        .unwrap()
        .forms
        .push(("revoke".into(), f, headers.contains_key("authorization")));
    axum::Json(json!({"revoked": true})).into_response()
}

async fn start(stub: Stub) -> (String, Shared) {
    let shared: Shared = Arc::new(Mutex::new(stub));
    let app = Router::new()
        .route("/v1/device/authorize", post(authorize))
        .route("/v1/oauth/token", post(token))
        .route("/v1/oauth/revoke", post(revoke))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, shared)
}

fn stub(polls: Vec<&'static str>) -> Stub {
    Stub {
        polls,
        expires_in: 600,
        interval: 1,
        ..Default::default()
    }
}

/// Every form went with `client_id=extend`, no secret, and no Authorization header.
fn assert_public_client(s: &Shared) {
    for (what, form, authorization) in &s.lock().unwrap().forms {
        assert_eq!(
            form.get("client_id").map(String::as_str),
            Some("extend"),
            "{what}: {form:?}"
        );
        assert!(!form.contains_key("client_secret"), "{what} sent a secret");
        assert!(!authorization, "{what} sent an Authorization header");
    }
}

#[tokio::test]
async fn device_flow_waits_slows_down_and_signs_in() {
    let (url, s) = start(stub(vec!["pending", "slow_down", "tokens"])).await;
    let sign_in = SignIn::new(&url).unwrap();
    let code = sign_in.start_device(Some("extend CLI on test-box")).await.unwrap();
    assert_eq!(code.user_code, "MVHB-KQAW");
    assert_eq!(code.interval, 1);
    assert!(format!("{code:?}").contains("Secret(sad_…)"), "{code:?}");
    let started = Instant::now();
    let mut events = Vec::new();
    let t = sign_in
        .wait_for_device(&code, |e| events.push(e.clone()))
        .await
        .unwrap();
    // 1 s, 1 s, then 6 s after slow_down.
    assert!(started.elapsed() >= Duration::from_secs(7), "{:?}", started.elapsed());
    assert_eq!(
        events,
        vec![
            DeviceProgress::Waiting {
                interval: Duration::from_secs(1)
            },
            DeviceProgress::SlowedDown {
                interval: Duration::from_secs(6)
            },
        ]
    );
    let account = t.account.unwrap();
    assert_eq!(
        (account.uuid.as_str(), account.id.as_str(), account.kind),
        ("zQo", "c:ada", MemberKind::Carbon)
    );
    assert_eq!(t.refresh_token.unwrap().expose(), "sar_device_1");
    assert!(t.refresh_expires_at.is_some());
    let forms = s.lock().unwrap().forms.clone();
    assert_eq!(
        forms[0].1.get("client_label").map(String::as_str),
        Some("extend CLI on test-box")
    );
    assert_eq!(
        forms[1].1.get("device_code").map(String::as_str),
        Some("sad_stub_device_code")
    );
    assert_public_client(&s);
}

#[tokio::test]
async fn a_denied_or_expired_code_says_so() {
    let (url, _) = start(stub(vec!["denied"])).await;
    let sign_in = SignIn::new(&url).unwrap();
    let code = sign_in.start_device(None).await.unwrap();
    let e = sign_in.wait_for_device(&code, |_| {}).await.unwrap_err();
    assert_eq!(e.code, "device_denied");
    assert!(
        e.message.contains("MVHB-KQAW") && e.hint.contains("extend login"),
        "{e:?}"
    );

    let (url, _) = start(stub(vec!["expired"])).await;
    let sign_in = SignIn::new(&url).unwrap();
    let code = sign_in.start_device(None).await.unwrap();
    assert_eq!(
        sign_in.wait_for_device(&code, |_| {}).await.unwrap_err().code,
        "device_expired"
    );

    // Nobody approves, and the code runs out on this side.
    let (url, _) = start(Stub {
        expires_in: 2,
        interval: 1,
        ..Default::default()
    })
    .await;
    let sign_in = SignIn::new(&url).unwrap();
    let code = sign_in.start_device(None).await.unwrap();
    let started = Instant::now();
    let e = sign_in.wait_for_device(&code, |_| {}).await.unwrap_err();
    assert_eq!(e.code, "device_expired");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert!(e.message.contains("expired before anyone approved it"), "{}", e.message);
}

#[tokio::test]
async fn a_short_lived_token_is_exchanged_with_client_id_alone() {
    let (url, s) = start(stub(vec![])).await;
    let sign_in = SignIn::new(&url).unwrap();
    let t = sign_in.exchange_slt("  slt_good_token_1\n").await.unwrap();
    let account = t.account.clone().unwrap();
    assert_eq!((account.id.as_str(), account.kind), ("si:scout", MemberKind::Silicon));
    assert_eq!(account.custodian.unwrap().id, "c:ada");
    assert!(
        !format!("{t:?}").contains("sar_slt_1"),
        "Debug must not print tokens: {t:?}"
    );
    let forms = s.lock().unwrap().forms.clone();
    assert_eq!(forms[0].1.get("grant_type").map(String::as_str), Some(auth::SLT_GRANT));
    assert_eq!(forms[0].1.get("slt").map(String::as_str), Some("slt_good_token_1"));
    assert_public_client(&s);
}

#[tokio::test]
async fn every_refusal_of_a_short_lived_token_says_why() {
    let (url, s) = start(stub(vec![])).await;
    let sign_in = SignIn::new(&url).unwrap();
    for (slt, code, hint) in [
        (
            "slt_used_token_1",
            "slt_already_used",
            "silicon-accounts login --app extend -q | extend login --slt-stdin",
        ),
        ("slt_expired_token", "slt_expired", "Mint a fresh one"),
        ("slt_other_app_tok", "slt_wrong_app", "--app extend"),
        ("slt_unknown_token", "slt_unknown", "copied whole"),
        (
            "slt_rotated_token",
            "slt_sign_in_ended",
            "Sign in to Silicon Accounts again",
        ),
        ("slt_public_off_tk", "public_client_off", "public_client"),
    ] {
        let e = sign_in.exchange_slt(slt).await.unwrap_err();
        assert_eq!(e.code, code, "{slt}: {e:?}");
        assert!(e.hint.contains(hint), "{slt}: {}", e.hint);
        assert!(
            !e.message.contains(slt) && !e.hint.contains(slt),
            "{slt} was echoed: {e:?}"
        );
        assert_eq!(e.status, Some(400));
    }
    let sent = s.lock().unwrap().forms.len();
    // Not short-lived tokens: refused here, never sent.
    for (value, says) in [
        ("sar_a_refresh_token", "refresh token"),
        ("stk-0123456789ab", "STK"),
        ("", "empty"),
        ("oac_old_iam_token_value", "starts with oac_"),
    ] {
        let e = sign_in.exchange_slt(value).await.unwrap_err();
        assert_eq!(e.code, "not_an_slt", "{value}");
        assert!(
            e.message.contains(says) && e.message.contains("Nothing was sent"),
            "{}",
            e.message
        );
        assert!(value.is_empty() || !e.message.contains(value), "{value} was echoed");
    }
    assert_eq!(s.lock().unwrap().forms.len(), sent);
    assert_public_client(&s);
}

#[tokio::test]
async fn refresh_rotates_once_and_a_used_token_ends_the_sign_in() {
    let mut st = stub(vec![]);
    st.refresh.insert("sar_old".into(), "sar_new".into());
    let (url, s) = start(st).await;
    let sign_in = SignIn::new(&url).unwrap();
    let t = sign_in.refresh("sar_old").await.unwrap();
    assert_eq!(t.refresh_token.unwrap().expose(), "sar_new");
    let e = sign_in.refresh("sar_old").await.unwrap_err();
    assert!(e.sign_in_ended(), "{e:?}");
    assert!(
        e.message
            .starts_with("Your sign-in to Extend has ended: This refresh token was already used once"),
        "{}",
        e.message
    );
    assert!(
        e.hint.contains("extend login") && e.hint.contains("--slt-stdin"),
        "{}",
        e.hint
    );
    assert_public_client(&s);
}

#[tokio::test]
async fn revoke_sends_the_refresh_token_with_client_id() {
    let (url, s) = start(stub(vec![])).await;
    let r = SignIn::new(&url).unwrap().revoke("sar_to_revoke").await.unwrap();
    assert!(r.revoked);
    let forms = s.lock().unwrap().forms.clone();
    assert_eq!(forms[0].0, "revoke");
    assert_eq!(forms[0].1.get("token").map(String::as_str), Some("sar_to_revoke"));
    assert_eq!(
        forms[0].1.get("token_type_hint").map(String::as_str),
        Some("refresh_token")
    );
    assert_public_client(&s);
}

#[tokio::test]
async fn an_unreachable_accounts_is_named_and_retryable() {
    // Nothing listens on port 9 here.
    let sign_in = SignIn::new("http://127.0.0.1:9").unwrap();
    let e = sign_in.exchange_slt("slt_good_token_1").await.unwrap_err();
    assert_eq!(e.code, "accounts_unreachable");
    assert!(e.transient && e.message.contains("http://127.0.0.1:9"), "{e:?}");
    let e = sign_in.start_device(None).await.unwrap_err();
    assert_eq!(e.code, "accounts_unreachable");
}

#[test]
fn urls_must_be_https_or_this_machine() {
    assert_eq!(
        auth::check_url("https://accounts.teamofsilicons.com/", "x").unwrap(),
        "https://accounts.teamofsilicons.com"
    );
    for ok in [
        "http://localhost:9590",
        "http://127.0.0.1:4221",
        "http://[::1]:1",
        "http://dev.localhost",
    ] {
        assert!(auth::check_url(ok, "x").is_ok(), "{ok}");
    }
    for bad in [
        "http://example.com",
        "http://localhost.evil.com",
        "http://10.0.2.2:8480",
        "ftp://x",
        "https://x?y=1",
        "https://u:p@x",
        "nope",
    ] {
        assert!(auth::check_url(bad, "x").is_err(), "{bad}");
    }
    let e = SignIn::new("http://example.com").unwrap_err();
    assert_eq!(e.code, "invalid_accounts_url");
    assert!(e.message.contains("unencrypted"), "{}", e.message);
}
