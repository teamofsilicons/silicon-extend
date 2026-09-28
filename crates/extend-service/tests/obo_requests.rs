//! The exact requests Extend sends to Briefcase and Ting on a member's behalf, checked against
//! recording stand-ins of both services (no database, no network beyond loopback).
//!
//! The shapes asserted here are the ones a real Briefcase 2.1.0 and Ting 0.1.9 accepted in
//! e2e/real-iam (`realiam.py all --briefcase --ting`): Briefcase's delegated JSON bodies are
//! `deny_unknown_fields` and need an `operation_id`, `write` can only be shared on a folder, a
//! repeated file name becomes a new version of the same entry, and Ting refuses a recipient that
//! never registered the sending app.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use extend_protocol::ErrorCode;
use extend_protocol::model::{AuthSession, Member, MemberKind, RequestRoute, TeamSilicon};
use extend_service::error::AppResult;
use extend_service::files::{BriefcaseFiles, FileStore as _, NewFile};
use extend_service::iam::{Iam, IamEvent, OboProof, Principal, TestingSelection};
use extend_service::ting::{DeviceRequestTing, Notifier as _, TingNotifier};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

/// One proof Extend asked IAM for.
#[derive(Debug, Clone)]
struct ProofAsked {
    audience: String,
    endpoint_id: String,
    member: String,
    metadata: serde_json::Value,
    body_sha256: String,
    proof: String,
}

/// An IAM that only mints OBO proofs, and remembers what each was bound to.
#[derive(Default)]
struct RecordingIam {
    asked: Mutex<Vec<ProofAsked>>,
}

#[async_trait]
impl Iam for RecordingIam {
    fn app_id(&self) -> &str {
        "extend"
    }
    async fn login(&self, _: &str, _: &str, _: Option<&TestingSelection>) -> AppResult<AuthSession> {
        unimplemented!()
    }
    async fn refresh(&self, _: &str, _: &str, _: Option<&TestingSelection>) -> AppResult<AuthSession> {
        unimplemented!()
    }
    async fn logout(&self, _: &str, _: Option<&TestingSelection>) -> AppResult<()> {
        unimplemented!()
    }
    async fn authorize(&self, _: &str, _: Option<&str>, _: Option<&TestingSelection>) -> AppResult<Principal> {
        unimplemented!()
    }
    async fn member_active(
        &self,
        _: &str,
        _: &str,
        _: Option<&Principal>,
        _: Option<&TestingSelection>,
    ) -> AppResult<bool> {
        unimplemented!()
    }
    async fn membership(
        &self,
        _: &str,
        _: &str,
        _: Option<&Principal>,
        _: Option<&TestingSelection>,
    ) -> extend_service::iam::Membership {
        unimplemented!()
    }
    async fn team_silicons(&self, _: &Principal, _: Option<&TestingSelection>) -> AppResult<Vec<TeamSilicon>> {
        unimplemented!()
    }
    async fn select_testing(&self, _: &str) -> AppResult<(Uuid, String)> {
        unimplemented!()
    }
    async fn obo_proof(
        &self,
        principal: &Principal,
        audience: &str,
        endpoint_id: &str,
        metadata: serde_json::Value,
        method: &str,
        body: &[u8],
        _sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof> {
        assert_eq!(method, "POST");
        let mut asked = self.asked.lock().unwrap();
        let proof = format!("obo_test_{}", asked.len());
        asked.push(ProofAsked {
            audience: audience.into(),
            endpoint_id: endpoint_id.into(),
            member: principal.id().into(),
            metadata,
            body_sha256: hex::encode(Sha256::digest(body)),
            proof: proof.clone(),
        });
        Ok(OboProof {
            access_proof: proof,
            testing_app_secret: None,
            testing_iam_key: None,
        })
    }
    async fn verify_webhook(&self, _: &http::HeaderMap, _: &[u8]) -> AppResult<IamEvent> {
        unimplemented!()
    }
}

/// One request a stand-in service received.
#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

type Script = Arc<dyn Fn(&Received) -> (StatusCode, Vec<(&'static str, String)>, Vec<u8>) + Send + Sync>;

/// Starts a loopback server that records every request and answers with `script`.
async fn stand_in(script: Script) -> (String, Arc<Mutex<Vec<Received>>>) {
    let seen: Arc<Mutex<Vec<Received>>> = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
        let log = log.clone();
        let script = script.clone();
        async move {
            assert_eq!(method, Method::POST, "{uri}");
            let r = Received {
                path: uri.path().to_owned(),
                headers,
                body: body.to_vec(),
            };
            log.lock().unwrap().push(r.clone());
            let (status, headers, body) = script(&r);
            let mut resp = (status, body).into_response();
            for (k, v) in headers {
                resp.headers_mut().insert(k, v.parse().unwrap());
            }
            resp
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn json(status: StatusCode, v: serde_json::Value) -> (StatusCode, Vec<(&'static str, String)>, Vec<u8>) {
    (
        status,
        vec![("content-type", "application/json".into())],
        serde_json::to_vec(&v).unwrap(),
    )
}

fn member(id: &str, token: &str) -> Principal {
    let kind = if id.starts_with("si:") {
        MemberKind::Silicon
    } else {
        MemberKind::Carbon
    };
    Principal {
        member: Member {
            kind,
            id: id.into(),
            display_name: None,
        },
        team: Some("acme".into()),
        teams: vec!["acme".into()],
        role: Some("member".into()),
        token: token.into(),
    }
}

/// A Briefcase that creates a new entry for every upload, like the real one does for a new name.
fn briefcase_script() -> Script {
    Arc::new(|r: &Received| match r.path.as_str() {
        "/api/v1/obo/files" => json(
            StatusCode::CREATED,
            serde_json::json!({"id": Uuid::new_v4(), "permanent_url": "https://briefcase.example/org/acme/apps/extend/private/si:chef/x"}),
        ),
        "/api/v1/obo/invitations" => json(
            StatusCode::OK,
            serde_json::json!({"id": Uuid::new_v4(), "access": ["read", "update"]}),
        ),
        "/api/v1/obo/entries/trash" => (StatusCode::NO_CONTENT, vec![], vec![]),
        "/api/v1/obo/files/read" => (
            StatusCode::OK,
            vec![("content-type", "image/png".into())],
            b"\x89PNG bytes".to_vec(),
        ),
        other => panic!("unexpected Briefcase path {other}"),
    })
}

/// The OBO headers Briefcase requires, and nothing it refuses.
fn assert_obo_headers(r: &Received, proof: &str) {
    assert_eq!(r.header("x-app-id"), Some("extend"));
    assert_eq!(r.header("x-iam-obo-access-proof"), Some(proof));
    assert_eq!(r.header("x-org-id"), Some("acme"));
    assert!(
        r.header("authorization").is_none(),
        "Briefcase answers 400 ambiguous_authentication to a bearer next to a proof"
    );
}

#[tokio::test]
async fn stored_files_get_their_own_names_and_are_shared_read_update() {
    let (url, seen) = stand_in(briefcase_script()).await;
    let iam = Arc::new(RecordingIam::default());
    let files = BriefcaseFiles::new(url.clone(), url, iam.clone());
    let chef = member("si:chef", "oat_chef");
    let png = b"\x89PNG one".to_vec();
    let first = files
        .store(
            &chef,
            NewFile {
                name: "screenshot.png",
                content_type: "image/png",
                bytes: png.clone(),
                owner_carbon: "c:alice",
            },
            None,
        )
        .await
        .unwrap();
    let second = files
        .store(
            &chef,
            NewFile {
                name: "screenshot.png",
                content_type: "image/png",
                bytes: b"two".to_vec(),
                owner_carbon: "c:alice",
            },
            None,
        )
        .await
        .unwrap();
    assert_ne!(first.file_id, second.file_id);
    assert_eq!(first.shared_with.as_deref(), Some("c:alice"));

    let asked = iam.asked.lock().unwrap().clone();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(asked.len(), 4);
    assert_eq!(seen.len(), 4);
    // Upload: metadata bound in the proof, raw bytes in the body, digest over exactly those bytes.
    let (upload, created) = (&asked[0], &seen[0]);
    assert_eq!(
        (
            upload.audience.as_str(),
            upload.endpoint_id.as_str(),
            upload.member.as_str()
        ),
        ("briefcase", "briefcase.files.create", "si:chef")
    );
    assert_eq!(created.path, "/api/v1/obo/files");
    assert_eq!(created.body, png);
    assert_eq!(upload.body_sha256, hex::encode(Sha256::digest(&png)));
    assert_eq!(created.header("content-type"), Some("application/octet-stream"));
    assert_obo_headers(created, &upload.proof);
    let name = upload.metadata["name"].as_str().unwrap();
    assert!(name.starts_with("screenshot-") && name.ends_with(".png"), "{name}");
    assert_eq!(upload.metadata["path"], "");
    assert_eq!(upload.metadata["content_type"], "image/png");
    assert_eq!(
        upload.metadata.as_object().unwrap().len(),
        3,
        "Briefcase's schema has exactly path, name, content_type"
    );
    // Briefcase would publish the second upload as a new version of the first if the names matched.
    assert_ne!(asked[2].metadata["name"], upload.metadata["name"]);

    // Sharing: the critical invitation endpoint, empty metadata, the whole operation in the body.
    let (invite, sent) = (&asked[1], &seen[1]);
    assert_eq!(invite.endpoint_id, "briefcase.invitations.create");
    assert_eq!(invite.metadata, serde_json::json!({}));
    assert_eq!(sent.path, "/api/v1/obo/invitations");
    assert_obo_headers(sent, &invite.proof);
    assert_eq!(invite.body_sha256, hex::encode(Sha256::digest(&sent.body)));
    let body = sent.json();
    assert!(
        body["operation_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .is_some_and(|u| !u.is_nil())
    );
    assert_eq!(body["entry_id"], serde_json::json!(first.file_id));
    assert_eq!(
        body["invitation"],
        serde_json::json!({"principal": {"type": "carbon", "id": "c:alice"}, "access": ["read", "update"], "inherit": true}),
        "Briefcase refuses write on a file (invalid_access) and delete is never shared"
    );
    assert_eq!(body.as_object().unwrap().len(), 3);
}

#[tokio::test]
async fn trash_repeats_one_logical_operation_and_treats_gone_as_done() {
    let gone = Arc::new(Mutex::new(false));
    let flag = gone.clone();
    let (url, seen) = stand_in(Arc::new(move |r: &Received| {
        assert_eq!(r.path, "/api/v1/obo/entries/trash");
        if *flag.lock().unwrap() {
            json(
                StatusCode::NOT_FOUND,
                serde_json::json!({"error": {"code": "not_found", "message": "Not found."}}),
            )
        } else {
            (StatusCode::NO_CONTENT, vec![], vec![])
        }
    }))
    .await;
    let iam = Arc::new(RecordingIam::default());
    let files = BriefcaseFiles::new(url.clone(), url, iam.clone());
    let chef = member("si:chef", "oat_chef");
    let id = Uuid::now_v7();
    files.destroy(&chef, id, None).await.unwrap();
    *gone.lock().unwrap() = true;
    files
        .destroy(&chef, id, None)
        .await
        .expect("an entry already in the bin counts as deleted");
    let seen = seen.lock().unwrap().clone();
    let (a, b) = (seen[0].json(), seen[1].json());
    assert_eq!(a, b, "a retry is the same logical deletion");
    assert_eq!(a["entry_id"], serde_json::json!(id));
    assert!(
        a["operation_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .is_some_and(|u| !u.is_nil())
    );
    assert_eq!(
        a.as_object().unwrap().len(),
        2,
        "Briefcase's trash body is deny_unknown_fields"
    );
    let asked = iam.asked.lock().unwrap().clone();
    assert_eq!(asked[0].endpoint_id, "briefcase.entries.trash");
    assert_ne!(
        asked[0].proof, asked[1].proof,
        "every try needs a fresh single-use proof"
    );
    assert_obo_headers(&seen[1], &asked[1].proof);
}

#[tokio::test]
async fn reads_bytes_through_the_delegated_read() {
    let (url, seen) = stand_in(briefcase_script()).await;
    let iam = Arc::new(RecordingIam::default());
    let files = BriefcaseFiles::new(url.clone(), url, iam.clone());
    let alice = member("c:alice", "oat_alice");
    let id = Uuid::now_v7();
    let (bytes, content_type) = files.read(&alice, id, None).await.unwrap();
    assert_eq!(
        (bytes.as_slice(), content_type.as_str()),
        (&b"\x89PNG bytes"[..], "image/png")
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].json(), serde_json::json!({"entry_id": id}));
    let asked = iam.asked.lock().unwrap().clone();
    assert_eq!(
        (asked[0].endpoint_id.as_str(), asked[0].member.as_str()),
        ("briefcase.files.read", "c:alice")
    );
}

#[tokio::test]
async fn refusals_are_explained_and_nothing_is_sent_without_a_login() {
    let (url, seen) = stand_in(Arc::new(|_: &Received| {
        json(StatusCode::UNPROCESSABLE_ENTITY, serde_json::json!({"error": {"code": "invalid_name", "message": "The request contains invalid data.", "request_id": "r-1"}}))
    }))
    .await;
    let iam = Arc::new(RecordingIam::default());
    let files = BriefcaseFiles::new(url.clone(), url, iam.clone());
    let err = files
        .store(
            &member("si:chef", "oat_chef"),
            NewFile {
                name: "a.png",
                content_type: "image/png",
                bytes: vec![1],
                owner_carbon: "c:alice",
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        err.0.message.contains("invalid_name") && err.0.message.contains("r-1") && err.0.message.contains("si:chef"),
        "{}",
        err.0.message
    );
    assert!(err.0.hint.is_some());

    // The scheduler's stand-in principal for a Silicon with no live session has no token.
    let err = files
        .destroy(&member("si:chef", ""), Uuid::now_v7(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotSignedIn);
    assert_eq!(seen.lock().unwrap().len(), 1, "no request without a login");
    assert_eq!(iam.asked.lock().unwrap().len(), 1, "no proof without a login");
}

#[tokio::test]
async fn ting_registers_the_recipient_once_then_sends() {
    let (url, seen) = stand_in(Arc::new(|r: &Received| match r.path.as_str() {
        "/v1/subscriptions" => json(
            StatusCode::CREATED,
            serde_json::json!({"id": "sub_1", "app_id": "extend", "for": "si:chef", "active": true}),
        ),
        "/v1/tings" => json(
            StatusCode::ACCEPTED,
            serde_json::json!({"id": "msg_1", "status": "accepted", "silent": false}),
        ),
        other => panic!("unexpected Ting path {other}"),
    }))
    .await;
    let iam = Arc::new(RecordingIam::default());
    let ting = TingNotifier::new(url, iam.clone());
    let chef = member("si:chef", "oat_chef");
    ting.register_recipient(&chef, false, None).await.unwrap();
    ting.register_recipient(&chef, false, None).await.unwrap();
    let request_id = Uuid::now_v7();
    let reason = "Need it for 2 minutes — \"vendor\" OTP";
    ting.device_request(
        &member("si:sous", "oat_sous"),
        &DeviceRequestTing {
            request_id,
            device_id: "5b6da0ed",
            device_name: "Box",
            from: "si:sous",
            to: "si:chef",
            session_id: Some("708"),
            reason,
            routed_to: Some(RequestRoute::Holder),
            team: Some("acme"),
            link: None,
            from_hidden: false,
        },
        None,
    )
    .await
    .unwrap();

    let seen = seen.lock().unwrap().clone();
    let asked = iam.asked.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "one registration per member per process");
    // Registration is made with the recipient's own proof.
    assert_eq!(
        (
            asked[0].audience.as_str(),
            asked[0].endpoint_id.as_str(),
            asked[0].member.as_str()
        ),
        ("ting", "subscriptions.register", "si:chef")
    );
    assert_eq!(seen[0].path, "/v1/subscriptions");
    assert_eq!(
        seen[0].json(),
        serde_json::json!({"org_id": "acme", "app_id": "extend", "for": "si:chef"})
    );
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {}", asked[0].proof).as_str())
    );
    // The send is made with the sender's proof, bound to the exact bytes.
    assert_eq!(
        (asked[1].endpoint_id.as_str(), asked[1].member.as_str()),
        ("tings.send", "si:sous")
    );
    assert_eq!(asked[1].body_sha256, hex::encode(Sha256::digest(&seen[1].body)));
    let body = seen[1].json();
    let keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    assert!(
        keys.iter()
            .all(|k| ["org_id", "type", "data", "metadata", "for", "key"].contains(k)),
        "Ting refuses unknown fields: {keys:?}"
    );
    assert_eq!(
        (body["org_id"].as_str(), body["type"].as_str(), body["for"].as_str()),
        (Some("acme"), Some("extend.device.requested"), Some("si:chef"))
    );
    assert_eq!(body["key"], serde_json::json!(request_id.to_string()));
    assert_eq!(body["data"]["reason"], reason);
}

#[tokio::test]
async fn ting_explains_an_unregistered_recipient() {
    let (url, _) = stand_in(Arc::new(|_: &Received| {
        json(StatusCode::FORBIDDEN, serde_json::json!({"error": {"code": "recipient_not_registered", "message": "The app does not have permission to notify this recipient."}}))
    }))
    .await;
    let ting = TingNotifier::new(url, Arc::new(RecordingIam::default()));
    let err = ting
        .device_request(
            &member("si:sous", "oat_sous"),
            &DeviceRequestTing {
                request_id: Uuid::now_v7(),
                device_id: "d",
                device_name: "Box",
                from: "si:sous",
                to: "si:chef",
                session_id: None,
                reason: "r",
                routed_to: None,
                team: None,
                link: None,
                from_hidden: false,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NoAccess);
    assert!(
        err.0.message.contains("si:chef has not registered"),
        "{}",
        err.0.message
    );
}

fn routed(request_id: Uuid) -> DeviceRequestTing<'static> {
    DeviceRequestTing {
        request_id,
        device_id: "0d44e1f2",
        device_name: "Family TV",
        from: "si:chef",
        to: "c:bob",
        session_id: None,
        reason: "Need it for an OTP",
        routed_to: Some(RequestRoute::Carbon),
        team: None,
        link: Some("https://extend.teamofsilicons.com/devices/0d44e1f2".into()),
        from_hidden: false,
    }
}

#[tokio::test]
async fn a_frozen_body_ting_already_has_counts_as_delivered() {
    // A retry whose body changed under the same key (a rename since) gets 409 idempotency_conflict:
    // Ting already holds a Ting under that key, so it counts as delivered.
    let (url, seen) = stand_in(Arc::new(|_: &Received| {
        json(
            StatusCode::CONFLICT,
            serde_json::json!({"error": {"code": "idempotency_conflict", "message": "The key was used with other data."}}),
        )
    }))
    .await;
    let ting = TingNotifier::new(url, Arc::new(RecordingIam::default()));
    ting.device_request(&member("si:chef", "oat_chef"), &routed(Uuid::now_v7()), None)
        .await
        .expect("idempotency_conflict is delivered");
    let seen = seen.lock().unwrap().clone();
    let body = seen[0].json();
    // Routed to a Carbon: no holder, no session, no requester Team; the asking Silicon is named.
    assert_eq!(body["data"]["routed_to"], "carbon");
    assert_eq!(body["data"]["from"], "si:chef");
    for gone in ["session_id", "end_session", "team"] {
        assert!(body["data"].get(gone).is_none(), "{gone}: {body}");
    }
}

#[tokio::test]
async fn a_type_ting_does_not_know_names_the_command_for_the_team() {
    let (url, _) = stand_in(Arc::new(|_: &Received| {
        json(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": {"code": "not_found", "message": "Unknown notification type."}}),
        )
    }))
    .await;
    let ting = TingNotifier::new(url, Arc::new(RecordingIam::default()));
    let mut chef = member("si:chef", "oat_chef");
    chef.team = Some("labs".into());
    let err = ting
        .device_request(&chef, &routed(Uuid::now_v7()), None)
        .await
        .unwrap_err();
    assert_eq!(
        extend_service::ting::missing_type(&err).as_deref(),
        Some("extend.device.requested")
    );
    let hint = err.0.hint.unwrap_or_default();
    assert!(
        hint.contains("ting --org '<owning-team>' types register --type extend.device.requested"),
        "{hint}"
    );
}

#[tokio::test]
async fn a_refused_self_send_is_remembered() {
    let (url, _) = stand_in(Arc::new(|_: &Received| {
        json(
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({"error": {"code": "self_send_not_allowed", "message": "A member can't notify themselves."}}),
        )
    }))
    .await;
    let ting = TingNotifier::new(url, Arc::new(RecordingIam::default()));
    let bob = member("c:bob", "oat_bob");
    assert!(!ting.self_send_refused());
    let err = ting
        .device_request(&bob, &routed(Uuid::now_v7()), None)
        .await
        .unwrap_err();
    assert!(extend_service::ting::self_send_refusal(&err));
    assert!(ting.self_send_refused(), "the chains skip self-sends from now on");
}

#[tokio::test]
async fn an_unrelated_self_send_failure_retries_the_same_body_with_a_new_proof() {
    for (status, code) in [
        (StatusCode::UNAUTHORIZED, "unauthorized"),
        (StatusCode::FORBIDDEN, "forbidden"),
        (StatusCode::BAD_REQUEST, "invalid_request"),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_recipient"),
    ] {
        let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (url, seen) = stand_in(Arc::new(move |_: &Received| {
            if failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                json(
                    status,
                    serde_json::json!({"error": {"code":code, "message":"This attempt was refused."}}),
                )
            } else {
                json(StatusCode::ACCEPTED, serde_json::json!({"data": {"accepted": true}}))
            }
        }))
        .await;
        let iam = Arc::new(RecordingIam::default());
        let ting = TingNotifier::new(url, iam.clone());
        let bob = member("c:bob", "oat_bob");
        let body = extend_service::ting::request_body("extend", "acme", &routed(Uuid::now_v7()));
        let err = ting.send_frozen(&bob, &body, None).await.unwrap_err();
        assert!(!extend_service::ting::self_send_refusal(&err), "{status} {code}");
        assert!(
            !ting.self_send_refused(),
            "{status} {code} must not disable later self-sends"
        );
        // The delivery actor chain consults this flag before each attempt. Keeping it clear lets
        // the next attempt recover, using a newly minted single-use proof and the frozen body.
        ting.send_frozen(&bob, &body, None).await.unwrap();
        let seen = seen.lock().unwrap();
        let asked = iam.asked.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(asked.len(), 2);
        assert_eq!(seen[0].body, seen[1].body);
        assert_ne!(asked[0].proof, asked[1].proof);
        assert_eq!(
            seen[0].header("authorization"),
            Some(format!("Bearer {}", asked[0].proof).as_str())
        );
        assert_eq!(
            seen[1].header("authorization"),
            Some(format!("Bearer {}", asked[1].proof).as_str())
        );
        assert!(!ting.self_send_refused());
    }
}
