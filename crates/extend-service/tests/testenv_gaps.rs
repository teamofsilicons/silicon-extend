//! Service tests for the test-environment rules: readiness, disable/restore/purge, the test secret
//! on every route, pairing codes bound to their world, the 10-environment limit, lifecycle
//! receipts, the clean fence, IAM webhook ordering and durability, logout by refresh token, and
//! the member-id login shortcut.
//!
//! Real PostgreSQL, the real HTTP and WebSocket stack, the official client crate, and scripted
//! fake devices. Needs a PostgreSQL the tests can create databases on: `EXTEND_TEST_ADMIN_URL`
//! (default `postgres://extend:extend@127.0.0.1:5440/postgres`).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use extend_protocol::frames::{DeviceFrame, EnrollmentFrame, Hello, ServiceFrame, close};
use extend_protocol::model::*;
use extend_protocol::{DeviceOs, ErrorCode};
use extend_service::config::{Config, Environment, FilesMode, IamMode, TingMode};
use extend_service::state::Shared;
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::Client;
use sqlx::Connection as _;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Env {
    base: String,
    pool: sqlx::PgPool,
    client: Client,
    state: Shared,
    http: reqwest::Client,
}

async fn start() -> Env {
    let admin = std::env::var("EXTEND_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://extend:extend@127.0.0.1:5440/postgres".into());
    let db = format!("extend_e2e_{}", Uuid::new_v4().simple());
    let mut conn = sqlx::PgConnection::connect(&admin)
        .await
        .expect("PostgreSQL for tests (set EXTEND_TEST_ADMIN_URL)");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut conn)
        .await
        .unwrap();
    let url = format!("{}/{db}", admin.rsplit_once('/').unwrap().0);
    let data = std::env::temp_dir().join(&db);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let cfg = Config {
        environment: Environment::Test,
        bind: addr,
        database_url: url,
        public_url: base.clone(),
        website_url: "http://localhost:5173".into(),
        docs_url: "http://localhost:5173/docs".into(),
        repository_url: "https://github.com/teamofsilicons/silicon-extend".into(),
        data_dir: data,
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
        local_members: vec![
            ("c:alice".into(), vec!["acme".into()]),
            ("c:bob".into(), vec!["acme".into()]),
            ("si:chef".into(), vec!["acme".into()]),
            ("si:sous".into(), vec!["acme".into()]),
        ],
        web_dir: None,
        trusted_proxies: vec![],
        tuning: Default::default(),
    };
    let state = extend_service::build(cfg).await.unwrap();
    let pool = state.pool.clone();
    tokio::spawn(extend_service::serve_on(listener, state.clone()));
    let client = Client::connect(&base).await.unwrap();
    Env {
        base,
        pool,
        client,
        state,
        http: reqwest::Client::new(),
    }
}

// ───────────────────────────── Honeycomb ─────────────────────────────

struct Op {
    env: Uuid,
    id: Uuid,
    body: serde_json::Value,
}

impl Op {
    fn new(env: Uuid, action: &str, revision: i64, generation: i64, key_version: i64) -> Self {
        let id = Uuid::new_v4();
        Self {
            env,
            id,
            body: serde_json::json!({
                "operation_id": id, "environment_id": env, "org_id": "acme", "app_id": "extend",
                "environment_revision": revision, "generation": generation, "key_version": key_version,
                "action": action, "testing_key": "abcdefghijklmnopqrstuvwxyz012345", "name": format!("env-{}", &env.simple().to_string()[..6]),
            }),
        }
    }
    fn url(&self, base: &str) -> String {
        format!(
            "{base}/internal/honeycomb/organizations/acme/testing-environments/{}/operations/{}",
            self.env, self.id
        )
    }
    async fn send(&self, env: &Env) -> (u16, serde_json::Value) {
        let r = env
            .http
            .put(self.url(&env.base))
            .bearer_auth("hck_test")
            .json(&self.body)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(serde_json::Value::Null))
    }
    async fn receipt(&self, env: &Env) -> (u16, serde_json::Value) {
        let r = env
            .http
            .get(self.url(&env.base))
            .bearer_auth("hck_test")
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(serde_json::Value::Null))
    }
}

async fn lifecycle(
    env: &Env,
    envid: Uuid,
    action: &str,
    revision: i64,
    generation: i64,
    key: i64,
) -> (u16, serde_json::Value) {
    Op::new(envid, action, revision, generation, key).send(env).await
}

async fn env_state(env: &Env, envid: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM extend_global.test_environments WHERE environment_id = $1")
        .bind(envid)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

/// Registers a test application secret with the local IAM stand-in (`active` = IAM has opened it).
async fn register_app(env: &Env, envid: Uuid, active: bool) -> String {
    let secret = extend_protocol::ids::new_secret("ask_");
    let r = env
        .http
        .post(format!("{}/dev/iam/test-apps", env.base))
        .json(&serde_json::json!({"type":"test_app","data":{"secret": secret, "environment_id": envid}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    set_app_active(env, &secret, active).await;
    secret
}

async fn set_app_active(env: &Env, secret: &str, active: bool) {
    sqlx::query("UPDATE extend_global.local_test_apps SET active = $2 WHERE secret_digest = $1")
        .bind(extend_protocol::ids::secret_digest(secret))
        .bind(active)
        .execute(&env.pool)
        .await
        .unwrap();
}

/// A prepared test environment whose app IAM has opened, selected once so it's `ready`.
async fn open_env(env: &Env) -> (Uuid, String, Client) {
    let envid = Uuid::new_v4();
    let (s, r) = lifecycle(env, envid, "prepare", 1, 1, 1).await;
    assert_eq!(s, 200, "{r}");
    let secret = register_app(env, envid, true).await;
    let t = Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    t.testing_environment().await.unwrap();
    assert_eq!(env_state(env, envid).await, "ready");
    (envid, secret, t)
}

fn code_of<T: std::fmt::Debug>(r: Result<T, silicon_extend_client::Error>) -> ErrorCode {
    r.expect_err("expected an error").code()
}

async fn raw(
    env: &Env,
    method: reqwest::Method,
    path: &str,
    headers: &[(&str, &[u8])],
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut r = env.http.request(method, format!("{}{path}", env.base));
    for (k, v) in headers {
        r = r.header(*k, reqwest::header::HeaderValue::from_bytes(v).unwrap());
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

// ───────────────────────────── Devices ─────────────────────────────

struct Device {
    id: String,
    credential: String,
    ws: Ws,
}

async fn ws_connect(url: &str, auth: &str) -> Ws {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("authorization", auth.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_text(ws: &mut Ws) -> String {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(45), ws.next())
            .await
            .expect("frame in time")
            .expect("socket open")
            .unwrap();
        if let Message::Text(t) = m {
            return t.to_string();
        }
    }
}

async fn enroll(client: &Client) -> EnrollmentCreated {
    client
        .enroll(&EnrollmentCreate {
            os: DeviceOs::Linux,
            os_version: Some("1".into()),
            model: Some("Fake".into()),
            app_version: "1.0.0".into(),
            engine_version: None,
        })
        .await
        .unwrap()
}

fn claim(code: &str) -> PairingClaim {
    PairingClaim {
        pairing_code: code.to_owned(),
        name: "Fake box".into(),
        visibility: None,
        pair_ttl_days: None,
        silicon_ids: vec!["si:chef".into()],
    }
}

impl Device {
    /// Enrolls through `client` (its world), lets `c:alice` pair it with access for `si:chef`,
    /// connects and says hello.
    async fn pair(env: &Env, client: &Client) -> Device {
        let e = enroll(client).await;
        let mut ews = ws_connect(
            &client.ws_url(&format!("/api/v1/enrollments/{}/connect", e.enrollment_id)),
            &format!("Extend-Enrollment {}", e.enrollment_secret),
        )
        .await;
        let token = client.login("c:alice").await.unwrap().access_token;
        client
            .authed(&token, Some("acme"))
            .pair(&claim(&e.pairing_code))
            .await
            .unwrap();
        let (id, credential) = loop {
            let f: EnrollmentFrame = serde_json::from_str(&next_text(&mut ews).await).unwrap();
            if let EnrollmentFrame::Paired {
                device_id,
                device_credential,
                ..
            } = f
            {
                break (device_id.to_string(), device_credential);
            }
        };
        let mut d = Device {
            ws: ws_connect(
                &env.client.ws_url("/api/v1/device/connect"),
                &format!("Extend-Device {credential}"),
            )
            .await,
            id,
            credential,
        };
        d.hello().await;
        d
    }

    async fn hello(&mut self) {
        let hello = DeviceFrame::Hello(Hello {
            app_version: "1.0.0".into(),
            os: DeviceOs::Linux,
            os_version: Some("15".into()),
            model: Some("Fake".into()),
            engine_version: None,
            capabilities: DeviceOs::Linux.full_capabilities().to_vec(),
            missing: vec![],
            setup: Setup::complete(),
            features: vec![],
        });
        self.ws
            .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    /// Frames until the socket closes: the frames, and the close code and reason.
    async fn until_closed(&mut self) -> (Vec<ServiceFrame>, Option<(u16, String)>) {
        let mut frames = Vec::new();
        loop {
            let m = tokio::time::timeout(Duration::from_secs(20), self.ws.next())
                .await
                .expect("the socket closes in time");
            match m {
                Some(Ok(Message::Text(t))) => {
                    let f: ServiceFrame = serde_json::from_str(&t).unwrap();
                    if let ServiceFrame::Ping { nonce } = f {
                        let pong = DeviceFrame::Pong { nonce };
                        let _ = self
                            .ws
                            .send(Message::Text(serde_json::to_string(&pong).unwrap().into()))
                            .await;
                    } else {
                        frames.push(f);
                    }
                }
                Some(Ok(Message::Close(c))) => {
                    return (frames, c.map(|c| (u16::from(c.code), c.reason.to_string())));
                }
                Some(Ok(_)) => {}
                None | Some(Err(_)) => return (frames, None),
            }
        }
    }
}

async fn device_self_status(env: &Env, credential: &str) -> (u16, serde_json::Value) {
    raw(
        env,
        reqwest::Method::GET,
        "/api/v1/device",
        &[("authorization", format!("Extend-Device {credential}").as_bytes())],
        None,
    )
    .await
}

// ───────────────────────────── Readiness ─────────────────────────────

/// `prepare` and `restore` open nothing by themselves: access opens once IAM (which Honeycomb
/// opens after every service is ready) accepts the secret, or Honeycomb sends `activate`.
#[tokio::test]
async fn test_access_opens_only_once_readiness_is_confirmed() {
    let env = start().await;
    let envid = Uuid::new_v4();
    let (s, r) = lifecycle(&env, envid, "prepare", 1, 1, 1).await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["state"], "completed");
    assert_eq!(r["target_state"], "preparing");
    assert_eq!(env_state(&env, envid).await, "preparing");

    // IAM keeps the environment's app closed until Honeycomb confirms readiness: refused, and
    // Extend's own preparation doesn't open it.
    let secret = register_app(&env, envid, false).await;
    let t = Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    assert_eq!(code_of(t.testing_environment().await), ErrorCode::TestingSecretInvalid);
    assert_eq!(code_of(t.login("c:alice").await), ErrorCode::TestingSecretInvalid);
    assert_eq!(env_state(&env, envid).await, "preparing");

    // IAM opens it (Honeycomb confirmed every service): the next request is admitted.
    set_app_active(&env, &secret, true).await;
    assert_eq!(t.testing_environment().await.unwrap().environment_id, envid);
    assert_eq!(env_state(&env, envid).await, "ready");

    // Restoring closes it again until readiness is confirmed; IAM refusing a secret Extend has
    // seen confirmed gets a precise answer naming the environment.
    assert_eq!(lifecycle(&env, envid, "disable", 2, 1, 1).await.0, 200);
    let (s, r) = lifecycle(&env, envid, "restore", 3, 1, 1).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("preparing")), "{r}");
    set_app_active(&env, &secret, false).await;
    let e = t.testing_environment().await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::TestingEnvironmentNotReady);
    let api = e.api().unwrap();
    assert!(
        api.message.contains("not open yet") && api.message.contains("Honeycomb"),
        "{}",
        api.message
    );
    assert!(
        api.hint
            .as_deref()
            .unwrap_or_default()
            .contains("Wait until Honeycomb reports"),
        "{api:?}"
    );
    assert_eq!(env_state(&env, envid).await, "preparing");

    // Honeycomb's explicit confirmation opens Extend's side (it may repeat the revision it confirms).
    let (s, r) = lifecycle(&env, envid, "activate", 3, 1, 1).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("ready")), "{r}");
    assert_eq!(env_state(&env, envid).await, "ready");
    set_app_active(&env, &secret, true).await;
    assert!(t.login("c:alice").await.is_ok());

    // An older confirmation is refused.
    assert_eq!(lifecycle(&env, envid, "activate", 2, 1, 1).await.0, 409);

    // While a clean runs the environment is closed, with a precise answer.
    sqlx::query("UPDATE extend_global.test_environments SET state = 'cleaning' WHERE environment_id = $1")
        .bind(envid)
        .execute(&env.pool)
        .await
        .unwrap();
    let e = t.login("c:alice").await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::TestingEnvironmentNotReady);
    assert!(e.api().unwrap().message.contains("being cleaned"), "{:?}", e.api());

    // Without IAM confirming anything, `activate` alone opens Extend's side of a prepared one.
    let other = Uuid::new_v4();
    assert_eq!(lifecycle(&env, other, "prepare", 1, 1, 1).await.0, 200);
    assert_eq!(env_state(&env, other).await, "preparing");
    assert_eq!(lifecycle(&env, other, "activate", 1, 1, 1).await.0, 200);
    assert_eq!(env_state(&env, other).await, "ready");

    // An import may be the first instruction; it prepares Extend's part the same way. Anything
    // else needs a prepared environment.
    let imported = Uuid::new_v4();
    let (s, r) = lifecycle(&env, imported, "import", 1, 1, 1).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("preparing")), "{r}");
    assert_eq!(env_state(&env, imported).await, "preparing");
    let (s, r) = lifecycle(&env, Uuid::new_v4(), "disable", 1, 1, 1).await;
    assert_eq!(s, 409, "{r}");
    assert!(r["data"]["hint"].as_str().unwrap().contains("prepare"), "{r}");
}

// ───────────────────────────── Disable ─────────────────────────────

#[tokio::test]
async fn disabling_ends_sessions_keeps_pairs_and_survives_clean_and_rotate() {
    let env = start().await;
    let (envid, _secret, t) = open_env(&env).await;
    let mut device = Device::pair(&env, &t).await;
    let chef = t.login("si:chef").await.unwrap().access_token;
    let session = t
        .authed(&chef, Some("acme"))
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap();

    // Disable: the session ends, the socket closes with a temporary code and a true reason.
    let (s, r) = lifecycle(&env, envid, "disable", 2, 1, 1).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("disabled")), "{r}");
    let (frames, closed) = device.until_closed().await;
    assert!(
        frames.iter().any(|f| matches!(
            f,
            ServiceFrame::SessionEnded {
                reason: EndReason::EnvironmentDisabled,
                ..
            }
        )),
        "{frames:?}"
    );
    assert!(
        !frames.iter().any(|f| matches!(f, ServiceFrame::Unpaired { .. })),
        "{frames:?}"
    );
    let (code, reason) = closed.expect("a close frame");
    assert_eq!(code, close::ENVIRONMENT_UNAVAILABLE);
    assert!(reason.contains("disabled") && reason.contains("paired"), "{reason}");
    let world = format!("extend_test_{}", envid.simple());
    let ended: (String, Option<String>) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT state, end_reason FROM {world}.sessions WHERE session_id = $1"
    )))
    .bind(session.session_id.as_str())
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(ended, ("ended".into(), Some("environment_disabled".into())));

    // The device keeps its pair: a temporary 503 that says so, never the 401 that unpairs apps.
    let (s, body) = device_self_status(&env, &device.credential).await;
    assert_eq!(s, 503, "{body}");
    assert_eq!(body["data"]["code"], "testing_environment_not_ready");
    assert!(body["data"]["message"].as_str().unwrap().contains("disabled"), "{body}");
    assert!(
        body["data"]["hint"].as_str().unwrap().contains("stays paired"),
        "{body}"
    );
    // The Silicon's side is refused, never run in production.
    assert_eq!(code_of(t.login("si:chef").await), ErrorCode::TestingSecretInvalid);

    // Restore reopens it with the same pair.
    assert_eq!(lifecycle(&env, envid, "restore", 3, 1, 1).await.0, 200);
    t.login("c:alice").await.unwrap(); // IAM confirms it: open again
    assert_eq!(env_state(&env, envid).await, "ready");
    let (s, body) = device_self_status(&env, &device.credential).await;
    assert_eq!(s, 200, "{body}");

    // Disabled stays disabled through rotate-key and clean, and prepare can't reopen it.
    assert_eq!(lifecycle(&env, envid, "disable", 4, 1, 1).await.0, 200);
    let (s, r) = lifecycle(&env, envid, "rotate-key", 5, 1, 2).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("disabled")), "{r}");
    assert_eq!(env_state(&env, envid).await, "disabled");
    let (s, r) = lifecycle(&env, envid, "clean", 6, 2, 2).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("disabled")), "{r}");
    assert_eq!(env_state(&env, envid).await, "disabled");
    assert_eq!(code_of(t.login("c:alice").await), ErrorCode::TestingSecretInvalid);
    let (s, r) = lifecycle(&env, envid, "prepare", 7, 2, 2).await;
    assert_eq!(s, 409, "{r}");
    assert!(r["data"]["hint"].as_str().unwrap().contains("restore"), "{r}");
    assert_eq!(env_state(&env, envid).await, "disabled");

    // A removed environment can't be restored or prepared again; purging again is harmless.
    assert_eq!(lifecycle(&env, envid, "purge", 8, 2, 2).await.0, 200);
    let (s, r) = lifecycle(&env, envid, "restore", 9, 2, 2).await;
    assert_eq!(s, 409, "{r}");
    assert!(
        r["data"]["message"].as_str().unwrap().contains("permanently removed"),
        "{r}"
    );
    assert_eq!(lifecycle(&env, envid, "prepare", 10, 2, 2).await.0, 409);
    assert_eq!(lifecycle(&env, envid, "purge", 11, 2, 2).await.0, 200);
    assert_eq!(env_state(&env, envid).await, "removed");
    let gone: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
        .bind(&world)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert!(!gone);
}

// ───────────────────────────── The secret on every route ─────────────────────────────

#[tokio::test]
async fn a_bad_test_secret_is_refused_on_every_route() {
    let env = start().await;
    let (disabled, disabled_secret, _) = open_env(&env).await;
    assert_eq!(lifecycle(&env, disabled, "disable", 2, 1, 1).await.0, 200);
    let (removed, removed_secret, _) = open_env(&env).await;
    assert_eq!(lifecycle(&env, removed, "purge", 2, 1, 1).await.0, 200);
    let prod = env.client.login("si:chef").await.unwrap().access_token;
    let e = enroll(&env.client).await;
    let enrollment_auth = format!("Extend-Enrollment {}", e.enrollment_secret);
    let bearer = format!("Bearer {prod}");
    let unknown = extend_protocol::ids::new_secret("ask_");
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("blank", b"   ".to_vec()),
        ("not printable", b"ask_\xff\xfe".to_vec()),
        ("malformed", b"ask_short".to_vec()),
        ("unknown", unknown.as_bytes().to_vec()),
        ("disabled environment", disabled_secret.as_bytes().to_vec()),
        ("removed environment", removed_secret.as_bytes().to_vec()),
    ];
    let enrollment_body = serde_json::json!({"type":"enrollment","data":{"os":"linux","app_version":"1.0.0"}});
    type Route<'a> = (
        reqwest::Method,
        String,
        Vec<(&'a str, Vec<u8>)>,
        Option<serde_json::Value>,
    );
    let routes: Vec<Route> = vec![
        (
            reqwest::Method::POST,
            "/api/v1/enrollments".into(),
            vec![],
            Some(enrollment_body),
        ),
        (
            reqwest::Method::GET,
            format!("/api/v1/enrollments/{}", e.enrollment_id),
            vec![("authorization", enrollment_auth.as_bytes().to_vec())],
            None,
        ),
        (reqwest::Method::GET, "/api/v1/iam".into(), vec![], None),
        (
            reqwest::Method::POST,
            "/api/v1/auth/login".into(),
            vec![],
            Some(serde_json::json!({"type":"login","data":{"slt":"c:alice"}})),
        ),
        (
            reqwest::Method::POST,
            "/api/v1/reports".into(),
            vec![
                ("authorization", bearer.as_bytes().to_vec()),
                ("x-org-id", b"acme".to_vec()),
            ],
            Some(serde_json::json!({"type":"report","data":{"message":"m","client_version":"t","context":{}}})),
        ),
        (
            reqwest::Method::POST,
            "/api/v1/telemetry".into(),
            vec![("authorization", bearer.as_bytes().to_vec())],
            Some(serde_json::json!({"type":"telemetry","data":{"event":"x"}})),
        ),
        (reqwest::Method::GET, "/api/v1/device".into(), vec![], None),
        (reqwest::Method::GET, "/api/v1/testing-environment".into(), vec![], None),
    ];
    for (why, value) in &bad {
        for (method, path, extra, body) in &routes {
            let mut headers: Vec<(&str, &[u8])> = extra.iter().map(|(k, v)| (*k, v.as_slice())).collect();
            headers.push(("x-testing-application-secret", value.as_slice()));
            let (s, b) = raw(&env, method.clone(), path, &headers, body.clone()).await;
            assert_eq!(s, 401, "{why} on {method} {path}: {b}");
            assert_eq!(
                b["data"]["code"], "testing_secret_invalid",
                "{why} on {method} {path}: {b}"
            );
            assert!(
                b["data"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("Nothing ran in production"),
                "{why} on {method} {path}: {b}"
            );
        }
    }
    // Sent twice, even with one good value.
    let (_, good, _) = open_env(&env).await;
    let r = env
        .http
        .get(format!("{}/api/v1/iam", env.base))
        .header("x-testing-application-secret", good.clone())
        .header("x-testing-application-secret", good.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // A good one works on the same routes.
    let (s, b) = raw(
        &env,
        reqwest::Method::GET,
        "/api/v1/iam",
        &[("x-testing-application-secret", good.as_bytes())],
        None,
    )
    .await;
    assert_eq!(s, 200, "{b}");
    // Version negotiation is the same in every world and stays unscoped.
    let (s, _) = raw(
        &env,
        reqwest::Method::GET,
        "/api/version",
        &[("x-testing-application-secret", b"")],
        None,
    )
    .await;
    assert_eq!(s, 200);
}

// ───────────────────────────── Pairing codes ─────────────────────────────

#[tokio::test]
async fn pairing_codes_pair_only_into_the_world_they_were_made_in() {
    let env = start().await;
    let (envid, _secret, t) = open_env(&env).await;
    let (_other, _other_secret, t2) = open_env(&env).await;
    let alice_p = env.client.login("c:alice").await.unwrap().access_token;
    let alice_t = t.login("c:alice").await.unwrap().access_token;

    // An app started with the test secret: its code is recorded for that environment.
    let te = enroll(&t).await;
    let schema: String =
        sqlx::query_scalar("SELECT world_schema FROM extend_global.enrollments WHERE enrollment_id = $1")
            .bind(te.enrollment_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(schema, format!("extend_test_{}", envid.simple()));

    // Production can't claim it, nor can another test environment.
    let e = env
        .client
        .authed(&alice_p, Some("acme"))
        .pair(&claim(&te.pairing_code))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::PairingCodeInvalid);
    let api = e.api().unwrap();
    assert!(
        api.message.contains("test environment") && api.message.contains("production"),
        "{}",
        api.message
    );
    assert!(api.hint.as_deref().unwrap().contains("--test"), "{api:?}");
    let alice_t2 = t2.login("c:alice").await.unwrap().access_token;
    let e = t2
        .authed(&alice_t2, Some("acme"))
        .pair(&claim(&te.pairing_code))
        .await
        .unwrap_err();
    assert!(
        e.api().unwrap().message.contains("different test environment"),
        "{:?}",
        e.api()
    );
    // Polling it from another environment is refused too.
    assert_eq!(
        code_of(t2.enrollment(te.enrollment_id, &te.enrollment_secret).await),
        ErrorCode::TestingSecretInvalid
    );

    // A production code can't be claimed from a test environment.
    let pe = enroll(&env.client).await;
    let e = t
        .authed(&alice_t, Some("acme"))
        .pair(&claim(&pe.pairing_code))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::PairingCodeInvalid);
    assert!(e.api().unwrap().message.contains("into production"), "{:?}", e.api());

    // Each is claimed in its own world.
    let d = t
        .authed(&alice_t, Some("acme"))
        .pair(&claim(&te.pairing_code))
        .await
        .unwrap();
    assert_eq!(
        t.authed(&alice_t, Some("acme"))
            .device(d.device_id.as_ref())
            .await
            .unwrap()
            .name,
        "Fake box"
    );
    env.client
        .authed(&alice_p, Some("acme"))
        .pair(&claim(&pe.pairing_code))
        .await
        .unwrap();
    match t.enrollment(te.enrollment_id, &te.enrollment_secret).await.unwrap() {
        EnrollmentState::Paired { environment, .. } => assert_eq!(environment.unwrap().environment_id, envid),
        other => panic!("{other:?}"),
    }

    // The database refuses a cross-world claim even if a handler forgot to check.
    let late = enroll(&t).await;
    let err = sqlx::query("UPDATE extend_global.enrollments SET paired_schema = 'extend', paired_device_id = 'x' WHERE enrollment_id = $1")
        .bind(late.enrollment_id)
        .execute(&env.pool)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("enrollments_claimed_in_own_world"), "{err}");

    // A clean takes the environment's waiting codes with it.
    assert_eq!(lifecycle(&env, envid, "clean", 2, 2, 1).await.0, 200);
    assert_eq!(
        code_of(t.enrollment(late.enrollment_id, &late.enrollment_secret).await),
        ErrorCode::EnrollmentNotFound
    );
}

/// Where a live pairing code was made is told only to a signed-in Carbon of a team. Anyone else
/// gets the claim's own refusal, the same answer as for a code that doesn't exist, and the answer
/// counts against each Carbon's own limit, not one shared by everyone behind the load balancer.
#[tokio::test]
async fn where_a_pairing_code_was_made_is_told_only_to_a_signed_in_carbon() {
    let env = start().await;
    let (_envid, secret, t) = open_env(&env).await;
    let test_code = enroll(&t).await.pairing_code;
    let prod_code = enroll(&env.client).await.pairing_code;
    let unknown = if test_code == "0F0F0F" || prod_code == "0F0F0F" {
        "F0F0F0"
    } else {
        "0F0F0F"
    };
    let body = |code: &str| serde_json::json!({"type": "pairing", "data": {"pairing_code": code, "name": "Fake box", "silicon_ids": ["si:chef"]}});
    type Headers = Vec<(&'static str, Vec<u8>)>;
    let claim_as = |headers: Headers, code: String| {
        let env = &env;
        let body = body(&code);
        async move {
            let h: Vec<(&str, &[u8])> = headers.iter().map(|(k, v)| (*k, v.as_slice())).collect();
            raw(env, reqwest::Method::POST, "/api/v1/pairings", &h, Some(body)).await
        }
    };
    let answer = |r: &(u16, serde_json::Value)| (r.0, r.1["data"]["code"].as_str().unwrap_or_default().to_owned());

    // Not signed in, a token IAM doesn't know, or a Silicon: a live code from another world gets
    // exactly what an unknown code gets, from production and from inside the test environment.
    let chef = env.client.login("si:chef").await.unwrap().access_token;
    let callers: Vec<(&str, Headers, &str, (u16, &str))> = vec![
        ("no sign-in", vec![], &test_code, (401, "not_signed_in")),
        (
            "no sign-in, test secret",
            vec![("x-testing-application-secret", secret.clone().into_bytes())],
            &prod_code,
            (401, "not_signed_in"),
        ),
        (
            "unknown token",
            vec![
                ("authorization", b"Bearer not-a-token".to_vec()),
                ("x-org-id", b"acme".to_vec()),
            ],
            &test_code,
            (0, ""),
        ),
        (
            "Silicon",
            vec![
                ("authorization", format!("Bearer {chef}").into_bytes()),
                ("x-org-id", b"acme".to_vec()),
            ],
            &test_code,
            (0, "carbon_only"),
        ),
    ];
    for (who, headers, live, want) in callers {
        let live_answer = answer(&claim_as(headers.clone(), live.to_owned()).await);
        let unknown_answer = answer(&claim_as(headers, unknown.to_owned()).await);
        assert_eq!(live_answer, unknown_answer, "{who}");
        assert_ne!(live_answer.1, "pairing_code_invalid", "{who}");
        if want.0 != 0 {
            assert_eq!(live_answer.0, want.0, "{who}");
        }
        if !want.1.is_empty() {
            assert_eq!(live_answer.1, want.1, "{who}");
        }
    }

    // A signed-in Carbon hears where the code was made, up to their own limit...
    let alice = env.client.login("c:alice").await.unwrap().access_token;
    for i in 0..20 {
        let e = env
            .client
            .authed(&alice, Some("acme"))
            .pair(&claim(&test_code))
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::PairingCodeInvalid, "{i}: {:?}", e.api());
        assert!(e.api().unwrap().message.contains("test environment"), "{:?}", e.api());
    }
    let e = env
        .client
        .authed(&alice, Some("acme"))
        .pair(&claim(&test_code))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::RateLimited, "{:?}", e.api());
    // ...which is theirs alone: another Carbon from the same address still gets the answer.
    let bob = env.client.login("c:bob").await.unwrap().access_token;
    let e = env
        .client
        .authed(&bob, Some("acme"))
        .pair(&claim(&test_code))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::PairingCodeInvalid, "{:?}", e.api());
}

// ───────────────────────────── The 10-environment limit ─────────────────────────────

#[tokio::test]
async fn ten_active_environments_hold_under_concurrency() {
    let env = start().await;
    let ops: Vec<Op> = (0..12).map(|_| Op::new(Uuid::new_v4(), "prepare", 1, 1, 1)).collect();
    let results = futures::future::join_all(ops.iter().map(|op| op.send(&env))).await;
    let ok = results.iter().filter(|(s, _)| *s == 200).count();
    let refused: Vec<&(u16, serde_json::Value)> = results.iter().filter(|(s, _)| *s == 409).collect();
    assert_eq!((ok, refused.len()), (10, 2), "{results:?}");
    for (_, body) in &refused {
        assert_eq!(body["data"]["code"], "test_environment_limit", "{body}");
        assert!(body["data"]["hint"].as_str().unwrap().contains("Free a slot"), "{body}");
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM extend_global.test_environments WHERE state IN ('preparing','ready','cleaning')",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(active, 10);

    // A refused operation has a failed receipt, and the identical retry works once a slot frees.
    let refused_op = ops.iter().zip(&results).find(|(_, (s, _))| *s == 409).unwrap().0;
    let (s, r) = refused_op.receipt(&env).await;
    assert_eq!((s, r["state"].as_str()), (200, Some("failed")), "{r}");
    assert!(r["error"].as_str().unwrap().contains("slots"), "{r}");
    let prepared: Vec<Uuid> = ops
        .iter()
        .zip(&results)
        .filter(|(_, (s, _))| *s == 200)
        .map(|(o, _)| o.env)
        .collect();
    assert_eq!(lifecycle(&env, prepared[0], "disable", 2, 1, 1).await.0, 200);
    let (s, r) = refused_op.send(&env).await;
    assert_eq!((s, r["state"].as_str()), (200, Some("completed")), "{r}");

    // A disabled one can't come back through prepare, and restore needs a free slot.
    assert_eq!(lifecycle(&env, prepared[0], "prepare", 3, 1, 1).await.0, 409);
    let (s, r) = lifecycle(&env, prepared[0], "restore", 4, 1, 1).await;
    assert_eq!(
        (s, r["data"]["code"].as_str()),
        (409, Some("test_environment_limit")),
        "{r}"
    );
    assert_eq!(env_state(&env, prepared[0]).await, "disabled");

    // Three disabled, nine active: concurrent restores get exactly the one free slot.
    for (i, id) in prepared[1..3].iter().enumerate() {
        assert_eq!(lifecycle(&env, *id, "disable", 2, 1, 1).await.0, 200, "{i}");
    }
    assert_eq!(lifecycle(&env, Uuid::new_v4(), "prepare", 1, 1, 1).await.0, 200);
    let restores: Vec<Op> = [prepared[0], prepared[1], prepared[2]]
        .iter()
        .map(|id| Op::new(*id, "restore", 10, 1, 1))
        .collect();
    let results = futures::future::join_all(restores.iter().map(|op| op.send(&env))).await;
    assert_eq!(results.iter().filter(|(s, _)| *s == 200).count(), 1, "{results:?}");
    assert_eq!(results.iter().filter(|(s, _)| *s == 409).count(), 2, "{results:?}");
}

async fn active_count(env: &Env) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM extend_global.test_environments WHERE state IN ('preparing','ready','cleaning')",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

async fn schema_exists(env: &Env, envid: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
        .bind(format!("extend_test_{}", envid.simple()))
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

/// A removed environment takes nothing but purge, and an operation superseded by a later one never
/// runs: retrying either must not bring an environment back, recreate its data, or take an 11th
/// active slot.
#[tokio::test]
async fn removed_and_superseded_operations_never_run_again() {
    let env = start().await;
    // Two environments that are disabled (they hold no slot), then all 10 slots in use.
    let (y, z) = (Uuid::new_v4(), Uuid::new_v4());
    for id in [y, z] {
        assert_eq!(lifecycle(&env, id, "prepare", 1, 1, 1).await.0, 200);
        assert_eq!(lifecycle(&env, id, "disable", 2, 1, 1).await.0, 200);
    }
    let mut full = Vec::new();
    for _ in 0..10 {
        let id = Uuid::new_v4();
        assert_eq!(lifecycle(&env, id, "prepare", 1, 1, 1).await.0, 200);
        full.push(id);
    }
    assert_eq!(active_count(&env).await, 10);

    // X's prepare is refused for want of a slot, then X is purged (with the same revision, as
    // e2e/cli-e2e.sh sends). Retrying the refused prepare doesn't bring X back as an 11th.
    let x = Uuid::new_v4();
    let prepare_x = Op::new(x, "prepare", 1, 1, 1);
    let (s, r) = prepare_x.send(&env).await;
    assert_eq!(
        (s, r["data"]["code"].as_str()),
        (409, Some("test_environment_limit")),
        "{r}"
    );
    let (s, r) = lifecycle(&env, x, "purge", 1, 1, 1).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("removed")), "{r}");
    let (s, r) = prepare_x.send(&env).await;
    assert_eq!(s, 409, "{r}");
    assert!(
        r["data"]["message"].as_str().unwrap().contains("permanently removed"),
        "{r}"
    );
    assert!(
        r["data"]["hint"].as_str().unwrap().contains("new test environment"),
        "{r}"
    );
    assert_eq!(env_state(&env, x).await, "removed");
    assert_eq!(active_count(&env).await, 10);
    assert!(!schema_exists(&env, x).await);

    // Y's restore is refused for want of a slot, then Y is purged: retrying the restore neither
    // reopens Y nor recreates its data. Purging again is harmless.
    let restore_y = Op::new(y, "restore", 3, 1, 1);
    let (s, r) = restore_y.send(&env).await;
    assert_eq!(
        (s, r["data"]["code"].as_str()),
        (409, Some("test_environment_limit")),
        "{r}"
    );
    assert!(schema_exists(&env, y).await);
    assert_eq!(lifecycle(&env, y, "purge", 4, 1, 1).await.0, 200);
    assert!(!schema_exists(&env, y).await);
    let (s, r) = restore_y.send(&env).await;
    assert_eq!(s, 409, "{r}");
    assert!(
        r["data"]["message"].as_str().unwrap().contains("permanently removed"),
        "{r}"
    );
    assert_eq!(env_state(&env, y).await, "removed");
    assert!(!schema_exists(&env, y).await);
    assert_eq!(active_count(&env).await, 10);
    assert_eq!(lifecycle(&env, y, "purge", 5, 1, 1).await.0, 200);
    assert_eq!(restore_y.receipt(&env).await.1["state"], "failed");

    // Z's restore is refused for want of a slot, then a later disable (same revision) supersedes
    // it. Once a slot frees, retrying the restore doesn't undo the disable.
    let restore_z = Op::new(z, "restore", 3, 1, 1);
    assert_eq!(restore_z.send(&env).await.0, 409);
    assert_eq!(lifecycle(&env, z, "disable", 3, 1, 1).await.0, 200);
    assert_eq!(lifecycle(&env, full[0], "disable", 2, 1, 1).await.0, 200);
    let (s, r) = restore_z.send(&env).await;
    assert_eq!(s, 409, "{r}");
    assert!(r["data"]["message"].as_str().unwrap().contains("superseded"), "{r}");
    assert_eq!(env_state(&env, z).await, "disabled");
    assert_eq!(active_count(&env).await, 9);

    // e2e/cli-e2e.sh's cleanup: a purge repeating the prepare's revision frees the slot.
    let (s, r) = lifecycle(&env, full[1], "purge", 1, 1, 1).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("removed")), "{r}");
    assert_eq!(active_count(&env).await, 8);
}

// ───────────────────────────── Receipts ─────────────────────────────

#[tokio::test]
async fn lifecycle_receipts_replay_conflict_serialize_and_survive_failures() {
    let env = start().await;
    let envid = Uuid::new_v4();
    let prepare = Op::new(envid, "prepare", 1, 1, 1);
    let (s, first) = prepare.send(&env).await;
    assert_eq!(s, 200, "{first}");
    // The identical instruction returns the stored receipt.
    assert_eq!(prepare.send(&env).await, (200, first.clone()));
    assert_eq!(prepare.receipt(&env).await, (200, first));
    // A changed instruction under the same operation id is a conflict.
    let mut changed = Op::new(envid, "prepare", 1, 1, 1);
    changed.id = prepare.id;
    changed.body = prepare.body.clone();
    changed.body["name"] = "renamed".into();
    let (s, r) = changed.send(&env).await;
    assert_eq!(s, 409, "{r}");
    assert!(
        r["data"]["message"].as_str().unwrap().contains("different instruction"),
        "{r}"
    );
    // A new operation may repeat the environment's revision (newer or equal, like the other
    // participants' fence; e2e/cli-e2e.sh sends revision 1 throughout)...
    let (s, r) = lifecycle(&env, envid, "rotate-key", 1, 1, 2).await;
    assert_eq!((s, r["state"].as_str()), (200, Some("completed")), "{r}");
    // An unknown receipt says what to do.
    let (s, r) = Op::new(envid, "clean", 9, 9, 1).receipt(&env).await;
    assert_eq!(s, 404);
    assert!(r["data"]["hint"].as_str().unwrap().contains("PUT"), "{r}");

    // Concurrent operations on one environment run one at a time: the newest revision wins and an
    // older one arriving second is refused, never applied over it.
    let a = Op::new(envid, "rotate-key", 2, 1, 3);
    let b = Op::new(envid, "rotate-key", 3, 1, 4);
    let (ra, rb) = tokio::join!(a.send(&env), b.send(&env));
    assert_eq!(rb.0, 200, "{rb:?}");
    assert!(ra.0 == 200 || ra.0 == 409, "{ra:?}");
    let (rev, key): (i64, i64) = sqlx::query_as(
        "SELECT environment_revision, key_version FROM extend_global.test_environments WHERE environment_id = $1",
    )
    .bind(envid)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!((rev, key), (3, 4));
    // ...but an older revision is stale.
    let (s, r) = lifecycle(&env, envid, "rotate-key", 2, 1, 5).await;
    assert_eq!(s, 409, "{r}");
    assert!(r["data"]["message"].as_str().unwrap().contains("older than"), "{r}");
    // Identical deliveries at once: one effect, one receipt.
    let disable = Op::new(envid, "disable", 4, 1, 4);
    let all = futures::future::join_all((0..5).map(|_| disable.send(&env))).await;
    assert!(all.iter().all(|r| r.0 == 200 && r.1 == all[0].1), "{all:?}");
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM extend_global.honeycomb_operations WHERE operation_id = $1")
            .bind(disable.id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);

    // Reopen, then a clean is durably pending while it waits for work in the world to finish.
    assert_eq!(lifecycle(&env, envid, "restore", 5, 1, 4).await.0, 200);
    let secret = register_app(&env, envid, true).await;
    let t = Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    t.login("c:alice").await.unwrap();
    let held = env.state.fence(envid).read_owned().await;
    let clean = Op::new(envid, "clean", 6, 2, 4);
    let running = {
        let url = clean.url(&env.base);
        let body = clean.body.clone();
        let http = env.http.clone();
        tokio::spawn(async move {
            http.put(url)
                .bearer_auth("hck_test")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (_, r) = clean.receipt(&env).await;
    assert_eq!(
        (r["state"].as_str(), r["target_state"].as_str()),
        (Some("pending"), Some("ready")),
        "{r}"
    );
    assert_eq!(env_state(&env, envid).await, "cleaning");
    drop(held);
    assert_eq!(running.await.unwrap(), 200);
    assert_eq!(clean.receipt(&env).await.1["state"], "completed");
    assert_eq!(env_state(&env, envid).await, "ready");

    // A clean that fails midway leaves a failed receipt and the environment closed; the identical
    // retry finishes it.
    let world = format!("extend_test_{}", envid.simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE {world}.reports RENAME TO reports_moved"
    )))
    .execute(&env.pool)
    .await
    .unwrap();
    let clean = Op::new(envid, "clean", 7, 3, 4);
    let (s, r) = clean.send(&env).await;
    assert_eq!(s, 500, "{r}");
    let (_, r) = clean.receipt(&env).await;
    assert_eq!(
        (r["state"].as_str(), r["target_state"].as_str()),
        (Some("failed"), Some("ready")),
        "{r}"
    );
    assert!(!r["error"].as_str().unwrap().is_empty());
    assert_eq!(env_state(&env, envid).await, "cleaning");
    assert_eq!(code_of(t.login("c:alice").await), ErrorCode::TestingEnvironmentNotReady);
    let (s, r) = lifecycle(&env, envid, "disable", 8, 3, 4).await;
    assert_eq!(s, 409, "{r}");
    assert!(r["data"]["hint"].as_str().unwrap().contains("pending clean"), "{r}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE {world}.reports_moved RENAME TO reports"
    )))
    .execute(&env.pool)
    .await
    .unwrap();
    let (s, r) = clean.send(&env).await;
    assert_eq!((s, r["state"].as_str()), (200, Some("completed")), "{r}");
    assert_eq!(env_state(&env, envid).await, "ready");
}

/// Honeycomb retiring other applications changes nothing here; retiring Extend clears its data and
/// closes the environment (restorable, like a disabled one).
#[tokio::test]
async fn retiring_extend_from_an_environment_clears_and_closes_it() {
    let env = start().await;
    let (envid, _secret, t) = open_env(&env).await;
    let _device = Device::pair(&env, &t).await;
    let mut other = Op::new(envid, "retire-applications", 2, 1, 1);
    other.body["retired_apps"] = serde_json::json!(["briefcase"]);
    let (s, r) = other.send(&env).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("ready")), "{r}");
    assert_eq!(r["retired_apps"], serde_json::json!(["briefcase"]));
    assert!(t.login("c:alice").await.is_ok());
    let mut extend = Op::new(envid, "retire-applications", 3, 1, 1);
    extend.body["retired_apps"] = serde_json::json!(["extend", "briefcase"]);
    let (s, r) = extend.send(&env).await;
    assert_eq!((s, r["target_state"].as_str()), (200, Some("disabled")), "{r}");
    assert_eq!(env_state(&env, envid).await, "disabled");
    assert_eq!(code_of(t.login("c:alice").await), ErrorCode::TestingSecretInvalid);
    let devices: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM extend_test_{}.devices",
        envid.simple()
    )))
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(devices, 0);
}

// ───────────────────────────── The clean fence ─────────────────────────────

/// A command still waiting on a device when a clean starts fails at once, and nothing it or the
/// session writes survives the clean.
#[tokio::test]
async fn a_clean_waits_for_work_in_flight_and_nothing_written_survives_it() {
    let env = start().await;
    let (envid, secret, t) = open_env(&env).await;
    let device = Device::pair(&env, &t).await; // never answers commands
    let chef = t.login("si:chef").await.unwrap().access_token;
    let session = t
        .authed(&chef, Some("acme"))
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap();
    let sid = session.session_id.to_string();
    let base = env.base.clone();
    let command = tokio::spawn(async move {
        let t = Client::builder(&base).testing_secret(&secret).connect().await.unwrap();
        let started = Instant::now();
        let r = t
            .authed(&chef, Some("acme"))
            .run(
                &sid,
                &CommandRequest {
                    command: "snapshot".into(),
                    args: vec![],
                    timeout_ms: Some(25_000),
                    self_destruct_minutes: None,
                    permanent: false,
                    attachments: vec![],
                },
            )
            .await;
        (r.is_ok(), started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The request waiting on the device holds the world's fence, so a clean can't wipe the world
    // under it (lifecycle_receipts_… shows a clean waits for the fence).
    assert!(
        env.state.fence(envid).try_write().is_err(),
        "an in-flight request holds the fence"
    );
    let started = Instant::now();
    let (s, r) = lifecycle(&env, envid, "clean", 2, 2, 1).await;
    assert_eq!(s, 200, "{r}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the clean waited {:?}",
        started.elapsed()
    );
    let (ok, took) = command.await.unwrap();
    assert!(!ok);
    assert!(took < Duration::from_secs(10), "the command waited {took:?}");
    drop(device);
    let world = format!("extend_test_{}", envid.simple());
    for table in ["activity", "sessions", "devices", "device_access", "requests"] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {world}.{table}")))
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} kept rows after the clean");
    }
}

/// Disabling ends the session a command is waiting in, so the command fails at once and the
/// disable (which waits for work in flight) isn't held until the command's timeout.
#[tokio::test]
async fn disabling_fails_a_command_still_waiting_on_a_device_at_once() {
    let env = start().await;
    let (envid, secret, t) = open_env(&env).await;
    let device = Device::pair(&env, &t).await; // never answers commands
    let chef = t.login("si:chef").await.unwrap().access_token;
    let sid = t
        .authed(&chef, Some("acme"))
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let base = env.base.clone();
    let command = tokio::spawn(async move {
        let t = Client::builder(&base).testing_secret(&secret).connect().await.unwrap();
        let started = Instant::now();
        let r = t
            .authed(&chef, Some("acme"))
            .run(
                &sid,
                &CommandRequest {
                    command: "snapshot".into(),
                    args: vec![],
                    timeout_ms: Some(25_000),
                    self_destruct_minutes: None,
                    permanent: false,
                    attachments: vec![],
                },
            )
            .await;
        (r.is_ok(), started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = Instant::now();
    assert_eq!(lifecycle(&env, envid, "disable", 2, 1, 1).await.0, 200);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the disable waited {:?}",
        started.elapsed()
    );
    let (ok, took) = command.await.unwrap();
    assert!(!ok);
    assert!(took < Duration::from_secs(10), "the command waited {took:?}");
    let (s, _) = device_self_status(&env, &device.credential).await;
    assert_eq!(s, 503);
}

/// A handler that selects the world again (as `GET /api/v1/iam` does) while its request already
/// holds the world's fence must not queue behind a waiting clean: that would deadlock both.
#[tokio::test]
async fn selecting_again_inside_a_request_never_waits_on_the_fence() {
    let env = start().await;
    let (envid, secret, _t) = open_env(&env).await;
    let held = env.state.fence(envid).read_owned().await; // the request's own guard
    let state = env.state.clone();
    let writer = tokio::spawn(async move {
        drop(state.fence_exclusive(envid).await);
    });
    tokio::time::sleep(Duration::from_millis(200)).await; // a clean is now waiting for the fence
    let again = tokio::time::timeout(Duration::from_secs(3), env.state.select_world(Some(&secret))).await;
    let (world, sel) = again.expect("select_world waited on the fence").unwrap();
    assert_eq!(
        (world.environment_id, sel.unwrap().environment_id),
        (Some(envid), envid)
    );
    drop(held);
    writer.await.unwrap();
}

// ───────────────────────────── Webhooks ─────────────────────────────

async fn deliver(env: &Env, event: serde_json::Value) -> u16 {
    env.http
        .post(format!("{}/webhook", env.base))
        .json(&event)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn session_state(env: &Env, schema: &str, sid: &str) -> (String, Option<String>) {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT state, end_reason FROM {schema}.sessions WHERE session_id = $1"
    )))
    .bind(sid)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

async fn recorded(env: &Env, schema: &str, event_id: &str) -> bool {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS (SELECT 1 FROM {schema}.iam_events WHERE event_id = $1)"
    )))
    .bind(event_id)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn iam_events_apply_in_order_once_and_only_after_they_succeed() {
    let env = start().await;
    let device = Device::pair(&env, &env.client).await;
    let chef = env.client.login("si:chef").await.unwrap().access_token;
    let sid = env
        .client
        .authed(&chef, Some("acme"))
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let event = |id: &str, kind: &str, version: Option<i64>, removed: bool| {
        let mut e = serde_json::json!({"event_id": id, "event_type": kind, "members": ["si:chef"], "teams": ["acme"]});
        if removed {
            e["removed"] = serde_json::json!([["si:chef", "acme"]]);
        }
        if let Some(v) = version {
            e["aggregate"] = serde_json::json!({"type": "silicon", "id": "si:chef", "version": v});
        }
        e
    };

    // A newer event, then an older removal arriving late: the late one is dropped.
    assert_eq!(
        deliver(&env, event("e3", "organization.member.updated.v1", Some(3), false)).await,
        204
    );
    assert_eq!(
        deliver(&env, event("e2", "organization.member.removed.v1", Some(2), true)).await,
        204
    );
    assert_eq!(session_state(&env, "extend", &sid).await.0, "active");
    assert!(recorded(&env, "extend", "e2").await);
    // A removal IAM doesn't confirm (the Silicon is still an active member) ends nothing.
    assert_eq!(
        deliver(&env, event("e2b", "organization.member.removed.v1", None, true)).await,
        204
    );
    assert_eq!(session_state(&env, "extend", &sid).await.0, "active");
    let alice = env.client.login("c:alice").await.unwrap().access_token;
    assert_eq!(
        env.client
            .authed(&alice, Some("acme"))
            .access(&device.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // IAM removes the Silicon. The first delivery fails midway: it isn't recorded, so IAM's
    // retry applies it.
    env.state
        .local_iam
        .as_ref()
        .unwrap()
        .set_member("si:chef", Some(vec![]))
        .await;
    sqlx::raw_sql("ALTER TABLE extend.device_access RENAME TO device_access_moved")
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        deliver(&env, event("e5", "organization.member.removed.v1", Some(5), true)).await,
        500
    );
    assert!(!recorded(&env, "extend", "e5").await);
    sqlx::raw_sql("ALTER TABLE extend.device_access_moved RENAME TO device_access")
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        deliver(&env, event("e5", "organization.member.removed.v1", Some(5), true)).await,
        204
    );
    assert!(recorded(&env, "extend", "e5").await);
    assert_eq!(
        session_state(&env, "extend", &sid).await,
        ("ended".into(), Some("left_team".into()))
    );
    assert!(
        env.client
            .authed(&alice, Some("acme"))
            .access(&device.id)
            .await
            .unwrap()
            .is_empty()
    );
    // A duplicate is acknowledged and applied once.
    let revoked = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM extend.activity WHERE action = 'access_revoked'")
            .fetch_one(&env.pool)
            .await
            .unwrap()
    };
    let before = revoked().await;
    assert_eq!(
        deliver(&env, event("e5", "organization.member.removed.v1", Some(5), true)).await,
        204
    );
    assert_eq!(revoked().await, before);
}

#[tokio::test]
async fn iam_events_for_closed_test_environments_are_dropped() {
    let env = start().await;
    let (disabled, _, _) = open_env(&env).await;
    assert_eq!(lifecycle(&env, disabled, "disable", 2, 1, 1).await.0, 200);
    let (removed, _, _) = open_env(&env).await;
    assert_eq!(lifecycle(&env, removed, "purge", 2, 1, 1).await.0, 200);
    let unknown = Uuid::new_v4();
    for (id, env_id) in [("d1", disabled), ("r1", removed), ("u1", unknown)] {
        let e = serde_json::json!({"event_id": id, "event_type": "organization.member.removed.v1",
            "members": ["si:chef"], "teams": ["acme"], "environment_id": env_id});
        assert_eq!(deliver(&env, e).await, 204, "{id}");
    }
    assert!(!recorded(&env, &format!("extend_test_{}", disabled.simple()), "d1").await);
    for gone in [removed, unknown] {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
            .bind(format!("extend_test_{}", gone.simple()))
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert!(!exists, "an event recreated {gone}'s data");
    }
}

// ───────────────────────────── Logout ─────────────────────────────

#[tokio::test]
async fn logging_out_with_only_a_refresh_token_ends_the_silicons_sessions() {
    let env = start().await;
    let device = Device::pair(&env, &env.client).await;
    let login = env.client.login("si:chef").await.unwrap();
    let sid = env
        .client
        .authed(&login.access_token, Some("acme"))
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    // No Authorization header: only the refresh token being revoked.
    let (s, b) = raw(
        &env,
        reqwest::Method::POST,
        "/api/v1/auth/logout",
        &[],
        Some(serde_json::json!({"type":"logout","data":{"token": login.refresh_token}})),
    )
    .await;
    assert_eq!(s, 204, "{b}");
    assert_eq!(
        session_state(&env, "extend", &sid).await,
        ("ended".into(), Some("silicon_logged_out".into()))
    );
    assert_eq!(
        code_of(env.client.authed(&login.access_token, Some("acme")).me().await),
        ErrorCode::TokenExpired
    );
    assert_eq!(
        code_of(env.client.refresh(&login.refresh_token, "logout-test-key").await),
        ErrorCode::TokenExpired
    );
    // An empty token says what to send.
    let (s, b) = raw(
        &env,
        reqwest::Method::POST,
        "/api/v1/auth/logout",
        &[],
        Some(serde_json::json!({"type":"logout","data":{"token":" "}})),
    )
    .await;
    assert_eq!(s, 422, "{b}");
    assert!(b["data"]["hint"].as_str().unwrap().contains("refresh token"), "{b}");
}

// ───────────────────────────── Test-plane login ─────────────────────────────

#[tokio::test]
async fn test_plane_member_id_login_refuses_unknown_and_inactive_ids() {
    let env = start().await;
    let (_envid, _secret, t) = open_env(&env).await;
    assert!(t.login("si:chef").await.is_ok());
    assert_eq!(code_of(t.login("c:nobody").await), ErrorCode::SltInvalid);
    env.state
        .local_iam
        .as_ref()
        .unwrap()
        .set_member("si:sous", Some(vec![]))
        .await;
    assert_eq!(code_of(t.login("si:sous").await), ErrorCode::NotATeamMember);
    env.state.local_iam.as_ref().unwrap().set_member("c:bob", None).await;
    assert_eq!(code_of(t.login("c:bob").await), ErrorCode::SltInvalid);
}

// ───────────────────────────── Packaging ─────────────────────────────

#[test]
fn the_docker_image_defaults_to_production() {
    let dockerfile = include_str!("../../../Dockerfile");
    let runtime = dockerfile.split("AS runtime").nth(1).expect("a runtime stage");
    assert!(runtime.contains("EXTEND_ENVIRONMENT=production"), "{runtime}");
}
