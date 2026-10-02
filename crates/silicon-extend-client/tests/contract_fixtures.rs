//! Consumer-driven contract fixtures for this crate (UNDERSTANDING.md "Versioning" 4;
//! TECHNICAL.md section 10).
//!
//! Every public call of the crate runs against a recording stand-in for Extend that answers with
//! the response the crate expects. For each call this test writes
//! `contracts/v{api}/client/<operation>.json`: the method, the path, the headers that matter, the
//! body, the provider state the call needs (`given`), and which response fields the crate can't do
//! without. Those are found by taking each field out of the expected response in turn (and then
//! setting it to null) and seeing whether the call still decodes. Test-only values (tokens, ids,
//! codes) are written as placeholders such as `{device_id}` that the service's replay fills in.
//!
//! The service's `contracts` test (`crates/extend-service/tests/contracts.rs`) replays every
//! fixture of every API major it still serves against a real service, so a service change that
//! would break a published client fails there.
//!
//! This test fails when the fixtures on disk differ from what the crate sends now. After an
//! intended change, rewrite them and commit `contracts/`:
//!
//! ```text
//! EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use extend_protocol::model::*;
use extend_protocol::{API_VERSION, DeviceOs};
use serde_json::{Value, json};
use silicon_extend_client::{ActivityQuery, Client, DeviceQuery, Error, ListQuery};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

// ───────────── Sentinels and the placeholders they become ─────────────

const CARBON_TOKEN: &str = "sentinel-carbon-access-token";
const SILICON_TOKEN: &str = "sentinel-silicon-access-token";
const OTHER_SILICON_TOKEN: &str = "sentinel-other-silicon-access-token";
const OTHER_CARBON_TOKEN: &str = "sentinel-other-carbon-access-token";
const REFRESH_TOKEN: &str = "sentinel-refresh-token";
const CARBON_SLT: &str = "sentinel-carbon-slt";
const TEAM: &str = "sentinel-team";
const SILICON_ID: &str = "si:sentinel-silicon";
const DEVICE_ID: &str = "d0d0d0d0";
const HOST_ID: &str = "b0b0b0b0";
const ATTACHED_ID: &str = "c0c0c0c0";
/// Another Carbon's pair of the same device (`shared_device`).
const SHARED_DEVICE_ID: &str = "e0e0e0e0";
const SESSION_ID: &str = "a5a";
const DEVICE_CREDENTIAL: &str = "edc_sentinel-device-credential";
const ENROLLMENT_SECRET: &str = "ees_sentinel-enrollment-secret";
const ENROLLMENT_ID: &str = "11111111-1111-4111-8111-111111111111";
const UPLOAD_ID: &str = "22222222-2222-4222-8222-222222222222";
const FILE_ID: &str = "33333333-3333-4333-8333-333333333333";
const WAKE_ID: &str = "55555555-5555-4555-8555-555555555555";
const PAIRING_CODE: &str = "A1B2C3";
const TESTING_SECRET: &str = "ask_sentinel-testing-secret";
const PERMISSION_ID: &str = "77777777-7777-4777-8777-777777777777";
const PERMISSION_CODE: &str = "obc_sentinel-permission-code";
const PERMISSION_KEY: &str = "88888888-8888-4888-8888-888888888888";
const ISI: &str = "sentinel-isi";
const DEVICE_VERSION: i64 = 4242;

/// Longest first, so no sentinel is replaced inside another.
const PLACEHOLDERS: &[(&str, &str)] = &[
    (OTHER_SILICON_TOKEN, "{other_silicon_token}"),
    (OTHER_CARBON_TOKEN, "{other_carbon_token}"),
    (CARBON_TOKEN, "{carbon_token}"),
    (SILICON_TOKEN, "{silicon_token}"),
    (REFRESH_TOKEN, "{refresh_token}"),
    (CARBON_SLT, "{carbon_slt}"),
    (DEVICE_CREDENTIAL, "{device_credential}"),
    (ENROLLMENT_SECRET, "{enrollment_secret}"),
    (TESTING_SECRET, "{testing_secret}"),
    (PERMISSION_ID, "{permission_id}"),
    (PERMISSION_CODE, "{permission_code}"),
    (ENROLLMENT_ID, "{enrollment_id}"),
    (UPLOAD_ID, "{upload_id}"),
    (FILE_ID, "{file_id}"),
    (WAKE_ID, "{wake_id}"),
    (SILICON_ID, "{silicon_id}"),
    (TEAM, "{team}"),
    (ISI, "{isi}"),
    (DEVICE_ID, "{device_id}"),
    (HOST_ID, "{host_id}"),
    (ATTACHED_ID, "{attached_id}"),
    (SHARED_DEVICE_ID, "{shared_device_id}"),
    (PAIRING_CODE, "{pairing_code}"),
];

/// Headers that only describe the transport.
const TRANSPORT_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "accept",
    "accept-encoding",
    "user-agent",
    "connection",
];

fn placeholders(s: &str) -> String {
    let mut out = s.to_owned();
    for (sentinel, placeholder) in PLACEHOLDERS {
        out = out.replace(sentinel, placeholder);
    }
    // A session id is too short to replace inside other text: only whole path segments and
    // whole query values.
    let (path, query) = match out.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(q.to_owned())),
        None => (out, None),
    };
    let path = path
        .split('/')
        .map(|seg| if seg == SESSION_ID { "{session_id}" } else { seg })
        .collect::<Vec<_>>()
        .join("/");
    match query {
        None => path,
        Some(q) => {
            let q = q
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((k, v)) if v == SESSION_ID => format!("{k}={{session_id}}"),
                    _ => pair.to_owned(),
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{path}?{q}")
        }
    }
}

/// Standard padded base64, for a binary request body.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn placeholders_json(v: &Value) -> Value {
    match v {
        Value::String(s) if s == SESSION_ID => Value::String("{session_id}".into()),
        Value::String(s) => Value::String(placeholders(s)),
        Value::Array(a) => Value::Array(a.iter().map(placeholders_json).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), placeholders_json(v))).collect()),
        other => other.clone(),
    }
}

// ───────────── A recording stand-in for Extend ─────────────

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Default)]
struct StubState {
    status: u16,
    body: Option<Vec<u8>>,
    log: Vec<Seen>,
}

#[derive(Clone, Default)]
struct Stub(Arc<Mutex<StubState>>);

impl Stub {
    async fn start() -> (String, Stub) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stub = Stub::default();
        let s = stub.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let s = s.clone();
                tokio::spawn(async move {
                    let _ = s.handle(sock).await;
                });
            }
        });
        (base, stub)
    }

    fn reply(&self, status: u16, kind: &str, data: Option<&Value>) {
        let mut st = self.0.lock().unwrap();
        st.status = status;
        st.body = data.map(|d| serde_json::to_vec(&json!({"type": kind, "data": d})).unwrap());
        st.log.clear();
    }

    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut self.0.lock().unwrap().log)
    }

    async fn handle(&self, mut sock: TcpStream) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let head_end = loop {
            let mut chunk = [0u8; 8192];
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next().unwrap_or_default().split(' ');
        let method = request_line.next().unwrap_or_default().to_owned();
        let target = request_line.next().unwrap_or_default().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        assert!(
            !headers.iter().any(|(k, _)| k == "transfer-encoding"),
            "the recorder reads Content-Length bodies only"
        );
        let len: usize = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        let mut body = buf[head_end + 4..].to_vec();
        while body.len() < len {
            let mut chunk = vec![0u8; len - body.len()];
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        let (status, reply) = {
            let mut st = self.0.lock().unwrap();
            st.log.push(Seen {
                method,
                target,
                headers,
                body,
            });
            (st.status, st.body.clone().unwrap_or_default())
        };
        let mut head = format!(
            "HTTP/1.1 {status} Recorded\r\ncontent-length: {}\r\nconnection: close\r\n",
            reply.len()
        );
        if !reply.is_empty() {
            head.push_str("content-type: application/json\r\n");
        }
        head.push_str("\r\n");
        sock.write_all(head.as_bytes()).await?;
        sock.write_all(&reply).await?;
        sock.shutdown().await
    }
}

// ───────────── The calls ─────────────

type Fut<'a> = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>>;

struct Ctx {
    client: Client,
    /// Built with a test-environment secret.
    testing: Client,
    /// Built with an internal Silicon id and telemetry off.
    options: Client,
}

impl Ctx {
    fn carbon(&self) -> silicon_extend_client::Authed<'_> {
        self.client.authed(CARBON_TOKEN, Some(TEAM))
    }
    fn silicon(&self) -> silicon_extend_client::Authed<'_> {
        self.client.authed(SILICON_TOKEN, Some(TEAM))
    }
    fn other_silicon(&self) -> silicon_extend_client::Authed<'_> {
        self.client.authed(OTHER_SILICON_TOKEN, Some(TEAM))
    }
    /// c:bob, who paired the same device as c:alice (`shared_device`).
    fn other_carbon(&self) -> silicon_extend_client::Authed<'_> {
        self.client.authed(OTHER_CARBON_TOKEN, Some(TEAM))
    }
}

struct Op {
    /// The fixture's name.
    operation: &'static str,
    /// The crate's method.
    call: &'static str,
    /// Provider state the replay sets up first (see `crates/extend-service/tests/contracts.rs`).
    given: &'static [&'static str],
    status: u16,
    kind: &'static str,
    data: Option<Value>,
    run: for<'a> fn(&'a Ctx) -> Fut<'a>,
}

macro_rules! op {
    ($operation:literal, $call:literal, [$($given:literal),*], $status:literal, $kind:literal, $data:expr, |$c:ident| $body:expr) => {{
        fn run<'a>($c: &'a Ctx) -> Fut<'a> {
            Box::pin(async move { $body.await.map(|_| ()) })
        }
        Op {
            operation: $operation,
            call: $call,
            given: &[$($given),*],
            status: $status,
            kind: $kind,
            data: $data,
            run,
        }
    }};
}

const TS: &str = "2026-09-26T10:00:00.123Z";
const LATER: &str = "2026-09-26T10:05:00.123Z";

fn member(kind: &str, id: &str) -> Value {
    json!({"type": kind, "id": id, "display_name": "Sentinel"})
}

fn env_view() -> Value {
    json!({"environment_id": "44444444-4444-4444-8444-444444444444", "name": "checkout-e2e", "state": "ready",
           "paired_devices": 1, "device_limit": 5})
}

fn setup() -> Value {
    json!({"state": "needs_carbon", "steps": [
        {"key": "wireless_debugging", "title": "Turn on wireless debugging", "status": "needs_carbon",
         "help": "Settings › System › Developer options", "error": "Not yet", "input": "code"}
    ]})
}

fn device() -> Value {
    let mut d = json!({
        "device_id": DEVICE_ID, "name": "Pixel", "os": "android", "os_version": "15", "model": "Pixel 9",
        "kind": "phone", "owner": member("carbon", "c:alice"), "team": TEAM, "visibility": "team",
        "host_device_id": HOST_ID, "state": "ready", "online": true, "last_seen_at": TS,
        "in_use": {"silicon_id": SILICON_ID, "session_id": SESSION_ID, "since": TS, "paused": false, "team": TEAM},
        "last_used_at": TS, "paired_at": TS, "pair_ttl_days": 14, "pair_expires_at": LATER, "days_left": 13,
        "access_count": 2, "app_version": "1.0.0", "version": DEVICE_VERSION,
        "capabilities": ["screen.read"], "missing": [{"capability": "adb", "reason": "Wireless debugging is off."}],
        "commands": ["snapshot"], "removed_at": TS, "removed_reason": "device_removed"
    });
    let v1_1 = json!({
        "engine_version": "1.1.0", "agent_device_version": "1.1.0", "awake": false, "sleep_state": "standby",
        "last_sleep_state": "asleep", "awake_changed_at": TS, "wake_detectable": true, "in_use_by_other": true,
        "in_use_by_other_carried": true, "open_wake_requests": 1, "wake_requests": [wake_request()],
        "wake_muted": false, "paired_by_others": true, "same_device": [HOST_ID]
    });
    d.as_object_mut().unwrap().extend(v1_1.as_object().unwrap().clone());
    d
}

fn wake_request() -> Value {
    json!({
        "wake_id": WAKE_ID, "device_id": DEVICE_ID, "team": TEAM, "from": SILICON_ID, "to": "c:alice",
        "reason": "Need the TV awake to check the menu", "created_at": TS, "last_asked_at": TS, "asks": 2,
        "expires_at": LATER, "state": "open", "ended_at": LATER, "end_reason": "woken_on_device",
        "wake_detectable": false, "device_notice": "not_shown", "device_notice_note": "Notifications are off.",
        "ting": "delivered", "ting_covered_by": WAKE_ID, "ting_last_error": "sentinel", "answer_ting": "pending",
        "answer_ting_last_error": "sentinel", "host": {"device_id": HOST_ID, "name": "Studio Mac", "online": true}
    })
}

fn ting_registration() -> Value {
    json!({"team": TEAM, "member": "c:alice", "status": "pending", "registered_at": TS, "refused_at": TS,
           "last_error": "Sign in to Extend for sentinel-team", "missing_types": ["extend.device.wake_requested"]})
}

fn permissions() -> Value {
    json!({"items": [{"audience":"briefcase", "endpoint_id":"briefcase.uploads.reserve", "grant_id": PERMISSION_ID,
        "org_id":TEAM, "actor":{"public_id":"c:alice", "kind":"carbon"}, "expires_at":LATER}]})
}

fn enrollment_created() -> Value {
    json!({"enrollment_id": ENROLLMENT_ID, "enrollment_secret": ENROLLMENT_SECRET, "pairing_code": PAIRING_CODE,
           "code_expires_at": LATER, "rotates_every_s": 300})
}

fn session() -> Value {
    json!({
        "session_id": SESSION_ID, "device_id": DEVICE_ID, "silicon_id": SILICON_ID, "state": "active",
        "started_at": TS, "last_command_at": TS, "idle_ends_at": LATER, "ended_at": LATER,
        "end_reason": "ended_by_silicon", "command_count": 3, "device": device(),
        "capabilities": ["screen.read"], "commands": ["snapshot"], "team": TEAM
    })
}

fn file() -> Value {
    json!({
        "file_id": FILE_ID, "name": "screenshot.png", "kind": "screenshot", "content_type": "image/png",
        "size_bytes": 1024, "url": "https://briefcase.example/f/1", "self_destruct_at": LATER, "permanent": false,
        "session_id": SESSION_ID, "device_id": DEVICE_ID, "command_id": UPLOAD_ID, "created_by": SILICON_ID,
        "shared_with": "c:alice", "created_at": TS, "team": TEAM
    })
}

fn takeover() -> Value {
    json!({"takeover_id": UPLOAD_ID, "session_id": SESSION_ID, "reason": "Please approve Face ID",
           "started_at": TS, "expires_at": LATER})
}

fn request_info() -> Value {
    json!({"request_id": UPLOAD_ID, "device_id": DEVICE_ID, "from": "si:sous", "to": SILICON_ID,
           "session_id": SESSION_ID, "reason": "Need it for an OTP", "created_at": TS, "delivery": "delivered",
           "last_error": "Ting was unavailable", "team": TEAM, "routed_to": "carbon", "to_hidden": true,
           "from_hidden": true})
}

fn grant() -> Value {
    json!({"device_id": DEVICE_ID, "silicon_id": SILICON_ID, "granted_by": "c:alice", "granted_at": TS,
           "last_used_at": TS, "team": TEAM, "wake_muted": true})
}

fn auth_session() -> Value {
    json!({"access_token": CARBON_TOKEN, "refresh_token": REFRESH_TOKEN, "token_type": "Bearer", "expires_in": 900,
           "member": member("carbon", "c:alice"), "teams": [TEAM], "testing_environment": env_view()})
}

fn page(item: Value) -> Value {
    json!({"items": [item], "next_cursor": "c2VudGluZWw"})
}

fn command() -> CommandRequest {
    CommandRequest {
        command: "snapshot".into(),
        args: vec!["-i".into()],
        timeout_ms: Some(30_000),
        self_destruct_minutes: Some(60),
        permanent: false,
        attachments: vec![],
    }
}

fn ops() -> Vec<Op> {
    vec![
        op!(
            "permissions.list",
            "Authed::permissions",
            ["permission_grant"],
            200,
            "permissions",
            Some(permissions()),
            |c| c.carbon().permissions()
        ),
        op!(
            "permissions.start",
            "Authed::request_permissions",
            [],
            200,
            "permission",
            Some(json!({"id":PERMISSION_ID,"consent_url":"https://iam.example/obo/consent","expires_at":LATER})),
            |c| c.carbon().request_permissions(
                &[silicon_extend_client::PermissionEndpoint {
                    audience: "briefcase".into(),
                    endpoint_id: "briefcase.uploads.reserve".into()
                }],
                PERMISSION_KEY
            )
        ),
        op!(
            "permissions.complete",
            "Authed::complete_permissions",
            ["permission_request"],
            200,
            "permissions",
            Some(permissions()),
            |c| c
                .carbon()
                .complete_permissions(PERMISSION_ID.parse().unwrap(), PERMISSION_CODE, PERMISSION_KEY)
        ),
        op!(
            "version.negotiate",
            "Client::connect",
            [],
            200,
            "version",
            Some(json!({"api_version": 1, "supported": [1], "service_version": "1.0.0", "deprecated": []})),
            |c| Client::connect(c.client.base_url())
        ),
        op!(
            "iam.get",
            "Client::iam",
            [],
            200,
            "iam",
            Some(
                json!({"app_id": "extend", "iam_base_url": "https://iam.example", "iam_login_url": "https://auth.example/login",
                        "api_base_url": "https://api.example", "website_url": "https://extend.example",
                        "docs_url": "https://extend.example/docs", "repository_url": "https://github.com/x/y",
                        "testing_environment": env_view()})
            ),
            |c| c.client.iam()
        ),
        op!(
            "contracts.get",
            "Client::contracts",
            [],
            200,
            "contracts",
            Some(json!({"versions": [{"api_version": 1, "state": "current"}]})),
            |c| c.client.contracts()
        ),
        op!(
            "auth.login",
            "Client::login",
            [],
            200,
            "login",
            Some(auth_session()),
            |c| c.client.login(CARBON_SLT)
        ),
        op!(
            "auth.refresh",
            "Client::refresh",
            ["refresh_token"],
            200,
            "refresh",
            Some(auth_session()),
            |c| c.client.refresh(REFRESH_TOKEN, "sentinel-idempotency-key")
        ),
        op!(
            "auth.logout",
            "Client::logout",
            ["refresh_token"],
            204,
            "logout",
            None,
            |c| c.client.logout(REFRESH_TOKEN, Some(CARBON_TOKEN))
        ),
        op!(
            "testing.environment",
            "Client::testing_environment",
            ["test_environment"],
            200,
            "testing_environment",
            Some(env_view()),
            |c| c.testing.testing_environment()
        ),
        op!(
            "enrollments.create",
            "Client::enroll",
            [],
            201,
            "enrollment",
            Some(enrollment_created()),
            |c| c.client.enroll(&EnrollmentCreate {
                os: DeviceOs::Android,
                os_version: Some("15".into()),
                model: Some("Pixel 9".into()),
                app_version: "1.1.0".into(),
                engine_version: Some("0.13.0".into()),
            })
        ),
        op!(
            "device.enrollments.create",
            "Client::pair_enrollment",
            ["device"],
            201,
            "enrollment",
            Some(enrollment_created()),
            |c| c.client.pair_enrollment(DEVICE_CREDENTIAL)
        ),
        op!(
            "enrollments.get",
            "Client::enrollment",
            ["enrollment"],
            200,
            "enrollment_state",
            Some(json!({"state": "waiting", "pairing_code": PAIRING_CODE, "code_expires_at": LATER})),
            |c| c.client.enrollment(ENROLLMENT_ID.parse().unwrap(), ENROLLMENT_SECRET)
        ),
        op!(
            "device.self",
            "Client::device_self",
            ["device"],
            200,
            "device_self",
            Some(
                json!({"device_id": DEVICE_ID, "name": "Pixel", "owner": member("carbon", "c:alice"), "team": TEAM,
                        "os": "android", "in_use": {"silicon_id": SILICON_ID, "session_id": SESSION_ID, "since": TS},
                        "takeover": takeover(), "setup": setup(), "environment": env_view(),
                        "instance_id": ENROLLMENT_ID, "hardware_salt": "sentinel-salt", "first_pair": true})
            ),
            |c| c.client.device_self(DEVICE_CREDENTIAL)
        ),
        op!(
            "device.banner",
            "Client::update_device_self",
            ["device"],
            200,
            "device_self",
            Some(
                json!({"device_id": DEVICE_ID, "name": "Pixel", "owner": member("carbon", "c:alice"), "team": TEAM,
                "os": "android", "in_use": null, "takeover": null, "setup": setup(), "environment": null, "in_use_indicator": "hidden"})
            ),
            |c| c.client.update_device_self(
                DEVICE_CREDENTIAL,
                &DeviceSelfPatch::in_use_indicator(InUseIndicator::Hidden)
            )
        ),
        op!(
            "devices.banner",
            "Authed::set_in_use_indicator",
            ["device"],
            200,
            "device",
            Some({
                let mut d = device();
                d["in_use_indicator"] = json!("hidden");
                d
            }),
            |c| c.carbon().set_in_use_indicator(DEVICE_ID, InUseIndicator::Hidden)
        ),
        op!("device.revoke", "Client::revoke_pair", ["device"], 204, "", None, |c| c
            .client
            .revoke_pair(DEVICE_CREDENTIAL)),
        op!("device.stop", "Client::device_stop", ["session"], 204, "", None, |c| c
            .client
            .device_stop(DEVICE_CREDENTIAL)),
        op!(
            "device.artifact",
            "Client::upload_artifact",
            ["upload"],
            201,
            "",
            None,
            |c| c.client.upload_artifact(
                DEVICE_CREDENTIAL,
                UPLOAD_ID.parse().unwrap(),
                "shot.png",
                "image/png",
                b"\x89PNG contract fixture".to_vec()
            )
        ),
        op!(
            "auth.me",
            "Authed::me",
            [],
            200,
            "me",
            Some(
                json!({"authenticated": true, "member": member("carbon", "c:alice"), "teams": [TEAM], "team": TEAM,
                        "team_role": "member", "testing_environment": env_view()})
            ),
            |c| c.carbon().me()
        ),
        op!(
            "auth.me.options",
            "Authed::me",
            [],
            200,
            "me",
            Some(json!({"authenticated": true, "member": member("silicon", SILICON_ID), "teams": [TEAM]})),
            |c| c.options.authed(SILICON_TOKEN, Some(TEAM)).me()
        ),
        op!(
            "devices.list",
            "Authed::devices",
            ["device"],
            200,
            "devices",
            Some(page(device())),
            |c| c.carbon().devices(DeviceQuery {
                scope: Some("mine".into()),
                online: None,
                os: Some("android".into()),
                limit: Some(10),
                cursor: None,
            })
        ),
        op!(
            "devices.list.including_removed",
            "Authed::devices_including_removed",
            ["device"],
            200,
            "devices",
            Some(page(device())),
            |c| c.carbon().devices_including_removed(DeviceQuery {
                scope: Some("mine".into()),
                limit: Some(10),
                ..Default::default()
            })
        ),
        op!(
            "devices.get",
            "Authed::device",
            ["device"],
            200,
            "device",
            Some(device()),
            |c| c.carbon().device(DEVICE_ID)
        ),
        op!(
            "pairings.claim",
            "Authed::pair",
            ["enrollment"],
            201,
            "device",
            Some(device()),
            |c| c.carbon().pair(&PairingClaim {
                pairing_code: PAIRING_CODE.into(),
                name: "Pixel".into(),
                visibility: Some(Visibility::Team),
                pair_ttl_days: Some(14),
                silicon_ids: vec![SILICON_ID.into()],
            })
        ),
        op!(
            "devices.attach",
            "Authed::attach",
            ["host"],
            201,
            "device",
            Some(device()),
            |c| c.carbon().attach(
                HOST_ID,
                &AttachmentCreate {
                    os: DeviceOs::Tvos,
                    name: "Living room".into(),
                    visibility: Some(Visibility::Team),
                    pair_ttl_days: Some(7),
                    address: Some("192.168.1.20".into()),
                }
            )
        ),
        op!(
            "devices.update",
            "Authed::update_device",
            ["device"],
            200,
            "device",
            Some(device()),
            |c| c.carbon().update_device(
                DEVICE_ID,
                Some(DEVICE_VERSION),
                &DevicePatch {
                    name: Some("Studio phone".into()),
                    visibility: Some(Visibility::Personal),
                    pair_ttl_days: Some(20),
                }
            )
        ),
        op!(
            "devices.remove",
            "Authed::remove_device",
            ["device"],
            204,
            "",
            None,
            |c| c.carbon().remove_device(DEVICE_ID, Some(DEVICE_VERSION))
        ),
        op!(
            "devices.stop",
            "Authed::stop_device",
            ["session"],
            200,
            "session",
            Some(session()),
            |c| c.carbon().stop_device(DEVICE_ID)
        ),
        op!(
            "devices.stop.outcome",
            "Authed::stop",
            ["session"],
            200,
            "session",
            Some(session()),
            |c| c.carbon().stop(DEVICE_ID)
        ),
        op!(
            "devices.stop.device_stopped",
            "Authed::stop",
            ["shared_device", "session"],
            200,
            "device_stopped",
            Some(json!({"device_id": SHARED_DEVICE_ID, "stopped_at": TS, "in_use_by_other": true})),
            |c| c.other_carbon().stop(SHARED_DEVICE_ID)
        ),
        op!(
            "team.silicons",
            "Authed::team_silicons",
            [],
            200,
            "team_silicons",
            Some(json!({"items": [{"id": SILICON_ID, "display_name": "Chef"}]})),
            |c| c.carbon().team_silicons()
        ),
        op!(
            "team.silicons.all",
            "Authed::team_silicons_all",
            [],
            200,
            "team_silicons",
            Some(json!({
                "items": [{"id": SILICON_ID, "display_name": "Chef", "team": TEAM}],
                "teams": [{"team": TEAM, "ok": true},
                          {"team": "globex", "ok": false, "error": {"code": "not_a_team_member", "message": "Sign in to Extend for globex.",
                                                                    "hint": "sentinel", "request_id": "req-sentinel"}}]
            })),
            |c| c.carbon().team_silicons_all()
        ),
        op!(
            "devices.setup",
            "Authed::setup",
            ["device"],
            200,
            "setup",
            Some(setup()),
            |c| c.carbon().setup(DEVICE_ID)
        ),
        op!(
            "devices.setup_code",
            "Authed::setup_code",
            ["attached"],
            200,
            "setup",
            Some(setup()),
            |c| c.carbon().setup_code(ATTACHED_ID, "1234")
        ),
        op!(
            "devices.setup_retry",
            "Authed::retry_setup",
            ["failed_setup"],
            202,
            "setup_retry",
            Some(json!({"retrying": ["wireless_debugging"]})),
            |c| c.carbon().retry_setup(DEVICE_ID, None)
        ),
        op!(
            "access.list",
            "Authed::access",
            ["device"],
            200,
            "access",
            Some(json!({"items": [grant()]})),
            |c| c.carbon().access(DEVICE_ID)
        ),
        op!(
            "access.grant",
            "Authed::grant",
            ["device"],
            200,
            "access",
            Some(grant()),
            |c| c.carbon().grant(DEVICE_ID, SILICON_ID)
        ),
        op!("access.revoke", "Authed::revoke", ["device"], 204, "", None, |c| c
            .carbon()
            .revoke(DEVICE_ID, SILICON_ID)),
        op!(
            "access.revoke.team",
            "Authed::revoke_in_team",
            ["device"],
            204,
            "",
            None,
            |c| c.carbon().revoke_in_team(DEVICE_ID, SILICON_ID, TEAM)
        ),
        op!(
            "devices.activity",
            "Authed::activity",
            ["device"],
            200,
            "activity",
            Some(page(
                json!({"id": UPLOAD_ID, "at": TS, "actor": member("silicon", SILICON_ID), "action": "command",
                             "session_id": SESSION_ID, "command": "fill", "args": ["@e3", "[redacted 7 chars]"],
                             "outcome": "ok", "files": [FILE_ID], "details": {"why": "sentinel"}, "team": TEAM})
            )),
            |c| c.carbon().activity(
                DEVICE_ID,
                ActivityQuery {
                    silicon_id: Some(SILICON_ID.into()),
                    limit: Some(20),
                    ..Default::default()
                }
            )
        ),
        op!(
            "wake_requests.create",
            "Authed::wake",
            ["device"],
            201,
            "wake_request",
            Some(wake_request()),
            |c| c.silicon().wake(DEVICE_ID, "Need the TV awake to check the menu")
        ),
        op!(
            "wake_requests.list",
            "Authed::wake_requests",
            ["wake_request"],
            200,
            "wake_requests",
            Some(page(wake_request())),
            |c| c.carbon().wake_requests(
                DEVICE_ID,
                ListQuery {
                    state: Some("open".into()),
                    limit: Some(20),
                    ..Default::default()
                }
            )
        ),
        op!(
            "wake_requests.cancel",
            "Authed::cancel_wake",
            ["wake_request"],
            204,
            "",
            None,
            |c| c.silicon().cancel_wake(DEVICE_ID, WAKE_ID.parse().unwrap())
        ),
        op!(
            "wake_requests.answer",
            "Authed::answer_wake",
            ["wake_request"],
            200,
            "wake_answer",
            Some(json!({"answer": "declined", "ended": [wake_request()]})),
            |c| c.carbon().answer_wake(
                DEVICE_ID,
                &WakeAnswer::declined().wake_ids(vec![WAKE_ID.parse().unwrap()])
            )
        ),
        op!(
            "wake_settings.update",
            "Authed::set_wake_settings",
            ["device"],
            200,
            "wake_settings",
            Some(
                json!({"device_id": DEVICE_ID, "muted": false, "silicons_muted": [{"silicon_id": SILICON_ID, "team": TEAM}]})
            ),
            |c| c
                .carbon()
                .set_wake_settings(DEVICE_ID, &WakeSettings::new(true).silicon(SILICON_ID).team(TEAM))
        ),
        op!(
            "ting.registration.get",
            "Authed::ting_registration",
            [],
            200,
            "ting_registration",
            Some(ting_registration()),
            |c| c.carbon().ting_registration(TEAM)
        ),
        op!(
            "ting.registration.all",
            "Authed::ting_registrations",
            [],
            200,
            "ting_registrations",
            Some(page(ting_registration())),
            |c| c.carbon().ting_registrations()
        ),
        op!(
            "ting.registration.turn_on",
            "Authed::ting_turn_on",
            [],
            200,
            "ting_registration",
            Some(ting_registration()),
            |c| c.carbon().ting_turn_on(TEAM)
        ),
        op!(
            "devices.requests",
            "Authed::device_requests",
            ["device"],
            200,
            "requests",
            Some(page(request_info())),
            |c| c.carbon().device_requests(
                DEVICE_ID,
                ListQuery {
                    limit: Some(20),
                    ..Default::default()
                }
            )
        ),
        op!(
            "requests.send",
            "Authed::send_request",
            ["session"],
            201,
            "request",
            Some(request_info()),
            |c| c
                .other_silicon()
                .send_request(DEVICE_ID, "Need it for an OTP, 2 minutes")
        ),
        op!(
            "requests.list",
            "Authed::requests",
            ["session"],
            200,
            "requests",
            Some(page(request_info())),
            |c| c.silicon().requests(ListQuery {
                direction: Some("received".into()),
                device_id: Some(DEVICE_ID.into()),
                limit: Some(20),
                ..Default::default()
            })
        ),
        op!(
            "sessions.start",
            "Authed::start_session",
            ["device"],
            201,
            "session",
            Some(session()),
            |c| c.silicon().start_session(&DEVICE_ID.parse().unwrap())
        ),
        op!(
            "sessions.list",
            "Authed::sessions",
            ["session"],
            200,
            "sessions",
            Some(page(session())),
            |c| c.silicon().sessions(ListQuery {
                device_id: Some(DEVICE_ID.into()),
                state: Some("active".into()),
                limit: Some(20),
                ..Default::default()
            })
        ),
        op!(
            "sessions.get",
            "Authed::session",
            ["session"],
            200,
            "session",
            Some(session()),
            |c| c.silicon().session(SESSION_ID)
        ),
        op!(
            "sessions.end",
            "Authed::end_session",
            ["session"],
            200,
            "session",
            Some(session()),
            |c| c.silicon().end_session(SESSION_ID)
        ),
        op!(
            "sessions.takeover",
            "Authed::takeover",
            ["session"],
            201,
            "takeover",
            Some(takeover()),
            |c| c.silicon().takeover(SESSION_ID, "Please approve Face ID")
        ),
        op!(
            "sessions.takeover_status",
            "Authed::takeover_status",
            ["takeover"],
            200,
            "takeover",
            Some(takeover()),
            |c| c.silicon().takeover_status(SESSION_ID)
        ),
        op!(
            "sessions.takeover_release",
            "Authed::release_takeover",
            ["takeover"],
            204,
            "",
            None,
            |c| c.silicon().release_takeover(SESSION_ID)
        ),
        op!(
            "sessions.command",
            "Authed::run",
            ["session"],
            200,
            "command_result",
            Some(
                json!({"command_id": UPLOAD_ID, "session_id": SESSION_ID, "command": "snapshot", "ok": true,
                        "output": {"nodes": 3}, "text": "3 nodes", "files": [file()],
                        "error": {"code": "none", "message": "sentinel", "details": {"a": 1}},
                        "started_at": TS, "duration_ms": 120, "idle_ends_at": LATER, "warnings": ["sentinel"]})
            ),
            |c| c.silicon().run(SESSION_ID, &command())
        ),
        op!(
            "files.list",
            "Authed::files",
            ["file"],
            200,
            "files",
            Some(page(file())),
            |c| c.silicon().files(ListQuery {
                session_id: Some(SESSION_ID.into()),
                kind: Some("screenshot".into()),
                limit: Some(20),
                ..Default::default()
            })
        ),
        op!("files.get", "Authed::file", ["file"], 200, "file", Some(file()), |c| c
            .silicon()
            .file(FILE_ID)),
        op!(
            "files.keep",
            "Authed::keep_file",
            ["file"],
            200,
            "file",
            Some(file()),
            |c| c.silicon().keep_file(FILE_ID)
        ),
        op!(
            "files.content",
            "Authed::file_content",
            ["file"],
            200,
            "",
            Some(json!("file bytes")),
            |c| c.silicon().file_content(FILE_ID)
        ),
        op!(
            "files.content.range",
            "Authed::file_download",
            ["file"],
            206,
            "",
            Some(json!("file")),
            |c| c.silicon().file_download(FILE_ID, Some((0, Some(3))))
        ),
        op!(
            "reports.create",
            "Authed::report",
            [],
            202,
            "report",
            Some(
                json!({"report_id": UPLOAD_ID, "notification": "simulated", "repository_url": "https://github.com/x/y"})
            ),
            |c| c.silicon().report(&ReportInput {
                message: "snapshot misses a button".into(),
                pr: Some("https://github.com/teamofsilicons/silicon-extend/pull/1".into()),
                client_version: "1.0.0".into(),
                context: json!({"command": "snapshot"}),
            })
        ),
        op!("telemetry.send", "Authed::telemetry", [], 204, "", None, |c| c
            .silicon()
            .telemetry(json!({
                "source": "cli", "event": "command", "step": "device ls", "success": true, "duration_ms": 120,
                "error_code": null, "command": "device ls", "client_version": "1.0.0"
            }))),
    ]
}

/// Public calls that are not requests to the Extend API, with why.
const NOT_API_CALLS: &[(&str, &str)] = &[
    (
        "download",
        "fetches a file by the URL Extend returned (Briefcase, or /dev/files locally), not an Extend API path",
    ),
    (
        "chunk",
        "FileDownload::chunk reads the answer to a file_download request already made",
    ),
    (
        "content",
        "FileDownload::content reads the answer to a file_download request already made",
    ),
];

// ───────────── Required response fields, found by mutation ─────────────

#[derive(Debug, Clone)]
enum Seg {
    Key(String),
    Each,
}

fn render(path: &[Seg]) -> String {
    path.iter()
        .map(|s| match s {
            Seg::Key(k) => format!("/{k}"),
            Seg::Each => "/*".into(),
        })
        .collect()
}

/// Every object field in `v`, with arrays walked through their elements.
fn field_paths(v: &Value, prefix: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    match v {
        Value::Object(o) => {
            for (k, child) in o {
                prefix.push(Seg::Key(k.clone()));
                out.push(prefix.clone());
                field_paths(child, prefix, out);
                prefix.pop();
            }
        }
        Value::Array(a) => {
            if let Some(first) = a.first() {
                prefix.push(Seg::Each);
                field_paths(first, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

fn mutate(v: &mut Value, path: &[Seg], remove: bool) {
    match (v, path) {
        (Value::Object(o), [Seg::Key(k)]) => {
            if remove {
                o.remove(k);
            } else if let Some(slot) = o.get_mut(k) {
                *slot = Value::Null;
            }
        }
        (Value::Object(o), [Seg::Key(k), rest @ ..]) => {
            if let Some(child) = o.get_mut(k) {
                mutate(child, rest, remove);
            }
        }
        (Value::Array(a), [Seg::Each, rest @ ..]) => {
            for child in a {
                mutate(child, rest, remove);
            }
        }
        _ => {}
    }
}

// ───────────── Building and checking fixtures ─────────────

fn contracts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts")
        .join(format!("v{API_VERSION}"))
        .join("client")
}

fn fixture(op: &Op, seen: &Seen, required: Vec<String>, non_null: Vec<String>) -> Value {
    let mut headers = BTreeMap::new();
    for (k, v) in &seen.headers {
        if TRANSPORT_HEADERS.contains(&k.as_str()) {
            continue;
        }
        let v = match k.as_str() {
            "idempotency-key" => "{idempotency_key}".to_owned(),
            "if-match" => v.replace(&DEVICE_VERSION.to_string(), "{device_version}"),
            _ => placeholders(v),
        };
        headers.insert(k.clone(), Value::String(v));
    }
    let mut request = json!({
        "method": seen.method,
        "path": placeholders(&seen.target),
        "headers": headers,
    });
    let is_json = seen
        .headers
        .iter()
        .any(|(k, v)| k == "content-type" && v.starts_with("application/json"));
    if seen.body.is_empty() {
        request["body"] = Value::Null;
    } else if is_json {
        let body: Value = serde_json::from_slice(&seen.body).expect("JSON request body");
        request["body"] = placeholders_json(&body);
    } else {
        request["body_base64"] = Value::String(base64(&seen.body));
    }
    json!({
        "contract": 1,
        "consumer": "silicon-extend-client",
        "consumer_version": env!("CARGO_PKG_VERSION"),
        "api_version": API_VERSION,
        "operation": op.operation,
        "call": op.call,
        "given": op.given,
        "request": request,
        "response": {
            "status": "2xx",
            "type": if op.kind.is_empty() { Value::Null } else { Value::String(op.kind.into()) },
            "required": required,
            "non_null": non_null,
        },
    })
}

async fn record(ctx: &Ctx, stub: &Stub, op: &Op) -> Value {
    stub.reply(op.status, op.kind, op.data.as_ref());
    if let Err(e) = (op.run)(ctx).await {
        panic!(
            "{} ({}) failed against its own expected response: {e}",
            op.operation, op.call
        );
    }
    let seen = stub.take();
    assert_eq!(
        seen.len(),
        1,
        "{} should make exactly one request, made {seen:?}",
        op.operation
    );
    let (mut required, mut non_null) = (Vec::new(), Vec::new());
    if let Some(data) = &op.data {
        let mut paths = Vec::new();
        field_paths(data, &mut Vec::new(), &mut paths);
        for path in paths {
            for remove in [true, false] {
                let mut mutated = data.clone();
                mutate(&mut mutated, &path, remove);
                stub.reply(op.status, op.kind, Some(&mutated));
                let decoded = (op.run)(ctx).await;
                stub.take();
                match decoded {
                    Ok(()) => {}
                    Err(Error::Decode { .. }) => {
                        if remove {
                            required.push(render(&path));
                        } else {
                            non_null.push(render(&path));
                        }
                    }
                    Err(e) => panic!(
                        "{} failed unexpectedly while mutating {}: {e}",
                        op.operation,
                        render(&path)
                    ),
                }
            }
        }
    }
    required.sort();
    non_null.sort();
    fixture(op, &seen[0], required, non_null)
}

#[tokio::test]
async fn fixtures_match_what_the_client_sends() {
    let (base, stub) = Stub::start().await;
    stub.reply(
        200,
        "version",
        Some(&json!({"api_version": 1, "supported": [1], "service_version": "1.0.0"})),
    );
    let ctx = Ctx {
        client: Client::connect(&base).await.unwrap(),
        testing: Client::builder(&base)
            .testing_secret(TESTING_SECRET)
            .connect()
            .await
            .unwrap(),
        options: Client::builder(&base)
            .isi(Some(ISI.into()))
            .telemetry(false)
            .connect()
            .await
            .unwrap(),
    };
    let mut generated = BTreeMap::new();
    for op in ops() {
        assert!(
            generated
                .insert(op.operation.to_owned(), record(&ctx, &stub, &op).await)
                .is_none(),
            "operation {} is listed twice",
            op.operation
        );
    }

    let dir = contracts_dir();
    let write = std::env::var("EXTEND_CONTRACTS_WRITE").is_ok_and(|v| v == "1");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if write {
        std::fs::create_dir_all(&dir).unwrap();
        for stale in on_disk.iter().filter(|n| !generated.contains_key(*n)) {
            std::fs::remove_file(dir.join(format!("{stale}.json"))).unwrap();
        }
        for (name, f) in &generated {
            let mut text = serde_json::to_string_pretty(f).unwrap();
            text.push('\n');
            std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
        }
        return;
    }
    let mut problems = Vec::new();
    for (name, f) in &generated {
        let path = dir.join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Err(_) => problems.push(format!("{name}: no fixture at {}", path.display())),
            Ok(text) => {
                let committed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                if &committed != f {
                    problems.push(format!(
                        "{name}: the client now sends or needs something else.\n  committed: {committed}\n  now:       {f}"
                    ));
                }
            }
        }
    }
    for stale in on_disk.iter().filter(|n| !generated.contains_key(*n)) {
        problems.push(format!("{stale}: fixture for a call the client no longer makes"));
    }
    assert!(
        problems.is_empty(),
        "The client's contract fixtures in {} are out of date:\n{}\n\nIf the change is intended, rewrite them with \
         `EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures` and commit contracts/. \
         The service's `contracts` test then checks the service still accepts them.",
        dir.display(),
        problems.join("\n")
    );
}

#[test]
fn base64_matches_the_standard() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"\x89PNG"), "iVBORw==");
}

/// Every public async call of the crate has a fixture (or is not an Extend API call), so a new
/// method can't ship without a contract.
#[test]
fn every_public_call_has_a_fixture() {
    let covered: BTreeSet<String> = ops()
        .iter()
        .filter_map(|op| op.call.rsplit("::").next().map(str::to_owned))
        .chain(NOT_API_CALLS.iter().map(|(n, _)| (*n).to_owned()))
        // `Client::connect` covers `ClientBuilder::connect` too.
        .collect();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("pub async fn ") else {
                continue;
            };
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !covered.contains(&name) {
                missing.push(format!("{} in {}", name, path.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "These public calls have no contract fixture: {missing:?}. Add each to ops() in {} (with the provider \
         state it needs in `given`), or to NOT_API_CALLS with the reason it isn't an Extend API request.",
        file!()
    );
}
