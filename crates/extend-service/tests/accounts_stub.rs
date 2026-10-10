//! Extend against a stand-in Silicon Accounts served over HTTP, through the official client
//! (`SdkApi`): JWKS (and a rotated key fetched once, rate limited), introspection and lookups with
//! Extend's app credentials, and proofs issued, reused, refreshed with an `Idempotency-Key` derived
//! from the refresh token, sealed at rest and revoked.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use extend_service::accounts::local::LocalAccounts;
use extend_service::config::{AccountsMode, TingMode, Tuning};
use extend_service::proofs::{BRIEFCASE, BRIEFCASE_WRITE_SCOPES, TING, TING_SEND_SCOPES};
use serde_json::{Value, json};

const SECRET: &str = "sa_app_extend_stub_secret";

#[derive(Default)]
struct Stub {
    jwks: Vec<Value>,
    jwks_fetches: usize,
    inactive: HashSet<String>,
    accounts: HashMap<String, Value>,
    /// (path, Idempotency-Key, body) of every call made with app credentials.
    calls: Vec<(String, Option<String>, Value)>,
    issued: usize,
    /// Seconds the next issued proof token lives.
    proof_ttl: i64,
}

type Shared = Arc<Mutex<Stub>>;

fn stamp(t: time::OffsetDateTime) -> String {
    let f = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    t.to_offset(time::UtcOffset::UTC).format(&f).unwrap()
}

/// The refusal of a call without Extend's app credentials (HTTP Basic, as the client sends them).
fn refused(h: &HeaderMap) -> Option<Response> {
    let want = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("extend:{SECRET}"))
    );
    if h.get("authorization").and_then(|v| v.to_str().ok()) == Some(want.as_str()) {
        None
    } else {
        Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"code": "invalid_client", "message": "bad credentials"}})),
            )
                .into_response(),
        )
    }
}

fn sub_of(token: &str) -> String {
    let payload = token.split('.').nth(1).unwrap_or_default();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .unwrap_or_default();
    serde_json::from_slice::<Value>(&bytes).unwrap_or_default()["sub"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn proof(s: &mut Stub, kind: &str, receiving: &str, user: Value, scopes: Value) -> Value {
    s.issued += 1;
    let n = s.issued;
    let now = time::OffsetDateTime::now_utc();
    json!({"proof_id": format!("prf_{n}"), "kind": kind, "proof_token": format!("sap_stub_{n}"),
           "expires_at": stamp(now + time::Duration::seconds(s.proof_ttl)), "proof_refresh_token": format!("sapr_stub_{n}"),
           "refresh_expires_at": stamp(now + time::Duration::days(30)), "issuing_app": "extend",
           "receiving_app": receiving, "user": user, "scopes": scopes})
}

fn record(s: &mut Stub, path: &str, h: &HeaderMap, body: Value) {
    let key = h
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    s.calls.push((path.to_owned(), key, body));
}

fn stub_router(stub: Shared) -> Router {
    Router::new()
        .route(
            "/.well-known/jwks.json",
            get(|State(s): State<Shared>| async move {
                let mut s = s.lock().unwrap();
                s.jwks_fetches += 1;
                Json(json!({"keys": s.jwks}))
            }),
        )
        .route(
            "/v1/oauth/introspect",
            post(
                |State(s): State<Shared>, h: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    let mut s = s.lock().unwrap();
                    let token = f.get("token").cloned().unwrap_or_default();
                    record(&mut s, "introspect", &h, json!({"token": token}));
                    let sub = sub_of(&token);
                    Json(json!({"active": !s.inactive.contains(&sub), "sub": sub, "token_type": "access_token"}))
                        .into_response()
                },
            ),
        )
        .route(
            "/v1/accounts/{uuid}",
            get(
                |State(s): State<Shared>, h: HeaderMap, Path(uuid): Path<String>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    let s = s.lock().unwrap();
                    match s.accounts.get(&uuid) {
                        Some(a) => Json(a.clone()).into_response(),
                        None => (
                            StatusCode::NOT_FOUND,
                            Json(json!({"error": {"code": "not_found", "message": "no account"}})),
                        )
                            .into_response(),
                    }
                },
            ),
        )
        .route(
            "/v1/accounts/by-id/{id}",
            get(
                |State(s): State<Shared>, h: HeaderMap, Path(id): Path<String>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    let s = s.lock().unwrap();
                    match s.accounts.values().find(|a| a["id"] == id.as_str()) {
                        Some(a) => Json(a.clone()).into_response(),
                        None => (
                            StatusCode::NOT_FOUND,
                            Json(json!({"error": {"code": "not_found", "message": "no account"}})),
                        )
                            .into_response(),
                    }
                },
            ),
        )
        .route(
            "/v1/proofs/user-verification",
            post(
                |State(s): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    let mut s = s.lock().unwrap();
                    record(&mut s, "user-verification", &h, b.clone());
                    let sub = sub_of(b["subject_token"].as_str().unwrap_or_default());
                    let user = s.accounts.get(&sub).cloned().unwrap_or(Value::Null);
                    Json(proof(
                        &mut s,
                        "user_verification",
                        b["receiving_app"].as_str().unwrap_or_default(),
                        user,
                        b["scopes"].clone(),
                    ))
                    .into_response()
                },
            ),
        )
        .route(
            "/v1/proofs/app-verification",
            post(
                |State(s): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    let mut s = s.lock().unwrap();
                    record(&mut s, "app-verification", &h, b.clone());
                    Json(proof(
                        &mut s,
                        "app_verification",
                        b["receiving_app"].as_str().unwrap_or_default(),
                        Value::Null,
                        b["scopes"].clone(),
                    ))
                    .into_response()
                },
            ),
        )
        .route(
            "/v1/proofs/refresh",
            post(
                |State(s): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    let mut s = s.lock().unwrap();
                    record(&mut s, "refresh", &h, b.clone());
                    s.proof_ttl = 1800;
                    Json(proof(
                        &mut s,
                        "user_verification",
                        BRIEFCASE,
                        Value::Null,
                        json!(BRIEFCASE_WRITE_SCOPES),
                    ))
                    .into_response()
                },
            ),
        )
        .route(
            "/v1/proofs/revoke",
            post(
                |State(s): State<Shared>, h: HeaderMap, Json(b): Json<Value>| async move {
                    if let Some(r) = refused(&h) {
                        return r;
                    }
                    record(&mut s.lock().unwrap(), "revoke-proof", &h, b);
                    Json(json!({"revoked": true})).into_response()
                },
            ),
        )
        .with_state(stub)
}

/// A signer whose public key the stub publishes under `kid`.
fn jwk(signer: &LocalAccounts, kid: &str) -> Value {
    let mut keys = serde_json::to_value(signer.jwks_value()).unwrap()["keys"][0].clone();
    keys["kid"] = json!(kid);
    keys
}

async fn setup() -> (common::Env, Shared, LocalAccounts, String) {
    let stub: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let signer = LocalAccounts::new(&issuer, "extend");
    let ada = signer.ensure("c:ada", None).unwrap();
    let scout = signer.ensure("si:scout", Some("c:ada")).unwrap();
    {
        let mut s = stub.lock().unwrap();
        s.jwks = vec![jwk(&signer, "k1")];
        s.proof_ttl = 30;
        s.accounts.insert(
            ada.uuid.clone(),
            json!({"uuid": ada.uuid, "kind": "carbon", "id": "c:ada", "display_name": "Ada", "status": "active"}),
        );
        s.accounts.insert(
            scout.uuid.clone(),
            json!({"uuid": scout.uuid, "kind": "silicon", "id": "si:scout", "display_name": "Scout",
            "status": "active", "custodian": {"uuid": ada.uuid, "id": "c:ada", "kind": "carbon"}}),
        );
    }
    let app = stub_router(stub.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let i2 = issuer.clone();
    let env = common::start_config(move |c| {
        c.accounts = AccountsMode::Sdk {
            app_secret: SECRET.into(),
        };
        c.accounts_url = i2.clone();
        c.accounts_api_url = i2;
        c.ting = TingMode::Local;
        c.tuning = Tuning::default();
    })
    .await;
    (env, stub, signer, issuer)
}

#[tokio::test]
async fn tokens_verify_with_published_keys_and_a_rotated_key_is_fetched_once() {
    let (env, stub, signer, issuer) = setup().await;
    let ada = signer.ensure("c:ada", None).unwrap();
    let token = signer.mint_with(&ada, 1800, "extend", &issuer, "k1");
    let (s, me) = common::api(&env, "GET", "/api/v2/me", &token, None).await;
    assert_eq!(
        (s, me["data"]["id"].as_str(), me["data"]["display_name"].as_str()),
        (200, Some("c:ada"), Some("Ada")),
        "{me}"
    );
    // Silicon Accounts rotates in a second key.
    let other = LocalAccounts::new(&issuer, "extend");
    stub.lock().unwrap().jwks.push(jwk(&other, "k2"));
    let fetches = stub.lock().unwrap().jwks_fetches;
    let rotated = other.mint_with(&other.ensure("c:ada", None).unwrap(), 1800, "extend", &issuer, "k2");
    let (s, me) = common::api(&env, "GET", "/api/v2/me", &rotated, None).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(
        stub.lock().unwrap().jwks_fetches,
        fetches + 1,
        "the unknown key made Extend fetch the keys again"
    );
    // A kid nobody published: refused, and not fetched again right away (rate limited).
    let unknown = other.mint_with(&other.ensure("c:ada", None).unwrap(), 1800, "extend", &issuer, "k9");
    let (s, e) = common::api(&env, "GET", "/api/v2/me", &unknown, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (401, Some("unauthorized")), "{e}");
    assert_eq!(stub.lock().unwrap().jwks_fetches, fetches + 1);
}

#[tokio::test]
async fn introspection_and_lookups_use_extends_app_credentials() {
    let (env, stub, signer, issuer) = setup().await;
    let ada = signer.ensure("c:ada", None).unwrap();
    let token = signer.mint_with(&ada, 1800, "extend", &issuer, "k1");
    // Turning Ting notifications on is a sensitive route: it asks first.
    let (s, v) = common::api(&env, "PUT", "/api/v2/ting-registration", &token, None).await;
    assert_eq!(s, 200, "{v}");
    let introspected = stub
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|c| c.0 == "introspect")
        .count();
    assert_eq!(introspected, 1);
    // The answer is cached; once it is gone, a signed-out sign-in is refused.
    stub.lock().unwrap().inactive.insert(ada.uuid.clone());
    assert_eq!(
        common::api(&env, "PUT", "/api/v2/ting-registration", &token, None)
            .await
            .0,
        200
    );
    env.state.accounts.forget(&ada.uuid).await;
    let (s, e) = common::api(&env, "PUT", "/api/v2/ting-registration", &token, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (401, Some("token_expired")), "{e}");
    stub.lock().unwrap().inactive.clear();
    env.state.accounts.forget(&ada.uuid).await;
    // Lookups by id: a Silicon with its custodian; an id nobody has.
    let (s, a) = common::api(&env, "GET", "/api/v2/accounts/lookup?id=si:scout", &token, None).await;
    assert_eq!((s, a["data"]["type"].as_str()), (200, Some("silicon")), "{a}");
    let (s, e) = common::api(&env, "GET", "/api/v2/accounts/lookup?id=si:nobody", &token, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (422, Some("invalid_input")), "{e}");
}

#[tokio::test]
async fn proofs_are_issued_reused_refreshed_once_sealed_and_revoked() {
    let (env, stub, signer, issuer) = setup().await;
    let scout = signer.ensure("si:scout", Some("c:ada")).unwrap();
    let token = signer.mint_with(&scout, 1800, "extend", &issuer, "k1");
    let p = env.state.accounts.authenticate(&token).await.unwrap();
    let proofs = &env.state.proofs;
    // Issued with scout's own access token as the subject (it lives 30 s: due for refresh).
    let first = proofs.for_user(&p, BRIEFCASE, BRIEFCASE_WRITE_SCOPES).await.unwrap();
    {
        let s = stub.lock().unwrap();
        let (path, key, body) = s.calls.iter().find(|c| c.0 == "user-verification").unwrap();
        assert_eq!(path, "user-verification");
        assert!(key.as_deref().is_some_and(|k| k.starts_with("extend-uv-")), "{key:?}");
        assert_eq!(
            (body["subject_token"].as_str(), body["receiving_app"].as_str()),
            (Some(token.as_str()), Some(BRIEFCASE))
        );
    }
    // Next use refreshes it, with the key derived from the refresh token.
    let second = proofs.for_user(&p, BRIEFCASE, BRIEFCASE_WRITE_SCOPES).await.unwrap();
    assert_ne!(first, second);
    let refreshes: Vec<_> = stub
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|c| c.0 == "refresh")
        .cloned()
        .collect();
    assert_eq!(refreshes.len(), 1);
    assert_eq!(refreshes[0].2["proof_refresh_token"], "sapr_stub_1");
    assert_eq!(
        refreshes[0].1.as_deref(),
        Some(extend_service::accounts::api::refresh_idempotency_key("sapr_stub_1").as_str())
    );
    // Reused while it lasts, also for work that happens later without a sign-in.
    assert_eq!(
        proofs.for_user(&p, BRIEFCASE, BRIEFCASE_WRITE_SCOPES).await.unwrap(),
        second
    );
    assert_eq!(
        proofs
            .held(&scout.uuid, BRIEFCASE, BRIEFCASE_WRITE_SCOPES)
            .await
            .unwrap()
            .as_deref(),
        Some(second.as_str())
    );
    // Sealed at rest: the refresh token is never stored in the clear.
    let (cipher,): (Vec<u8>,) =
        sqlx::query_as("SELECT refresh_cipher FROM extend.proof_grants WHERE account_uuid = $1")
            .bind(&scout.uuid)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&cipher).contains("sapr_stub"));
    // Extend as itself.
    proofs.for_app(TING, TING_SEND_SCOPES).await.unwrap();
    proofs.for_app(TING, TING_SEND_SCOPES).await.unwrap();
    assert_eq!(
        stub.lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.0 == "app-verification")
            .count(),
        1
    );
    // The account signs out: what Extend held for it is revoked and forgotten.
    proofs.drop_account(&scout.uuid).await;
    assert!(
        stub.lock()
            .unwrap()
            .calls
            .iter()
            .any(|c| c.0 == "revoke-proof" && c.2["proof_id"] == "prf_2")
    );
    assert_eq!(
        proofs
            .held(&scout.uuid, BRIEFCASE, BRIEFCASE_WRITE_SCOPES)
            .await
            .unwrap(),
        None
    );
}
