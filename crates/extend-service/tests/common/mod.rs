//! Shared harness for the service tests: a real PostgreSQL (databases created per test,
//! `EXTEND_TEST_ADMIN_URL`, default `postgres://extend:extend@127.0.0.1:5440/postgres`), the real
//! HTTP and WebSocket stack, the local Silicon Accounts, Briefcase and Ting stand-ins, and
//! scripted Extend apps.
//!
//! Accounts (personal; no Teams): Carbons c:alice, c:bob, c:carol and c:dave; Silicons si:chef,
//! si:sous and si:line (custodian c:alice), si:scout (custodian c:carol) and si:rover (custodian
//! c:bob). So chef, sous, line and alice are one custodian circle; scout and carol another; rover
//! and bob another.

#![allow(dead_code)]

#[path = "v2.rs"]
pub mod v2;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use extend_protocol::DeviceOs;
use extend_protocol::frames::{DeviceFrame, Hello};
use extend_protocol::model::{EnrollmentCreate, EnrollmentState, Setup, SetupState};
use extend_service::accounts::local::LocalAccounts;
use extend_service::config::{AccountsMode, Config, Environment, FilesMode, TingMode, Tuning};
use extend_service::state::Shared;
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use silicon_extend_client::Client;
use sqlx::Connection as _;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use uuid::Uuid;

pub struct Env {
    pub base: String,
    pub pool: sqlx::PgPool,
    /// The 3.x client: the device wire (enrollments, a device's own calls) is unchanged.
    pub client: Client,
    pub state: Shared,
}

impl Env {
    /// Account API v2 calls as `token`.
    pub fn v2<'a>(&'a self, token: &'a str) -> v2::V2<'a> {
        v2::V2::new(&self.base, token)
    }
    /// The local Silicon Accounts stand-in.
    pub fn accounts(&self) -> &LocalAccounts {
        self.state
            .accounts
            .local
            .as_deref()
            .expect("tests run with the local Silicon Accounts")
    }
}

/// The signing secret of the test webhook.
pub const WEBHOOK_SECRET: &str = "whsec_extend_tests_0123456789";

/// The test accounts and their custodians.
pub const ACCOUNTS: &[(&str, Option<&str>)] = &[
    ("c:alice", None),
    ("c:bob", None),
    ("c:carol", None),
    ("c:dave", None),
    ("si:chef", Some("c:alice")),
    ("si:sous", Some("c:alice")),
    ("si:scout", Some("c:carol")),
    ("si:rover", Some("c:bob")),
    ("si:line", Some("c:alice")),
];

pub fn custodian_of(who: &str) -> Option<&'static str> {
    ACCOUNTS.iter().find(|(id, _)| *id == who).and_then(|(_, c)| *c)
}

/// The Silicon Accounts uuid the stand-in gives an account.
pub fn uuid(who: &str) -> String {
    LocalAccounts::uuid_for(who)
}

pub async fn database(prefix: &str) -> (String, PathBuf) {
    let admin = std::env::var("EXTEND_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://extend:extend@127.0.0.1:5440/postgres".into());
    let db = format!("extend_{prefix}_{}", Uuid::new_v4().simple());
    let mut conn = sqlx::PgConnection::connect(&admin)
        .await
        .expect("PostgreSQL for tests (set EXTEND_TEST_ADMIN_URL)");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut conn)
        .await
        .unwrap();
    let url = format!("{}/{db}", admin.rsplit_once('/').unwrap().0);
    (url, std::env::temp_dir().join(&db))
}

pub fn config(database_url: String, bind: SocketAddr, data_dir: PathBuf, tuning: Tuning) -> Config {
    let base = format!("http://{bind}");
    Config {
        environment: Environment::Test,
        bind,
        database_url,
        public_url: base.clone(),
        website_url: "http://localhost:4220".into(),
        docs_url: "http://localhost:4220/docs".into(),
        repository_url: "https://github.com/teamofsilicons/silicon-extend".into(),
        data_dir,
        accounts_url: format!("{base}/dev/accounts"),
        accounts_api_url: format!("{base}/dev/accounts"),
        app_id: "extend".into(),
        accounts: AccountsMode::Local,
        webhook_secret: Some(WEBHOOK_SECRET.into()),
        webhook_previous_secret: None,
        delegation_key: Some(
            extend_service::proofs::GrantKey::parse("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        ),
        files: FilesMode::Local,
        ting: TingMode::Local,
        postmark_token: None,
        report_recipients: vec!["bugs@example.test".into()],
        device_app_min_version: "1.0.0".into(),
        web_dir: None,
        trusted_proxies: vec![],
        cors_origins: vec![],
        tuning,
        obsolete: vec![],
    }
}

pub async fn start() -> Env {
    start_with(Tuning::default()).await
}

pub async fn start_with(tuning: Tuning) -> Env {
    start_config(|c| c.tuning = tuning).await
}

/// Starts a service whose configuration `change` adjusts first.
pub async fn start_config(change: impl FnOnce(&mut Config)) -> Env {
    let (url, data) = database("v11").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let mut cfg = config(url, addr, data, Tuning::default());
    change(&mut cfg);
    let state = extend_service::build(cfg).await.unwrap();
    seed_accounts(&state);
    let pool = state.pool.clone();
    tokio::spawn(extend_service::serve_on(listener, state.clone()));
    let client = Client::connect(&base).await.unwrap();
    Env {
        base,
        pool,
        client,
        state,
    }
}

/// The state alone, without serving (no scheduler): for driving passes by hand.
pub async fn state_only() -> Shared {
    let (url, data) = database("v11s").await;
    let state = extend_service::build(config(url, "127.0.0.1:9".parse().unwrap(), data, Tuning::default()))
        .await
        .unwrap();
    seed_accounts(&state);
    state
}

/// Every test account exists in the local Silicon Accounts from the start (Silicon Accounts knows
/// them before they ever use Extend).
pub fn seed_accounts(state: &Shared) {
    if let Some(local) = state.accounts.local.as_deref() {
        for (id, custodian) in ACCOUNTS {
            local.ensure(id, *custodian).expect("a test account");
        }
    }
}

/// An access token for a test account, signed by the local Silicon Accounts (the account and its
/// custodian are created on first use).
pub async fn login(env: &Env, who: &str) -> String {
    let account = env.accounts().ensure(who, custodian_of(who)).expect("a test account");
    env.accounts().mint(&account, 1800)
}

/// A raw API call: the status and the parsed body.
pub async fn api(env: &Env, method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", env.base),
        )
        .header("authorization", format!("Bearer {token}"));
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// A call with a device credential.
pub async fn device_api(env: &Env, method: &str, path: &str, credential: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", env.base),
        )
        .header("authorization", format!("Extend-Device {credential}"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// Delivers a Silicon Accounts webhook event, signed with [`WEBHOOK_SECRET`]. Returns the status.
pub async fn deliver(env: &Env, event_type: &str, data: Value) -> u16 {
    deliver_raw(env, &Uuid::now_v7().to_string(), event_type, data).await
}

/// [`deliver`] with a given event id (retries reuse it).
pub async fn deliver_raw(env: &Env, event_id: &str, event_type: &str, data: Value) -> u16 {
    deliver_at(env, event_id, event_type, data, time::OffsetDateTime::now_utc()).await
}

/// [`deliver_raw`] of an event that happened at `occurred_at` (deliveries arrive late).
pub async fn deliver_at(
    env: &Env,
    event_id: &str,
    event_type: &str,
    data: Value,
    occurred_at: time::OffsetDateTime,
) -> u16 {
    let body = json!({"event_id": event_id, "type": event_type, "occurred_at": occurred_at
        .format(&time::format_description::well_known::Rfc3339).unwrap(), "app_id": "extend", "data": data})
    .to_string();
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    let sig = silicon_accounts_client::sign_webhook(WEBHOOK_SECRET, ts, body.as_bytes());
    let resp = reqwest::Client::new()
        .post(format!("{}/webhooks/accounts", env.base))
        .header("content-type", "application/json")
        .header("x-accounts-timestamp", ts.to_string())
        .header("x-accounts-signature", sig)
        .header("x-accounts-event-id", event_id)
        .header("x-accounts-event-type", event_type)
        .body(body)
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

pub fn hello(os: DeviceOs, app_version: &str) -> Value {
    hello_with(os, app_version, Setup::complete())
}

pub fn hello_with(os: DeviceOs, app_version: &str, setup: Setup) -> Value {
    let v11 = app_version.starts_with("1.1");
    serde_json::to_value(DeviceFrame::Hello(Hello {
        app_version: app_version.into(),
        os,
        os_version: Some("15".into()),
        model: Some("Test".into()),
        engine_version: v11.then(|| "0.21.15".into()),
        capabilities: os.full_capabilities().to_vec(),
        missing: vec![],
        setup,
        features: if v11 { vec!["setup_retry".into()] } else { vec![] },
    }))
    .unwrap()
}

/// Pairs a new device as the apps do: an enrollment, a Carbon's claim, and the credential from the
/// enrollment. Returns (device id, credential).
pub async fn pair(env: &Env, carbon_token: &str, os: DeviceOs, name: &str, silicons: &[&str]) -> (String, String) {
    let e = env
        .client
        .enroll(&EnrollmentCreate {
            os,
            os_version: Some("15".into()),
            model: Some("Test".into()),
            app_version: "1.1.0".into(),
            engine_version: None,
        })
        .await
        .unwrap();
    let (status, d) = api(
        env,
        "POST",
        "/api/v2/pairings",
        carbon_token,
        Some(
            json!({"type": "pairing", "data": {"pairing_code": e.pairing_code, "name": name, "silicon_ids": silicons}}),
        ),
    )
    .await;
    assert_eq!(status, 201, "pairing {name}: {d}");
    let id = d["data"]["device_id"].as_str().unwrap().to_owned();
    (id, credential_of(env, e.enrollment_id, &e.enrollment_secret).await)
}

pub async fn credential_of(env: &Env, enrollment: Uuid, secret: &str) -> String {
    match env.client.enrollment(enrollment, secret).await.unwrap() {
        EnrollmentState::Paired { device_credential, .. } => device_credential,
        other => panic!("not paired: {other:?}"),
    }
}

/// "Pair with another Carbon": a code from the device's credential, claimed by `carbon_token`.
/// Returns the claim's status and body, and the new pair's credential when it was claimed.
pub async fn pair_another_raw(
    env: &Env,
    credential: &str,
    carbon_token: &str,
    silicons: &[&str],
) -> (u16, Value, Option<String>) {
    let (status, e) = device_api(env, "POST", "/api/v1/device/enrollments", credential).await;
    assert_eq!(status, 201, "a code for another Carbon: {e}");
    let e = &e["data"];
    let (status, d) = api(
        env,
        "POST",
        "/api/v2/pairings",
        carbon_token,
        Some(json!({"type": "pairing", "data": {"pairing_code": e["pairing_code"], "name": "Their name", "silicon_ids": silicons}})),
    )
    .await;
    if status != 201 {
        return (status, d, None);
    }
    let id: Uuid = e["enrollment_id"].as_str().unwrap().parse().unwrap();
    let cred = credential_of(env, id, e["enrollment_secret"].as_str().unwrap()).await;
    (status, d, Some(cred))
}

pub async fn pair_another(env: &Env, credential: &str, carbon_token: &str, silicons: &[&str]) -> (String, String) {
    let (status, d, cred) = pair_another_raw(env, credential, carbon_token, silicons).await;
    assert_eq!(status, 201, "pair with another Carbon: {d}");
    (d["data"]["device_id"].as_str().unwrap().to_owned(), cred.unwrap())
}

/// A scripted Extend app on one pair's connection: answers pings and commands, and records every
/// frame the service sends.
pub struct App {
    pub frames: Arc<Mutex<Vec<Value>>>,
    tx: mpsc::UnboundedSender<Value>,
    pub closed: Arc<AtomicBool>,
    /// Answer commands with ok: false.
    pub fail_commands: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl App {
    pub async fn connect(env: &Env, credential: &str, hello: Value) -> App {
        let DeviceFrame::Hello(expected) = serde_json::from_value(hello.clone()).expect("fixture Hello") else {
            panic!("App::connect needs a Hello frame")
        };
        let world = extend_service::db::World::production();
        let id = extend_service::state::device_by_credential(&env.state, credential)
            .await
            .unwrap()
            .expect("paired fixture device");
        let mut req = env
            .client
            .ws_url("/api/v1/device/connect")
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Extend-Device {credential}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.expect("device socket");
        ws.send(Message::Text(hello.to_string().into())).await.unwrap();
        // The service processes this connection's Hello before reading the next control frame.
        // Its matching Pong also fences reconnect initialization when old metadata is identical.
        let hello_fence = Uuid::new_v4().as_bytes().to_vec();
        ws.send(Message::Ping(hello_fence.clone().into())).await.unwrap();
        let (applied, observed) = tokio::sync::oneshot::channel();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let fail_commands = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let (f, c, fail) = (frames.clone(), closed.clone(), fail_commands.clone());
        let task = tokio::spawn(async move {
            let mut applied = Some(applied);
            loop {
                tokio::select! {
                    out = rx.recv() => {
                        let Some(v) = out else { break };
                        if ws.send(Message::Text(v.to_string().into())).await.is_err() { break }
                    }
                    m = ws.next() => {
                        let Some(Ok(m)) = m else { break };
                        if let Message::Pong(ref bytes) = m
                            && bytes.as_ref() == hello_fence.as_slice()
                            && let Some(applied) = applied.take()
                        {
                            let _ = applied.send(());
                            continue;
                        }
                        let Message::Text(t) = m else {
                            if matches!(m, Message::Close(_)) { break }
                            continue;
                        };
                        let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                        match v["type"].as_str() {
                            Some("ping") => {
                                let _ = ws.send(Message::Text(json!({"type": "pong", "nonce": v["nonce"]}).to_string().into())).await;
                            }
                            Some("command") => {
                                let ok = !fail.load(Ordering::Relaxed);
                                let answer = json!({"type": "result", "id": v["id"], "ok": ok, "output": {},
                                    "text": "ran", "error": if ok { Value::Null } else { json!({"code": "failed", "message": "The screen is off."}) },
                                    "files": []});
                                let _ = ws.send(Message::Text(answer.to_string().into())).await;
                            }
                            _ => {}
                        }
                        f.lock().unwrap().push(v);
                    }
                }
            }
            c.store(true, Ordering::Relaxed);
        });
        let app = App {
            frames,
            tx,
            closed,
            fail_commands,
            task,
        };
        // Retain the state assertion too: a processed Hello must have persisted its reported
        // setup and capabilities, including deliberately failed setup and old app versions.
        let mut capabilities = expected.capabilities.clone();
        capabilities.sort();
        let expected_state = if expected.setup.state == SetupState::Complete || expected.setup.steps.is_empty() {
            "ready"
        } else {
            "setup"
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            observed.await.expect("fixture socket closed before its Hello fence");
            loop {
                assert!(
                    !app.is_closed(),
                    "fixture socket closed before its required state was observed"
                );
                let d = extend_service::domain::load_device(&env.state, &world, &id)
                    .await
                    .unwrap()
                    .expect("paired fixture device");
                if d.state == expected_state
                    && d.setup() == expected.setup
                    && d.app_version.as_deref() == Some(expected.app_version.as_str())
                    && d.os() == expected.os
                    && d.os_version == expected.os_version
                    && d.model == expected.model
                    && d.engine_version == expected.engine_version
                    && d.capabilities == json!(capabilities)
                    && d.missing == json!(expected.missing)
                    && env.state.hub.is_connected(&d.key(&world)).await
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fixture device {id} never matched its reported state: {expected:?}"));
        app
    }

    pub fn send(&self, v: Value) {
        let _ = self.tx.send(v);
    }

    pub fn of(&self, kind: &str) -> Vec<Value> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["type"] == kind)
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.frames.lock().unwrap().clear();
    }

    /// Waits for a frame of `kind` that `pred` accepts.
    pub async fn wait(&self, kind: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(f) = self.of(kind).into_iter().find(|f| pred(f)) {
                return f;
            }
            assert!(
                Instant::now() < deadline,
                "no {kind} frame came; got {:?}",
                self.frames.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if check().await {
            return;
        }
        assert!(Instant::now() < deadline, "{what} never happened");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The activity rows of a pair, oldest first: (action, actor uuid, details, session_id).
pub async fn activity(env: &Env, device_id: &str) -> Vec<(String, String, Value, Option<String>)> {
    sqlx::query_as(
        "SELECT action, actor_id, COALESCE(details, 'null'::jsonb), session_id FROM extend.activity WHERE device_id = $1 ORDER BY id",
    )
    .bind(device_id)
    .fetch_all(&env.pool)
    .await
    .unwrap()
}

pub async fn instance_of(env: &Env, device_id: &str) -> Uuid {
    sqlx::query_scalar("SELECT instance_id FROM extend.devices WHERE device_id = $1")
        .bind(device_id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

/// Starts a session as a Silicon (raw).
pub async fn session(env: &Env, token: &str, device_id: &str) -> (u16, Value) {
    api(
        env,
        "POST",
        "/api/v2/sessions",
        token,
        Some(json!({"type": "session", "data": {"device_id": device_id}})),
    )
    .await
}

/// Gives a Silicon (by `si:` id) access through a Carbon's pair.
pub async fn grant(env: &Env, carbon_token: &str, device_id: &str, silicon: &str) {
    let (status, g) = api(
        env,
        "PUT",
        &format!("/api/v2/devices/{device_id}/access/{silicon}"),
        carbon_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "granting {silicon}: {g}");
}

pub async fn wake(env: &Env, token: &str, device_id: &str, reason: &str) -> (u16, Value) {
    api(
        env,
        "POST",
        &format!("/api/v2/devices/{device_id}/wake-requests"),
        token,
        Some(json!({"type": "wake_request", "data": {"reason": reason}})),
    )
    .await
}

pub fn awake_frame(awake: bool, sleep: Option<&str>, input_seen: Option<bool>, run: Uuid, seq: u64) -> Value {
    let mut v = json!({"type": "awake", "awake": awake, "run": run, "seq": seq});
    if let Some(s) = sleep {
        v["sleep_state"] = json!(s);
    }
    if let Some(i) = input_seen {
        v["input_seen"] = json!(i);
    }
    v
}
