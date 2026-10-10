//! Who may do what, on every route family, with no Teams: the Carbon who paired a device (its
//! owner), the Silicons they gave access to (a grant), a Silicon's custodian (the circle: sees and
//! stops what its Silicons do, never acts as them), and everyone else (refused, and told nothing).
//! Silicons are not open to the world: nobody outside a Silicon's circle reaches it unasked.
//!
//! The fixture: c:alice pairs a phone and gives si:chef (hers) and si:scout (c:carol's) access;
//! chef holds a session with a file. c:bob, si:rover (bob's) and si:sous (alice's, no grant) have
//! nothing to do with the phone.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};

struct Fixture {
    env: Env,
    device: String,
    session: String,
    file: String,
    _app: App,
}

async fn fixture() -> Fixture {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (device, cred) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:chef", "si:scout"]).await;
    let app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (s, sess) = session(&env, &chef, &device).await;
    assert_eq!(s, 201, "{sess}");
    let session = sess["data"]["session_id"].as_str().unwrap().to_owned();
    let file = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO extend.files (file_id, device_id, session_id, created_by, shared_with, name, kind, content_type, size_bytes, url, self_destruct_at)
         VALUES ($1::uuid, $2, $3, $4, $5, 'shot.png', 'screenshot', 'image/png', 4, 'http://files.test/shot.png', now() + interval '1 day')",
    )
    .bind(&file)
    .bind(&device)
    .bind(&session)
    .bind(uuid("si:chef"))
    .bind(uuid("c:alice"))
    .execute(&env.pool)
    .await
    .unwrap();
    Fixture {
        env,
        device,
        session,
        file,
        _app: app,
    }
}

impl Fixture {
    /// Calls as `who` and checks the status (and the error code, for a refusal).
    async fn check(
        &self,
        who: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
        want: (u16, Option<&str>),
    ) -> Value {
        let path = path
            .replace("{d}", &self.device)
            .replace("{sid}", &self.session)
            .replace("{fid}", &self.file);
        let token = login(&self.env, who).await;
        let (s, v) = api(&self.env, method, &path, &token, body).await;
        let code = if s >= 400 { v["data"]["code"].as_str() } else { None };
        assert_eq!((s, code), want, "{who} {method} {path}: {v}");
        if s >= 400 && !["c:alice", "si:chef"].contains(&who) {
            // A refusal never names the session's Silicon to someone outside its circle.
            assert!(
                !v.to_string().contains(&uuid("si:chef")),
                "{who} {method} {path} learnt who: {v}"
            );
        }
        v
    }
}

const OK: (u16, Option<&str>) = (200, None);
const NOT_FOUND: (u16, Option<&str>) = (404, Some("device_not_found"));
const NOT_OWNER: (u16, Option<&str>) = (403, Some("not_owner"));
const NO_ACCESS: (u16, Option<&str>) = (403, Some("no_access"));
const SESSION_NOT_FOUND: (u16, Option<&str>) = (404, Some("session_not_found"));
const FILE_NOT_FOUND: (u16, Option<&str>) = (404, Some("file_not_found"));

#[tokio::test]
async fn devices_and_their_access_belong_to_the_carbon_who_paired_them() {
    let f = fixture().await;
    for who in ["c:alice", "si:chef", "si:scout"] {
        f.check(who, "GET", "/api/v2/devices/{d}", None, OK).await;
    }
    for who in ["c:bob", "c:carol", "si:rover", "si:sous"] {
        f.check(who, "GET", "/api/v2/devices/{d}", None, NOT_FOUND).await;
        f.check(who, "GET", "/api/v2/devices/{d}/activity", None, NOT_FOUND)
            .await;
        f.check(who, "GET", "/api/v2/devices/{d}/access", None, NOT_FOUND).await;
    }
    let rename = || Some(json!({"type": "device", "data": {"name": "Renamed"}}));
    f.check("si:chef", "PATCH", "/api/v2/devices/{d}", rename(), NOT_OWNER)
        .await;
    f.check("c:bob", "PATCH", "/api/v2/devices/{d}", rename(), NOT_FOUND)
        .await;
    for who in ["si:chef", "si:scout"] {
        f.check(who, "GET", "/api/v2/devices/{d}/access", None, NOT_OWNER).await;
        f.check(who, "GET", "/api/v2/devices/{d}/activity", None, NOT_OWNER)
            .await;
    }
    for who in ["c:bob", "c:carol"] {
        f.check(who, "PUT", "/api/v2/devices/{d}/access/si:rover", None, NOT_FOUND)
            .await;
        f.check(who, "POST", "/api/v2/devices/{d}/stop", None, NOT_FOUND).await;
        f.check(who, "DELETE", "/api/v2/devices/{d}", None, NOT_FOUND).await;
    }
    f.check(
        "si:chef",
        "PUT",
        "/api/v2/devices/{d}/access/si:rover",
        None,
        (403, Some("carbon_only")),
    )
    .await;
    // A Silicon's list is what it was given; a Carbon's, what they paired.
    let (_, rover) = api(&f.env, "GET", "/api/v2/devices", &login(&f.env, "si:rover").await, None).await;
    assert_eq!(rover["data"]["items"], json!([]));
    let (_, bob) = api(&f.env, "GET", "/api/v2/devices", &login(&f.env, "c:bob").await, None).await;
    assert_eq!(bob["data"]["items"], json!([]));
    f.check("c:alice", "PATCH", "/api/v2/devices/{d}", rename(), OK).await;
}

#[tokio::test]
async fn a_session_is_its_silicons_seen_by_the_device_owner_and_the_silicons_custodian() {
    let f = fixture().await;
    for who in ["si:chef", "c:alice"] {
        f.check(who, "GET", "/api/v2/sessions/{sid}", None, OK).await;
    }
    for who in ["si:scout", "si:sous", "si:rover", "c:bob", "c:carol"] {
        f.check(who, "GET", "/api/v2/sessions/{sid}", None, SESSION_NOT_FOUND)
            .await;
    }
    // Only the Silicon runs commands in it, and takes it over.
    let snapshot = || Some(json!({"type": "command", "data": {"command": "snapshot", "args": []}}));
    f.check("si:chef", "POST", "/api/v2/sessions/{sid}/commands", snapshot(), OK)
        .await;
    f.check(
        "c:alice",
        "POST",
        "/api/v2/sessions/{sid}/commands",
        snapshot(),
        (403, Some("silicon_only")),
    )
    .await;
    f.check(
        "si:scout",
        "POST",
        "/api/v2/sessions/{sid}/commands",
        snapshot(),
        SESSION_NOT_FOUND,
    )
    .await;
    f.check(
        "c:bob",
        "POST",
        "/api/v2/sessions/{sid}/commands",
        snapshot(),
        (403, Some("silicon_only")),
    )
    .await;
    let takeover = || Some(json!({"type": "takeover", "data": {"reason": "Face ID"}}));
    f.check(
        "c:alice",
        "POST",
        "/api/v2/sessions/{sid}/takeover",
        takeover(),
        (403, Some("not_session_owner")),
    )
    .await;
    // The custodian lists its Silicon's sessions; nobody else can.
    let list = f
        .check("c:alice", "GET", "/api/v2/sessions?silicon=si:chef", None, OK)
        .await;
    assert_eq!(list["data"]["items"][0]["session_id"], f.session.as_str());
    for who in ["c:bob", "c:carol"] {
        f.check(who, "GET", "/api/v2/sessions?silicon=si:chef", None, NO_ACCESS)
            .await;
    }
    f.check(
        "si:chef",
        "GET",
        "/api/v2/sessions?silicon=si:chef",
        None,
        (422, Some("invalid_input")),
    )
    .await;
    // The device owner can't end another's session from it (Stop is for that); the custodian can.
    f.check("c:bob", "POST", "/api/v2/sessions/{sid}/end", None, SESSION_NOT_FOUND)
        .await;
    let ended = f.check("c:alice", "POST", "/api/v2/sessions/{sid}/end", None, OK).await;
    assert_eq!(ended["data"]["end_reason"], "stopped_by_carbon");
}

#[tokio::test]
async fn files_are_seen_by_their_silicon_the_device_owner_and_the_custodian() {
    let f = fixture().await;
    for who in ["si:chef", "c:alice"] {
        f.check(who, "GET", "/api/v2/files/{fid}", None, OK).await;
    }
    for who in ["si:scout", "si:rover", "c:bob", "c:carol"] {
        f.check(who, "GET", "/api/v2/files/{fid}", None, FILE_NOT_FOUND).await;
        f.check(who, "POST", "/api/v2/files/{fid}/keep", None, FILE_NOT_FOUND)
            .await;
    }
    let mine = f
        .check("c:alice", "GET", "/api/v2/files?silicon=si:chef", None, OK)
        .await;
    assert_eq!(mine["data"]["items"][0]["file_id"], f.file.as_str());
    f.check("c:carol", "GET", "/api/v2/files?silicon=si:chef", None, NO_ACCESS)
        .await;
    let kept = f.check("c:alice", "POST", "/api/v2/files/{fid}/keep", None, OK).await;
    assert_eq!(kept["data"]["permanent"], true);
}

#[tokio::test]
async fn a_custodian_sees_and_withdraws_its_silicons_access_but_never_acts_as_it() {
    let f = fixture().await;
    f.check("c:alice", "GET", "/api/v2/silicons/si:chef/grants", None, OK)
        .await;
    for who in ["c:bob", "c:carol", "si:chef"] {
        f.check(who, "GET", "/api/v2/silicons/si:chef/grants", None, NO_ACCESS)
            .await;
    }
    f.check("si:chef", "GET", "/api/v2/silicons", None, (403, Some("carbon_only")))
        .await;
    // A Carbon can't start a session (only a Silicon uses devices), even for its own Silicon.
    let start = || Some(json!({"type": "session", "data": {"device_id": f.device}}));
    f.check(
        "c:alice",
        "POST",
        "/api/v2/sessions",
        start(),
        (403, Some("silicon_only")),
    )
    .await;
    // alice owns the device but doesn't look after scout: she revokes, she doesn't renounce.
    f.check(
        "c:alice",
        "DELETE",
        "/api/v2/silicons/si:scout/grants/{d}",
        None,
        NO_ACCESS,
    )
    .await;
    f.check(
        "c:bob",
        "DELETE",
        "/api/v2/silicons/si:scout/grants/{d}",
        None,
        NO_ACCESS,
    )
    .await;
    let token = login(&f.env, "c:carol").await;
    let (s, v) = api(
        &f.env,
        "DELETE",
        &format!("/api/v2/silicons/si:scout/grants/{}", f.device),
        &token,
        None,
    )
    .await;
    assert_eq!(s, 204, "{v}");
    f.check("si:scout", "GET", "/api/v2/devices/{d}", None, NOT_FOUND).await;
}

#[tokio::test]
async fn wake_requests_and_requests_stay_inside_the_circle() {
    let f = fixture().await;
    for who in ["si:rover", "si:sous"] {
        f.check(
            who,
            "POST",
            "/api/v2/devices/{d}/wake-requests",
            Some(json!({"type": "wake_request", "data": {"reason": "x"}})),
            NOT_FOUND,
        )
        .await;
    }
    f.check(
        "c:alice",
        "POST",
        "/api/v2/devices/{d}/wake-requests",
        Some(json!({"type": "wake_request", "data": {"reason": "x"}})),
        (403, Some("silicon_only")),
    )
    .await;
    let w = f
        .check(
            "si:chef",
            "POST",
            "/api/v2/devices/{d}/wake-requests",
            Some(json!({"type": "wake_request", "data": {"reason": "the OTP"}})),
            (201, None),
        )
        .await;
    let wid = w["data"]["wake_id"].as_str().unwrap().to_owned();
    let cancel = format!("/api/v2/devices/{{d}}/wake-requests/{wid}");
    for who in ["c:bob", "c:carol", "si:scout"] {
        f.check(who, "DELETE", &cancel, None, (404, Some("request_not_found")))
            .await;
    }
    let token = login(&f.env, "c:alice").await;
    let (s, v) = api(&f.env, "DELETE", &cancel.replace("{d}", &f.device), &token, None).await;
    assert_eq!(s, 204, "the custodian withdraws its Silicon's request: {v}");
    // scout asks for the phone chef is using: chef is outside scout's circle, so the request goes
    // to alice, who gave chef access; scout learns neither.
    let r = f
        .check(
            "si:scout",
            "POST",
            "/api/v2/devices/{d}/requests",
            Some(json!({"type": "request", "data": {"reason": "need it"}})),
            (201, None),
        )
        .await;
    assert_eq!(
        (r["data"]["to_hidden"].as_bool(), r["data"]["routed_to"].as_str()),
        (Some(true), Some("carbon")),
        "{r}"
    );
    assert!(
        !r.to_string().contains("si:chef") && !r.to_string().contains(&uuid("si:chef")),
        "{r}"
    );
    f.check(
        "si:rover",
        "POST",
        "/api/v2/devices/{d}/requests",
        Some(json!({"type": "request", "data": {"reason": "x"}})),
        NOT_FOUND,
    )
    .await;
}
