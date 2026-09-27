//! Shared harness for the 1.1 service tests: a real PostgreSQL (databases created per test,
//! `EXTEND_TEST_ADMIN_URL`, default `postgres://extend:extend@127.0.0.1:5440/postgres`), the real
//! HTTP and WebSocket stack, the local IAM and Ting stand-ins, and scripted Extend apps.
//!
//! Members (test_plan): c:alice in acme and globex, c:bob in acme, c:carol in globex, si:chef in
//! acme and globex, si:sous in acme, si:scout in globex.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use extend_protocol::DeviceOs;
use extend_protocol::frames::{DeviceFrame, Hello};
use extend_protocol::model::{EnrollmentCreate, EnrollmentState, Setup};
use extend_service::config::{Config, Environment, FilesMode, IamMode, TingMode, Tuning};
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
    pub client: Client,
    pub state: Shared,
}

pub const MEMBERS: &[(&str, &[&str])] = &[
    ("c:alice", &["acme", "globex"]),
    ("c:bob", &["acme"]),
    ("c:carol", &["globex"]),
    ("si:chef", &["acme", "globex"]),
    ("si:sous", &["acme"]),
    ("si:scout", &["globex"]),
];

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
        website_url: "http://localhost:5173".into(),
        docs_url: "http://localhost:5173/docs".into(),
        repository_url: "https://github.com/teamofsilicons/silicon-extend".into(),
        data_dir,
        iam: IamMode::Local,
        iam_public_url: format!("{base}/dev/iam"),
        iam_login_url: format!("{base}/dev/iam/login"),
        webhook_secret: None,
        webhook_previous_secret: None,
        files: FilesMode::Local,
        ting: TingMode::Local,
        honeycomb_service_token: Some("hck_test".into()),
        postmark_token: None,
        report_recipients: vec!["bugs@example.test".into()],
        device_app_min_version: "1.0.0".into(),
        local_members: MEMBERS
            .iter()
            .map(|(id, teams)| ((*id).to_owned(), teams.iter().map(|t| (*t).to_owned()).collect()))
            .collect(),
        web_dir: None,
        trusted_proxies: vec![],
        tuning,
    }
}

pub async fn start() -> Env {
    start_with(Tuning::default()).await
}

pub async fn start_with(tuning: Tuning) -> Env {
    let (url, data) = database("v11").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let state = extend_service::build(config(url, addr, data, tuning)).await.unwrap();
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
    extend_service::build(config(url, "127.0.0.1:9".parse().unwrap(), data, Tuning::default()))
        .await
        .unwrap()
}

pub async fn login(env: &Env, who: &str) -> String {
    env.client.login(who).await.unwrap().access_token
}

/// A raw API call: the status and the parsed body.
pub async fn api(
    env: &Env,
    method: &str,
    path: &str,
    token: &str,
    team: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", env.base),
        )
        .header("authorization", format!("Bearer {token}"));
    if let Some(t) = team {
        r = r.header("x-org-id", t);
    }
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

/// Pairs a new device as the apps do: an enrollment, a Carbon's claim (with `team` as X-Org-ID
/// when given), and the credential from the enrollment. Returns (device id, credential).
pub async fn pair(
    env: &Env,
    carbon_token: &str,
    team: Option<&str>,
    os: DeviceOs,
    name: &str,
    silicons: &[&str],
) -> (String, String) {
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
        "/api/v1/pairings",
        carbon_token,
        team,
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
    team: Option<&str>,
    silicons: &[&str],
) -> (u16, Value, Option<String>) {
    let (status, e) = device_api(env, "POST", "/api/v1/device/enrollments", credential).await;
    assert_eq!(status, 201, "a code for another Carbon: {e}");
    let e = &e["data"];
    let (status, d) = api(
        env,
        "POST",
        "/api/v1/pairings",
        carbon_token,
        team,
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

pub async fn pair_another(
    env: &Env,
    credential: &str,
    carbon_token: &str,
    team: Option<&str>,
    silicons: &[&str],
) -> (String, String) {
    let (status, d, cred) = pair_another_raw(env, credential, carbon_token, team, silicons).await;
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
        let mut req = env
            .client
            .ws_url("/api/v1/device/connect")
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Extend-Device {credential}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.expect("device socket");
        ws.send(Message::Text(hello.to_string().into())).await.unwrap();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let fail_commands = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let (f, c, fail) = (frames.clone(), closed.clone(), fail_commands.clone());
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    out = rx.recv() => {
                        let Some(v) = out else { break };
                        if ws.send(Message::Text(v.to_string().into())).await.is_err() { break }
                    }
                    m = ws.next() => {
                        let Some(Ok(m)) = m else { break };
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
        tokio::time::sleep(Duration::from_millis(150)).await;
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

/// The activity rows of a pair, oldest first: (action, actor, details, team, session_id).
pub async fn activity(env: &Env, device_id: &str) -> Vec<(String, String, Value, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT action, actor_id, COALESCE(details, 'null'::jsonb), team, session_id FROM extend.activity WHERE device_id = $1 ORDER BY id",
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

/// Starts a session as a Silicon (raw), returning its id.
pub async fn session(env: &Env, token: &str, team: &str, device_id: &str) -> (u16, Value) {
    api(
        env,
        "POST",
        "/api/v1/sessions",
        token,
        Some(team),
        Some(json!({"type": "session", "data": {"device_id": device_id}})),
    )
    .await
}

pub async fn grant(env: &Env, carbon_token: &str, device_id: &str, silicon: &str, team: &str) {
    let (status, g) = api(
        env,
        "PUT",
        &format!("/api/v1/devices/{device_id}/access/{silicon}?team={team}"),
        carbon_token,
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "granting {silicon} in {team}: {g}");
}

pub async fn wake(env: &Env, token: &str, team: &str, device_id: &str, reason: &str) -> (u16, Value) {
    api(
        env,
        "POST",
        &format!("/api/v1/devices/{device_id}/wake-requests"),
        token,
        Some(team),
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

// ───────────── Test environments ─────────────

/// Prepares a test environment through Honeycomb's instruction, opens its test application in the
/// local IAM, and selects it once so it is ready. Returns (environment id, app secret).
pub async fn open_test_env(env: &Env) -> (Uuid, String) {
    let envid = Uuid::new_v4();
    let op = Uuid::new_v4();
    let r = reqwest::Client::new()
        .put(format!(
            "{}/internal/honeycomb/organizations/acme/testing-environments/{envid}/operations/{op}",
            env.base
        ))
        .bearer_auth("hck_test")
        .json(
            &json!({"operation_id": op, "environment_id": envid, "org_id": "acme", "app_id": "extend",
            "environment_revision": 1, "generation": 1, "key_version": 1, "action": "prepare",
            "testing_key": "abcdefghijklmnopqrstuvwxyz012345", "name": "v11-env"}),
        )
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "prepare: {}", r.status());
    let secret = extend_protocol::ids::new_secret("ask_");
    let r = reqwest::Client::new()
        .post(format!("{}/dev/iam/test-apps", env.base))
        .json(&json!({"type": "test_app", "data": {"secret": secret, "environment_id": envid}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let c = Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    c.testing_environment().await.unwrap();
    (envid, secret)
}

/// Sends a Honeycomb lifecycle instruction for a test environment.
pub async fn lifecycle(env: &Env, envid: Uuid, action: &str, revision: i64, generation: i64) {
    let op = Uuid::new_v4();
    let r = reqwest::Client::new()
        .put(format!(
            "{}/internal/honeycomb/organizations/acme/testing-environments/{envid}/operations/{op}",
            env.base
        ))
        .bearer_auth("hck_test")
        .json(
            &json!({"operation_id": op, "environment_id": envid, "org_id": "acme", "app_id": "extend",
            "environment_revision": revision, "generation": generation, "key_version": 1, "action": action,
            "testing_key": "abcdefghijklmnopqrstuvwxyz012345", "name": "v11-env"}),
        )
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{action}: {}", r.status());
}

/// A raw call in a test environment.
pub async fn api_in(
    env: &Env,
    secret: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    team: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", env.base),
        )
        .header("x-testing-application-secret", secret);
    if let Some(t) = token {
        r = r.header("authorization", t);
    }
    if let Some(t) = team {
        r = r.header("x-org-id", t);
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// A member's access token in a test environment (the member id works as the login there).
pub async fn login_in(env: &Env, secret: &str, who: &str) -> String {
    let (s, v) = api_in(
        env,
        secret,
        "POST",
        "/api/v1/auth/login",
        None,
        None,
        Some(json!({"type": "login", "data": {"slt": who}})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    format!("Bearer {}", v["data"]["access_token"].as_str().unwrap())
}

/// Pairs a device into a test environment. Returns (device id, credential), or the claim's error.
pub async fn pair_in(
    env: &Env,
    secret: &str,
    carbon: &str,
    os: DeviceOs,
    name: &str,
) -> Result<(String, String), (u16, Value)> {
    let (s, e) = api_in(
        env,
        secret,
        "POST",
        "/api/v1/enrollments",
        None,
        None,
        Some(json!({"type": "enrollment", "data": {"os": os, "app_version": "1.1.0"}})),
    )
    .await;
    assert_eq!(s, 201, "{e}");
    let (s, d) = api_in(
        env,
        secret,
        "POST",
        "/api/v1/pairings",
        Some(carbon),
        Some("acme"),
        Some(json!({"type": "pairing", "data": {"pairing_code": e["data"]["pairing_code"], "name": name}})),
    )
    .await;
    if s != 201 {
        return Err((s, d));
    }
    let (_, st) = api_in(
        env,
        secret,
        "GET",
        &format!("/api/v1/enrollments/{}", e["data"]["enrollment_id"].as_str().unwrap()),
        Some(&format!(
            "Extend-Enrollment {}",
            e["data"]["enrollment_secret"].as_str().unwrap()
        )),
        None,
        None,
    )
    .await;
    Ok((
        d["data"]["device_id"].as_str().unwrap().to_owned(),
        st["data"]["device_credential"].as_str().unwrap().to_owned(),
    ))
}
