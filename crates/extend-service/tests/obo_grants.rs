//! Real PostgreSQL broker lifecycle with a loopback IAM contract fixture. No real consent or
//! production notifications are created. Exercises retry identities and encrypted plane binding.
mod common;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use extend_protocol::{
    ErrorCode,
    model::{Member, MemberKind},
};
use extend_service::{
    db::{self, World},
    iam::{Principal, TestingSelection},
    obo::{GrantKey, GrantStore, PermissionInput},
};
use futures::FutureExt as _;
use serde_json::{Value, json};
use silicon_iam_client::{Client, Credential, models};
use sqlx::{Connection as _, Row as _};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
struct Fixture {
    starts: usize,
    redeems: usize,
    refresh_keys: Vec<String>,
    refresh_uncertain: bool,
    revoked: bool,
    selected_test: bool,
}
fn actor() -> Value {
    json!({"type":"silicon","public_id":"si:selected"})
}
fn pair(test: bool, refresh: bool) -> Value {
    let mut result = json!({"grant_id":"aa0a11e0-334d-4d94-8791-985d86834d62","access_token":if refresh {"oba_rotated"} else {"oba_initial"},"token_type":"Bearer","expires_in":300,"expires_at":if refresh {"2099-01-01T00:00:00Z"} else {"2020-01-01T00:00:00Z"},"audience":"briefcase","endpoint_id":"briefcase.files.read","org_id":"selected-org","actor":actor(),"scope":"obo:briefcase.files.read","refresh_token":if refresh {"obr_rotated_secret"} else {"obr_initial_secret"}});
    if test {
        result["testing_context"] =
            json!({"app_id":"briefcase","app_secret":format!("ask_{}","a".repeat(43)),"iam_test_key":"b".repeat(32)});
    }
    result
}
async fn authorize(State(s): State<Arc<Mutex<Fixture>>>, Json(body): Json<Value>) -> Json<Value> {
    let mut s = s.lock().unwrap();
    s.starts += 1;
    assert_eq!(body["subject_token"], "oat_initial");
    Json(
        json!({"id":"7b77df91-df6b-4e8f-8028-4c0d6af170bd","app_id":"extend","app_name":"Extend","actor":{"type":"silicon","public_id":"si:chef"},"org_id":"acme","status":"pending","version":1,"expires_at":"2099-01-01T00:00:00Z","endpoints":[],"authorization_url":"https://auth.iam.example/obo/consent"}),
    )
}
async fn tokens(
    State(s): State<Arc<Mutex<Fixture>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut s = s.lock().unwrap();
    if body.get("authorization_code").is_some() {
        if body["authorization_code"] != "approved-fixture-code" {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error":{"code":"invalid_code","message":"invalid"}})),
            );
        }
        s.redeems += 1;
        return (StatusCode::OK, Json(json!({"items":[pair(s.selected_test,false)]})));
    }
    s.refresh_keys
        .push(headers["idempotency-key"].to_str().unwrap().to_owned());
    assert_eq!(body["refresh_token"], "obr_initial_secret");
    if s.revoked {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":{"code":"grant_revoked","message":"revoked"}})),
        );
    }
    if s.refresh_uncertain {
        s.refresh_uncertain = false;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"temporarily_unavailable","message":"uncertain"}})),
        );
    }
    (StatusCode::OK, Json(json!({"items":[pair(s.selected_test,true)]})))
}
fn principal(id: &str) -> Principal {
    Principal {
        member: Member {
            kind: MemberKind::Silicon,
            id: id.into(),
            display_name: None,
        },
        team: Some("acme".into()),
        teams: vec!["acme".into()],
        role: Some("member".into()),
        token: "oat_initial".into(),
    }
}
fn input() -> PermissionInput {
    PermissionInput {
        endpoints: vec![models::OboAuthorizationEndpoint {
            audience: "briefcase".into(),
            endpoint_id: "briefcase.files.read".into(),
        }],
    }
}

#[tokio::test]
async fn durable_grants_keep_secrets_server_side_and_retry_rotation_without_crossing_worlds() {
    let (url, _) = common::database("obo").await;
    let pool = db::connect(&url).await.unwrap();
    let run = std::panic::AssertUnwindSafe(async {
        db::migrate_global(&pool).await.unwrap();
        db::ensure_world(&pool, &World::production()).await.unwrap();
        let selection = TestingSelection {
            environment_id: Uuid::new_v4(),
            name: "isolated".into(),
            secret: format!("ask_{}", "x".repeat(43)),
        };
        let world = World::test(selection.environment_id);
        db::ensure_world(&pool, &world).await.unwrap();
        let fixture = Arc::new(Mutex::new(Fixture {
            refresh_uncertain: true,
            ..Default::default()
        }));
        let router = axum::Router::new()
            .route("/api/v1/obo-access/authorizations", post(authorize))
            .route("/api/v1/obo-access/tokens", post(tokens))
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = Client::builder(&base)
            .unwrap()
            .credential(Credential::application("extend", "test-fixture-app-secret"))
            .telemetry(false)
            .build()
            .unwrap();
        let key = GrantKey::parse(&URL_SAFE_NO_PAD.encode([47u8; 32])).unwrap();
        let store = GrantStore::new(pool.clone(), key.clone());
        let p = principal("si:chef");
        assert_eq!(
            store
                .access(&client, &p, "briefcase", "briefcase.files.read", None)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::ConfirmationRequired
        );
        let retry = Uuid::new_v4();
        let pending = store.start(&client, &p, input(), retry, None).await.unwrap();
        let id: Uuid = serde_json::from_value(pending["id"].clone()).unwrap();
        let mut refreshed = p.clone();
        refreshed.token = "oat_changed".into();
        assert_eq!(
            store.start(&client, &refreshed, input(), retry, None).await.unwrap(),
            pending
        );
        assert_eq!(fixture.lock().unwrap().starts, 1);
        let other = principal("si:other");
        assert_eq!(
            store
                .complete(&client, &other, id, "approved-fixture-code", None)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::NoAccess
        );
        assert_eq!(
            store
                .complete(&client, &p, id, "wrong-code", None)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::ConfirmationRequired
        );
        let complete = store
            .complete(&client, &p, id, "approved-fixture-code", None)
            .await
            .unwrap();
        assert_eq!(complete["items"][0]["actor"], actor());
        assert_eq!(complete["items"][0]["org_id"], "selected-org");
        assert!(!complete.to_string().contains("obr_"));
        assert!(!complete.to_string().contains("oba_"));
        store
            .complete(&client, &p, id, "approved-fixture-code", None)
            .await
            .unwrap();
        assert_eq!(fixture.lock().unwrap().redeems, 1);
        let cipher: Vec<u8> = sqlx::query("SELECT credentials FROM extend.obo_grants")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("credentials");
        assert!(!String::from_utf8_lossy(&cipher).contains("obr_initial_secret"));
        assert_eq!(
            store
                .access(&client, &p, "briefcase", "briefcase.files.read", Some(&selection))
                .await
                .unwrap_err()
                .code(),
            ErrorCode::ConfirmationRequired
        );
        assert_eq!(
            store
                .access(&client, &p, "briefcase", "briefcase.files.read", None)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::ServiceUnavailable
        );
        // Simulate process restart after IAM rotated but the response/transaction was lost.
        let resumed = GrantStore::new(pool.clone(), key);
        let (one, two) = tokio::join!(
            resumed.access(&client, &p, "briefcase", "briefcase.files.read", None),
            resumed.access(&client, &p, "briefcase", "briefcase.files.read", None)
        );
        assert_eq!(one.unwrap().access_proof, "oba_rotated");
        assert_eq!(two.unwrap().access_proof, "oba_rotated");
        let keys = fixture.lock().unwrap().refresh_keys.clone();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], keys[1]);
        // Ordinary login disappearance never deletes feature credentials; receivers still validate
        // the grant on each operation, and IAM refuses renewal of revoked authority.
        let mut no_login = p.clone();
        no_login.token.clear();
        assert_eq!(
            resumed
                .access(&client, &no_login, "briefcase", "briefcase.files.read", None)
                .await
                .unwrap()
                .actor
                .as_deref(),
            Some("si:selected")
        );
        fixture.lock().unwrap().selected_test = true;
        let pending = resumed
            .start(&client, &p, input(), Uuid::new_v4(), Some(&selection))
            .await
            .unwrap();
        let id: Uuid = serde_json::from_value(pending["id"].clone()).unwrap();
        resumed
            .complete(&client, &p, id, "approved-fixture-code", Some(&selection))
            .await
            .unwrap();
        fixture.lock().unwrap().revoked = true;
        assert_eq!(
            resumed
                .access(&client, &p, "briefcase", "briefcase.files.read", Some(&selection))
                .await
                .unwrap_err()
                .code(),
            ErrorCode::ConfirmationRequired
        );
        db::truncate_world(&pool, &world).await.unwrap();
        assert_eq!(resumed.list(&p, Some(&selection)).await.unwrap()["items"], json!([]));
        assert_eq!(
            resumed.list(&p, None).await.unwrap()["items"].as_array().unwrap().len(),
            1
        );
        server.abort();
    })
    .catch_unwind()
    .await;
    pool.close().await;
    let (admin, name) = url.rsplit_once('/').unwrap();
    assert!(name.starts_with("extend_obo_") && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    let mut conn = sqlx::PgConnection::connect(&format!("{admin}/postgres")).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&mut conn)
        .await
        .unwrap();
    if let Err(e) = run {
        std::panic::resume_unwind(e)
    }
}
