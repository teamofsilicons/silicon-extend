//! Extend at other apps, against HTTP stand-ins: Ting (sent as Extend with an App verification
//! proof; recipients enrolled with their own User verification proof) and Briefcase (a Silicon's
//! files stored, shared and trashed with its User verification proof). No IAM, no Team header.

mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use common::*;
use extend_protocol::ErrorCode;
use extend_service::files::{BriefcaseFiles, FileStore as _, NewFile, Recipient, trash_operation_id};
use extend_service::ting::{Notifier as _, TingNotifier};
use serde_json::{Value, json};
use uuid::Uuid;

/// Every call a stand-in got: (path, Authorization, body).
type Calls = Arc<Mutex<Vec<(String, String, Value)>>>;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

fn auth(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn forbidden_headers(h: &HeaderMap) {
    for gone in [
        "x-org-id",
        "x-iam-obo-access-token",
        "x-iam-obo-access-proof",
        "x-testing-application-secret",
    ] {
        assert!(!h.contains_key(gone), "{gone} must not be sent");
    }
}

#[tokio::test]
async fn ting_is_sent_as_extend_and_recipients_enrol_with_their_own_proof() {
    let env = start().await;
    let calls: Calls = Arc::default();
    let app = Router::new()
        .route(
            "/v1/tings",
            post(|State(c): State<Calls>, h: HeaderMap, body: Bytes| async move {
                forbidden_headers(&h);
                let b: Value = serde_json::from_slice(&body).unwrap();
                c.lock().unwrap().push(("/v1/tings".into(), auth(&h), b.clone()));
                match b["key"].as_str() {
                    Some("again") => (
                        StatusCode::CONFLICT,
                        axum::Json(json!({"error": {"code": "idempotency_conflict", "message": "seen"}})),
                    )
                        .into_response(),
                    Some("unknown-type") => (
                        StatusCode::NOT_FOUND,
                        axum::Json(json!({"error": {"code": "not_found", "message": "no type"}})),
                    )
                        .into_response(),
                    Some("not-enrolled") => (
                        StatusCode::FORBIDDEN,
                        axum::Json(json!({"error": {"code": "recipient_not_registered", "message": "no"}})),
                    )
                        .into_response(),
                    _ => (StatusCode::CREATED, axum::Json(json!({"id": "tng_1"}))).into_response(),
                }
            }),
        )
        .route(
            "/v1/subscriptions",
            post(
                |State(c): State<Calls>, h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
                    forbidden_headers(&h);
                    c.lock().unwrap().push(("/v1/subscriptions".into(), auth(&h), b));
                    axum::Json(json!({"active": true}))
                },
            ),
        )
        .with_state(calls.clone());
    let base = serve(app).await;
    let ting = TingNotifier::new(base, "extend".into(), env.state.proofs.clone());
    let body = |key: &str| {
        extend_service::ting::body(
            "extend",
            "device.woken",
            &uuid("si:chef"),
            "si:chef",
            key,
            json!({"x": 1}),
        )
    };
    ting.send_frozen(&body("first")).await.unwrap();
    // Ting already has it (a retry after a lost answer): delivered.
    ting.send_frozen(&body("again")).await.unwrap();
    let e = ting.send_frozen(&body("unknown-type")).await.unwrap_err();
    assert_eq!(
        extend_service::ting::missing_type(&e).as_deref(),
        Some("extend.device.woken")
    );
    let e = ting.send_frozen(&body("not-enrolled")).await.unwrap_err();
    assert!(extend_service::ting::not_registered(&e), "{e:?}");
    // One App verification proof, for Ting, to send: reused for every send.
    let local = env.accounts();
    let sends: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.0 == "/v1/tings")
        .cloned()
        .collect();
    assert_eq!(sends.len(), 4);
    assert!(
        sends.iter().all(|c| c.1 == sends[0].1 && c.1.starts_with("Proof sap_")),
        "{sends:?}"
    );
    assert!(local.proof_valid(sends[0].1.trim_start_matches("Proof ")));
    let issued = local.issued_proofs();
    assert_eq!(issued.len(), 1, "{issued:?}");
    assert_eq!(
        (
            issued[0]["kind"].as_str(),
            issued[0]["receiving_app"].as_str(),
            issued[0]["scopes"].clone()
        ),
        (Some("app_verification"), Some("ting"), json!(["tings.send"]))
    );
    // Enrolling chef: chef's own proof, addressed by uuid with its current id.
    let token = login(&env, "si:chef").await;
    let chef = env.state.accounts.authenticate(&token).await.unwrap();
    ting.register_recipient(&chef, false).await.unwrap();
    let (_, proof, sub) = calls
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.0 == "/v1/subscriptions")
        .cloned()
        .unwrap();
    assert_eq!(
        sub,
        json!({"app_id": "extend", "for": uuid("si:chef"), "for_id": "si:chef"})
    );
    assert!(proof.starts_with("Proof sap_") && proof != sends[0].1);
    let user = local
        .issued_proofs()
        .into_iter()
        .find(|p| p["kind"] == "user_verification")
        .unwrap();
    assert_eq!(
        (user["receiving_app"].as_str(), user["user"].as_str()),
        (Some("ting"), Some(uuid("si:chef").as_str()))
    );
    // Once per process for a Silicon (a Carbon's enrolment is recorded in the database).
    ting.register_recipient(&chef, false).await.unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.0 == "/v1/subscriptions")
            .count(),
        1
    );
}

#[derive(Default)]
struct Briefcase {
    calls: Vec<(String, String, Value)>,
}

#[tokio::test]
async fn a_silicons_files_go_to_its_briefcase_with_its_own_proof() {
    let env = start().await;
    let state: Arc<Mutex<Briefcase>> = Arc::default();
    let entry = Uuid::now_v7();
    let upload = Uuid::now_v7();
    let record = |s: &Arc<Mutex<Briefcase>>, path: &str, h: &HeaderMap, b: Value| {
        forbidden_headers(h);
        s.lock().unwrap().calls.push((path.to_owned(), auth(h), b));
    };
    let app = Router::new()
        .route("/api/v1/obo/uploads/reserve", post(move |State(s): State<Arc<Mutex<Briefcase>>>, h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
            record(&s, "reserve", &h, b);
            axum::Json(json!({"state": "reserved", "upload_id": upload, "capability": "cap-1"}))
        }))
        .route("/api/v1/obo/uploads/{id}/content", put(move |State(s): State<Arc<Mutex<Briefcase>>>, h: HeaderMap, Path(id): Path<Uuid>, body: Bytes| async move {
            assert_eq!(id, upload);
            assert_eq!(h["x-briefcase-upload-capability"], "cap-1");
            s.lock().unwrap().calls.push(("content".into(), auth(&h), json!({"bytes": body.len()})));
            axum::Json(json!({"state": "staged", "upload_id": upload}))
        }))
        .route("/api/v1/obo/uploads/commit", post(move |State(s): State<Arc<Mutex<Briefcase>>>, h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
            record(&s, "commit", &h, b);
            axum::Json(json!({"state": "committed", "published_entry_id": entry}))
        }))
        .route("/api/v1/obo/invitations", post(move |State(s): State<Arc<Mutex<Briefcase>>>, h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
            record(&s, "invitations", &h, b);
            axum::Json(json!({"invitation_id": "inv_1"}))
        }))
        .route("/api/v1/obo/entries/trash", post(move |State(s): State<Arc<Mutex<Briefcase>>>, h: HeaderMap, axum::Json(b): axum::Json<Value>| async move {
            record(&s, "trash", &h, b);
            Response::from(StatusCode::NO_CONTENT.into_response())
        }))
        .with_state(state.clone());
    let base = serve(app).await;
    let files = BriefcaseFiles::new(
        base,
        "https://briefcase.example".into(),
        "extend".into(),
        env.state.proofs.clone(),
    );
    let chef = env
        .state
        .accounts
        .authenticate(&login(&env, "si:chef").await)
        .await
        .unwrap();
    // Without a proof held for chef (it never used Extend since), trashing waits.
    let e = files.destroy(&uuid("si:chef"), entry).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::ServiceUnavailable);
    let alice = uuid("c:alice");
    let stored = files
        .store(
            &chef,
            NewFile {
                operation_id: Uuid::now_v7(),
                name: "shot.png",
                content_type: "image/png",
                bytes: b"\x89PNG".to_vec(),
                owner_carbon: Recipient {
                    uuid: &alice,
                    id: "c:alice",
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (stored.file_id, stored.shared_with.as_deref()),
        (entry, Some(alice.as_str()))
    );
    assert!(
        stored
            .url
            .starts_with("https://briefcase.example/si:chef/apps/extend/shot-"),
        "{}",
        stored.url
    );
    let calls = state.lock().unwrap().calls.clone();
    let order: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(order, vec!["reserve", "content", "commit", "invitations"]);
    let proof = &calls[0].1;
    assert!(proof.starts_with("Proof sap_"), "{proof}");
    assert!(
        calls.iter().filter(|c| c.0 != "content").all(|c| &c.1 == proof),
        "one proof for the whole store"
    );
    assert_eq!(
        calls[3].2["invitation"]["principal"],
        json!({"type": "carbon", "id": "c:alice"})
    );
    assert_eq!(calls[3].2["invitation"]["access"], json!(["read", "update"]));
    // The proof is chef's, for Briefcase, with exactly the scopes Extend uses there.
    let issued = env.accounts().issued_proofs();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0]["user"], uuid("si:chef"));
    assert_eq!(
        issued[0]["scopes"],
        json!(extend_service::proofs::BRIEFCASE_WRITE_SCOPES)
    );
    // Self-destruct later, with the proof Extend keeps: the same trash operation every retry.
    files.destroy(&uuid("si:chef"), entry).await.unwrap();
    let trash = state
        .lock()
        .unwrap()
        .calls
        .iter()
        .find(|c| c.0 == "trash")
        .cloned()
        .unwrap();
    assert_eq!(
        trash.2,
        json!({"operation_id": trash_operation_id(entry), "entry_id": entry})
    );
    assert_eq!(&trash.1, proof);
}
