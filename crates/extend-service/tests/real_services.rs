//! Extend's Briefcase and Ting code against the real services of e2e/real-iam, through the official
//! IAM SDK (`SdkIam`): real SLTs, real OBO proofs, a real Briefcase (PostgreSQL + MinIO) and the
//! published Ting server.
//!
//! Skipped unless the fixture is up with both lanes and not yet checked (the check removes si:sous
//! and logs si:chef out):
//!
//! ```sh
//! python3 e2e/real-iam/realiam.py up --briefcase --ting
//! EXTEND_REALIAM_STATE=e2e/real-iam/.state/state.json cargo test -p extend-service --test real_services -- --nocapture
//! python3 e2e/real-iam/realiam.py down
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use extend_protocol::ErrorCode;
use extend_service::files::{BriefcaseFiles, FileStore as _, NewFile};
use extend_service::iam::{Iam as _, Principal, SdkIam};
use extend_service::ting::{DeviceRequestTing, Notifier as _, TingNotifier};
use uuid::Uuid;

struct Fixture {
    dir: PathBuf,
    state: serde_json::Value,
    cli: String,
}

impl Fixture {
    fn load() -> Option<Self> {
        let path = PathBuf::from(std::env::var("EXTEND_REALIAM_STATE").ok()?);
        let path = if path.is_absolute() {
            path
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
        };
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("the fixture's state.json")).unwrap();
        let lanes: Vec<&str> = state["lanes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(
            lanes.contains(&"briefcase") && lanes.contains(&"ting"),
            "bring the fixture up with --briefcase --ting"
        );
        let cli = std::env::var("REALIAM_IAM_CLI")
            .unwrap_or_else(|_| format!("{}/.silicon/bin/iam", std::env::var("HOME").unwrap()));
        Some(Self {
            dir: path.parent().unwrap().to_owned(),
            state,
            cli,
        })
    }

    fn s(&self, pointer: &str) -> String {
        self.state
            .pointer(pointer)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("state{pointer}"))
            .to_owned()
    }

    /// A real SLT from the IAM CLI, issued with the member's fixture profile and approved scopes.
    fn slt(&self, who: &str, app: &str) -> String {
        let mut cmd = std::process::Command::new(&self.cli);
        cmd.args([
            "--url",
            &self.s("/iam_url"),
            "--no-org",
            "--json",
            "login",
            "--app-id",
            app,
            "--grant-org",
            "acme",
            "--approve-scopes",
        ])
        .env("SILICON_HOME", self.dir.join("iam-profiles").join(who));
        for (k, _) in std::env::vars() {
            if k.starts_with("SILICON_IAM_") || k.starts_with("IAM_TEST_") || k == "SILICON_ORG" {
                cmd.env_remove(k);
            }
        }
        let out = cmd.output().expect("the IAM CLI");
        assert!(
            out.status.success(),
            "iam login for {who}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["slt"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn signed_in(&self, iam: &SdkIam, who: &str) -> Principal {
        let session = iam
            .login(&self.slt(who, "extend"), &Uuid::new_v4().to_string(), None)
            .await
            .expect("Extend login through IAM");
        iam.authorize(&session.access_token, Some("acme"), None)
            .await
            .expect("live authorization")
    }

    /// Signs a member in to another application directly (Briefcase or Ting) and returns its token.
    async fn app_token(&self, http: &reqwest::Client, who: &str, app: &str) -> String {
        let (url, path, field) = match app {
            "briefcase" => (self.s("/briefcase/url"), "/api/v1/auth/slt", "access_token"),
            _ => (self.s("/ting/url"), "/v1/session", "session_token"),
        };
        let v: serde_json::Value = http
            .post(format!("{url}{path}"))
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&serde_json::json!({"slt": self.slt(who, app)}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v[field]
            .as_str()
            .unwrap_or_else(|| panic!("{app} sign-in for {who}: {v}"))
            .to_owned()
    }
}

#[tokio::test]
async fn briefcase_and_ting_through_real_obo() {
    let Some(fx) = Fixture::load() else {
        eprintln!("skipped: set EXTEND_REALIAM_STATE to a running e2e/real-iam fixture (--briefcase --ting)");
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let iam = Arc::new(
        SdkIam::connect(&fx.s("/iam_url"), "extend", &fx.s("/app_secret"), None, None)
            .await
            .expect("IAM negotiation"),
    );
    let http = reqwest::Client::new();
    let chef = fx.signed_in(&iam, "chef").await;
    let sous = fx.signed_in(&iam, "sous").await;
    let alice = fx.signed_in(&iam, "alice").await;

    // Briefcase shares only with members it already knows; a Carbon that has used Briefcase is one.
    let bc = fx.s("/briefcase/url");
    let alice_bc = fx.app_token(&http, "alice", "briefcase").await;
    let status = http
        .get(format!("{bc}/api/v1/entries"))
        .bearer_auth(&alice_bc)
        .header("X-Org-ID", "acme")
        .send()
        .await
        .unwrap()
        .status();
    assert!(status.is_success(), "c:alice lists Briefcase: {status}");

    let files = BriefcaseFiles::new(bc.clone(), bc.clone(), iam.clone());
    let text = format!("real services {}", Uuid::new_v4()).into_bytes();
    let one = files
        .store(
            &chef,
            NewFile {
                name: "real-services.txt",
                content_type: "text/plain",
                bytes: text.clone(),
                owner_carbon: "c:alice",
            },
            None,
        )
        .await
        .expect("briefcase.files.create through a real proof");
    let two = files
        .store(
            &chef,
            NewFile {
                name: "real-services.txt",
                content_type: "text/plain",
                bytes: b"second".to_vec(),
                owner_carbon: "c:alice",
            },
            None,
        )
        .await
        .unwrap();
    assert_ne!(
        one.file_id, two.file_id,
        "the same device name must not become a second version of one entry"
    );
    assert_eq!(
        one.shared_with.as_deref(),
        Some("c:alice"),
        "briefcase.invitations.create (read, update)"
    );
    assert!(
        one.url
            .starts_with(&format!("{bc}/org/acme/apps/extend/private/si:chef/real-services-")),
        "{}",
        one.url
    );
    eprintln!("stored {} at {}", one.file_id, one.url);

    let grants: serde_json::Value = http
        .get(format!("{bc}/api/v1/entries/{}/permissions", one.file_id))
        .bearer_auth(&alice_bc)
        .header("X-Org-ID", "acme")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let shared: Vec<_> = grants["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["principal"]["id"] == "c:alice")
        .collect();
    assert_eq!(shared.len(), 1, "{grants}");
    assert_eq!(shared[0]["access"], serde_json::json!(["read", "update"]));

    let (bytes, content_type) = files
        .read(&chef, one.file_id, None)
        .await
        .expect("briefcase.files.read as the Silicon");
    assert_eq!(
        (bytes.as_slice(), content_type.starts_with("text/plain")),
        (text.as_slice(), true)
    );
    let (bytes, _) = files
        .read(&alice, one.file_id, None)
        .await
        .expect("briefcase.files.read as the device owner");
    assert_eq!(bytes, text);

    files
        .destroy(&chef, one.file_id, None)
        .await
        .expect("briefcase.entries.trash");
    files
        .destroy(&chef, one.file_id, None)
        .await
        .expect("trashing again is done, not an error");
    let err = files.read(&chef, one.file_id, None).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::FileNotFound, "{}", err.0.message);
    assert_eq!(files.read(&chef, two.file_id, None).await.unwrap().0, b"second");
    files.destroy(&chef, two.file_id, None).await.unwrap();

    // Ting: the recipient registers Extend with its own proof, then a request reaches its inbox.
    let ting = TingNotifier::new(fx.s("/ting/url"), iam.clone());
    ting.register_recipient(&chef, false, None)
        .await
        .expect("subscriptions.register as si:chef");
    let request_id = Uuid::now_v7();
    let reason = format!("real services check {request_id} — \"exact\" ünïcode");
    ting.device_request(
        &sous,
        &DeviceRequestTing {
            request_id,
            device_id: "0000abcd",
            device_name: "Test box",
            from: "si:sous",
            to: "si:chef",
            session_id: None,
            reason: &reason,
            routed_to: Some(extend_protocol::model::RequestRoute::Holder),
            team: Some("acme"),
            link: None,
            from_hidden: false,
        },
        None,
    )
    .await
    .expect("tings.send as si:sous");
    let session = fx.app_token(&http, "chef", "ting").await;
    let inbox: serde_json::Value = http
        .get(format!("{}/v1/orgs/acme/inbox?app_id=extend", fx.s("/ting/url")))
        .bearer_auth(&session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let got: Vec<_> = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["key"] == request_id.to_string())
        .collect();
    assert_eq!(got.len(), 1, "{inbox}");
    assert_eq!(
        (got[0]["type"].as_str(), got[0]["for"].as_str()),
        (Some("extend.device.requested"), Some("si:chef"))
    );
    assert_eq!(got[0]["data"]["reason"].as_str(), Some(reason.as_str()));
    eprintln!("ting {} delivered to si:chef", got[0]["id"]);
}
