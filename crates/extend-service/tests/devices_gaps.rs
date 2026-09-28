//! Service tests for device reads after removal, the test-environment device limit under
//! concurrency, hosted devices end to end, and device-list paging with the online filter.
//!
//! Real PostgreSQL, the real HTTP and WebSocket stack, the official client crate, and scripted
//! fake devices speaking docs/device-protocol.md. Needs a PostgreSQL the tests can create databases
//! on: `EXTEND_TEST_ADMIN_URL` (default `postgres://extend:extend@127.0.0.1:5440/postgres`).

use std::net::SocketAddr;
use std::time::Duration;

use extend_protocol::frames::{AttachedStatus, CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ServiceFrame};
use extend_protocol::model::*;
use extend_protocol::{Capability, DeviceOs, ErrorCode, TEST_DEVICE_LIMIT_MESSAGE};
use extend_service::config::{Config, Environment, FilesMode, IamMode, TingMode};
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::{ActivityQuery, Client, DeviceQuery, ListQuery};
use sqlx::Connection as _;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Env {
    base: String,
    client: Client,
    /// The service's database, for checking what its connections are doing.
    db: String,
}

async fn start() -> Env {
    let admin = std::env::var("EXTEND_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://extend:extend@127.0.0.1:5440/postgres".into());
    let db = format!("extend_gaps_{}", Uuid::new_v4().simple());
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
        database_url: url.clone(),
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
    tokio::spawn(extend_service::serve_on(listener, state));
    let client = Client::connect(&base).await.unwrap();
    Env { base, client, db: url }
}

async fn login(c: &Client, who: &str) -> String {
    c.login(who).await.unwrap().access_token
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

fn enrollment(os: DeviceOs) -> EnrollmentCreate {
    EnrollmentCreate {
        os,
        os_version: Some("1".into()),
        model: Some("Fake".into()),
        app_version: "1.0.0".into(),
        engine_version: None,
    }
}

fn claim(code: &str, name: &str, silicons: &[&str]) -> PairingClaim {
    PairingClaim {
        pairing_code: code.to_owned(),
        name: name.to_owned(),
        visibility: None,
        pair_ttl_days: None,
        silicon_ids: silicons.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// Pairs a device that never connects (so it stays offline). Returns its id.
async fn pair_offline(client: &Client, token: &str, os: DeviceOs, name: &str) -> String {
    let e = client.enroll(&enrollment(os)).await.unwrap();
    client
        .authed(token, Some("acme"))
        .pair(&claim(&e.pairing_code, name, &[]))
        .await
        .unwrap()
        .device_id
        .to_string()
}

/// A scripted Extend app with a live socket.
struct FakeDevice {
    id: String,
    ws: Ws,
}

impl FakeDevice {
    /// Enrolls, lets `carbon` pair it (with access for `silicons`), connects and says hello.
    async fn pair(client: &Client, carbon: &str, os: DeviceOs, name: &str, silicons: &[&str]) -> FakeDevice {
        let e = client.enroll(&enrollment(os)).await.unwrap();
        let mut ews = ws_connect(
            &client.ws_url(&format!("/api/v1/enrollments/{}/connect", e.enrollment_id)),
            &format!("Extend-Enrollment {}", e.enrollment_secret),
        )
        .await;
        let first: EnrollmentFrame = serde_json::from_str(&next_text(&mut ews).await).unwrap();
        let EnrollmentFrame::Code { pairing_code, .. } = first else {
            panic!("expected code")
        };
        let token = client.login(carbon).await.unwrap().access_token;
        client
            .authed(&token, Some("acme"))
            .pair(&claim(&pairing_code, name, silicons))
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
        let mut d = FakeDevice {
            ws: ws_connect(
                &client.ws_url("/api/v1/device/connect"),
                &format!("Extend-Device {credential}"),
            )
            .await,
            id,
        };
        d.send(&DeviceFrame::Hello(Hello {
            app_version: "1.0.0".into(),
            os,
            os_version: Some("15".into()),
            model: Some("Fake".into()),
            engine_version: None,
            capabilities: os.full_capabilities().to_vec(),
            missing: vec![],
            setup: Setup::complete(),
            features: vec![],
        }))
        .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        d
    }

    async fn send(&mut self, f: &DeviceFrame) {
        self.ws
            .send(Message::Text(serde_json::to_string(f).unwrap().into()))
            .await
            .unwrap();
    }

    /// Next frame that isn't a ping or an environment notice (pings are answered).
    async fn recv(&mut self) -> ServiceFrame {
        loop {
            let f: ServiceFrame = serde_json::from_str(&next_text(&mut self.ws).await).unwrap();
            match f {
                ServiceFrame::Ping { nonce } => self.send(&DeviceFrame::Pong { nonce }).await,
                ServiceFrame::Environment { .. } => {}
                other => return other,
            }
        }
    }

    /// Reads frames until one matches, failing with everything seen when none does in time.
    async fn expect(&mut self, what: &str, want: impl Fn(&ServiceFrame) -> bool) -> ServiceFrame {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let Ok(f) = tokio::time::timeout_at(deadline, self.recv()).await else {
                panic!("no {what} frame in time; saw {seen:?}");
            };
            if want(&f) {
                return f;
            }
            seen.push(f);
        }
    }

    /// Answers every command with an echo, in the background, until the pair ends.
    fn serve(mut self) -> tokio::task::JoinHandle<Vec<ServiceFrame>> {
        tokio::spawn(async move {
            let mut seen = Vec::new();
            loop {
                let Ok(f) = tokio::time::timeout(Duration::from_secs(30), self.recv()).await else {
                    break;
                };
                if let ServiceFrame::Command(c) = &f {
                    self.send(&DeviceFrame::Result(CommandOutcome {
                        id: c.id,
                        ok: true,
                        output: serde_json::json!({"echo": c.args}),
                        text: Some(format!("ran {}", c.command)),
                        error: None,
                        files: vec![],
                    }))
                    .await;
                }
                let end = matches!(f, ServiceFrame::Unpaired { .. } | ServiceFrame::Superseded);
                seen.push(f);
                if end {
                    break;
                }
            }
            seen
        })
    }
}

fn cmd(name: &str, args: &[&str]) -> CommandRequest {
    CommandRequest {
        command: name.into(),
        args: args.iter().map(|x| (*x).to_owned()).collect(),
        timeout_ms: None,
        self_destruct_minutes: None,
        permanent: false,
        attachments: vec![],
    }
}

fn api_err<T: std::fmt::Debug>(r: Result<T, silicon_extend_client::Error>) -> extend_protocol::ApiError {
    r.expect_err("expected an error").api().expect("an API error").clone()
}

/// A removed device's record and activity log stay readable by the Carbon who paired it; everything
/// else about it answers device_not_found, saying it was removed and where its log is.
#[tokio::test]
async fn removed_device_stays_readable_to_its_owner() {
    let env = start().await;
    let alice = login(&env.client, "c:alice").await;
    let bob = login(&env.client, "c:bob").await;
    let chef = login(&env.client, "si:chef").await;
    let a = env.client.authed(&alice, Some("acme"));
    let b = env.client.authed(&bob, Some("acme"));
    let c = env.client.authed(&chef, Some("acme"));

    let phone = FakeDevice::pair(&env.client, "c:alice", DeviceOs::Android, "Alice's Pixel", &["si:chef"]).await;
    let id = phone.id.clone();
    let frames = phone.serve();
    let kept = pair_offline(&env.client, &alice, DeviceOs::Linux, "Alice's desk").await;

    // A session with a command, then removal while the session is still running.
    let sid = c
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    assert!(c.run(&sid, &cmd("snapshot", &["-i"])).await.unwrap().ok);
    a.remove_device(&id, None).await.unwrap();
    let ended = c.session(&sid).await.unwrap();
    assert_eq!(ended.state, SessionState::Ended);
    assert_eq!(ended.end_reason, Some(EndReason::DeviceRemoved));
    let frames = frames.await.unwrap();
    assert!(frames.iter().any(|f| matches!(
        f,
        ServiceFrame::SessionEnded {
            reason: EndReason::DeviceRemoved,
            ..
        }
    )));
    assert!(matches!(
        frames.last(),
        Some(ServiceFrame::Unpaired {
            reason: EndReason::DeviceRemoved
        })
    ));

    // The default list leaves it out; include_removed brings it back, flagged.
    let live = a.devices(DeviceQuery::default()).await.unwrap();
    assert_eq!(live.items.len(), 1);
    assert_eq!(live.items[0].device_id.to_string(), kept);
    assert!(live.items[0].removed_at.is_none());
    let all = a.devices_including_removed(DeviceQuery::default()).await.unwrap();
    assert_eq!(all.items.len(), 2);
    let gone = all.items.iter().find(|d| d.device_id.to_string() == id).unwrap();
    assert!(gone.removed_at.is_some());
    assert_eq!(gone.removed_reason, Some(EndReason::DeviceRemoved));
    assert!(!gone.online);
    assert!(gone.in_use.is_none());
    assert!(gone.days_left.is_none() && gone.pair_expires_at.is_none());
    let still = all.items.iter().find(|d| d.device_id.to_string() == kept).unwrap();
    assert!(still.removed_at.is_none() && still.removed_reason.is_none());
    // Removed devices are only offline.
    let online_only = a
        .devices_including_removed(DeviceQuery {
            online: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(online_only.items.is_empty());

    // The device itself and its log.
    let d = a.device(&id).await.unwrap();
    assert_eq!(d.name, "Alice's Pixel");
    assert_eq!(d.removed_reason, Some(EndReason::DeviceRemoved));
    assert!(d.removed_at.is_some());
    assert!(!d.online);
    assert_eq!(d.commands.as_deref(), Some(&[][..]));
    let log = a.activity(&id, ActivityQuery::default()).await.unwrap();
    let actions: Vec<&str> = log.items.iter().map(|e| e.action.as_str()).collect();
    for want in ["paired", "session_started", "command", "session_ended", "removed"] {
        assert!(actions.contains(&want), "{want} missing from {actions:?}");
    }
    assert_eq!(log.items[0].action, "removed", "newest first");
    assert!(
        a.device_requests(&id, ListQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(a.access(&id).await.unwrap().is_empty());
    a.setup(&id).await.unwrap();

    // Every change answers device_not_found, saying what happened and where the log is.
    let check = |e: extend_protocol::ApiError| {
        assert_eq!(e.code, ErrorCode::DeviceNotFound, "{e:?}");
        assert!(e.message.contains("was removed"), "{}", e.message);
        assert!(e.message.contains(&id), "{}", e.message);
        let hint = e.hint.unwrap_or_default();
        assert!(hint.contains(&format!("extend device activity {id}")), "{hint}");
    };
    check(api_err(
        a.update_device(
            &id,
            None,
            &DevicePatch {
                name: Some("New name".into()),
                ..Default::default()
            },
        )
        .await,
    ));
    check(api_err(a.remove_device(&id, None).await));
    check(api_err(a.stop_device(&id).await));
    check(api_err(a.grant(&id, "si:chef").await));
    check(api_err(a.revoke(&id, "si:chef").await));
    check(api_err(a.setup_code(&id, "1234").await));
    check(api_err(
        a.attach(
            &id,
            &AttachmentCreate {
                os: DeviceOs::Tvos,
                name: "TV".into(),
                visibility: None,
                pair_ttl_days: None,
                address: None,
            },
        )
        .await,
    ));

    // Nobody else learns it existed: another Carbon and the Silicon get the plain not-found.
    for e in [api_err(b.device(&id).await), api_err(c.device(&id).await)] {
        assert_eq!(e.code, ErrorCode::DeviceNotFound);
        assert!(!e.message.contains("removed"), "{}", e.message);
    }
    assert_eq!(
        api_err(b.activity(&id, ActivityQuery::default()).await).code,
        ErrorCode::DeviceNotFound
    );
    assert!(
        b.devices_including_removed(DeviceQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let e = api_err(c.start_session(&id.parse().unwrap()).await);
    assert_eq!(e.code, ErrorCode::DeviceNotFound);

    // include_removed is for the Carbon's own devices only.
    let e = api_err(
        a.devices_including_removed(DeviceQuery {
            scope: Some("team".into()),
            ..Default::default()
        })
        .await,
    );
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert!(e.message.contains("scope=mine"), "{}", e.message);
    let e = api_err(c.devices_including_removed(DeviceQuery::default()).await);
    assert_eq!(e.code, ErrorCode::InvalidInput);
    let raw = reqwest::Client::new()
        .get(format!("{}/api/v1/devices?include_removed=maybe", env.base))
        .bearer_auth(&alice)
        .header(extend_protocol::TEAM_HEADER, "acme")
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status(), 422);
    let body: serde_json::Value = raw.json().await.unwrap();
    assert_eq!(body["data"]["code"], "invalid_input", "{body}");
    assert!(
        body["data"]["message"].as_str().unwrap().contains("include_removed"),
        "{body}"
    );
}

async fn test_environment(env: &Env) -> Client {
    test_environment_with_id(env).await.0
}

/// A test environment, a client that selects it, and its id.
async fn test_environment_with_id(env: &Env) -> (Client, Uuid) {
    let envid = Uuid::new_v4();
    let secret = extend_protocol::ids::new_secret("ask_");
    let op = Uuid::new_v4();
    let r = reqwest::Client::new()
        .put(format!(
            "{}/internal/honeycomb/organizations/acme/testing-environments/{envid}/operations/{op}",
            env.base
        ))
        .bearer_auth("hck_test")
        .json(&serde_json::json!({
            "operation_id": op, "environment_id": envid, "org_id": "acme", "app_id": "extend",
            "environment_revision": 1, "generation": 1, "key_version": 1, "action": "prepare",
            "testing_key": "abcdefghijklmnopqrstuvwxyz012345", "name": format!("limit-race {envid}")
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let r = reqwest::Client::new()
        .post(format!("{}/dev/iam/test-apps", env.base))
        .json(&serde_json::json!({"type":"test_app","data":{"secret": secret, "environment_id": envid}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let client = Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    (client, envid)
}

/// Retrying the successful fifth pairing must replay it before checking the new-device limit.
#[tokio::test]
async fn pairing_retry_replays_success_after_filling_the_test_environment() {
    let env = start().await;
    let t = test_environment(&env).await;
    let alice = login(&t, "c:alice").await;
    for i in 0..4 {
        pair_offline(&t, &alice, DeviceOs::Linux, &format!("existing {i}")).await;
    }
    let code = t.enroll(&enrollment(DeviceOs::Linux)).await.unwrap().pairing_code;
    let body = serde_json::json!({"type": "pairing", "data": claim(&code, "fifth", &[])});
    let http = reqwest::Client::new();
    let send = |body: serde_json::Value| {
        http.post(format!("{}/api/v1/pairings", env.base))
            .bearer_auth(&alice)
            .header("x-org-id", "acme")
            .header(extend_protocol::TESTING_SECRET_HEADER, t.testing_secret().unwrap())
            .header("idempotency-key", "pairing-fifth-device")
            .json(&body)
            .send()
    };
    let first = send(body.clone()).await.unwrap();
    assert_eq!(first.status(), 201);
    let first = first.json::<serde_json::Value>().await.unwrap();
    let retry = send(body.clone()).await.unwrap();
    assert_eq!(retry.status(), 201, "{}", retry.text().await.unwrap_or_default());
    assert_eq!(retry.headers()["idempotency-replayed"], "true");
    assert_eq!(retry.json::<serde_json::Value>().await.unwrap(), first);
    let mut changed = body;
    changed["data"]["name"] = "different body".into();
    let conflict = send(changed).await.unwrap();
    assert_eq!(conflict.status(), 409);
    let conflict = conflict.json::<serde_json::Value>().await.unwrap();
    assert_eq!(conflict["data"]["code"], "conflict");
    assert!(
        conflict["data"]["message"]
            .as_str()
            .unwrap()
            .contains("Idempotency-Key")
    );
    let fresh = t.enroll(&enrollment(DeviceOs::Linux)).await.unwrap();
    let full = t
        .authed(&alice, Some("acme"))
        .pair(&claim(&fresh.pairing_code, "sixth", &[]))
        .await
        .unwrap_err();
    assert_eq!(full.api().unwrap().code, ErrorCode::TestDeviceLimit);
    assert_eq!(
        t.authed(&alice, Some("acme"))
            .devices(DeviceQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        5
    );
}

/// Eight devices added at once to a test environment that has three: exactly two get in, whether
/// they arrive by pairing code or through a host computer.
#[tokio::test]
async fn test_device_limit_holds_under_concurrent_adds() {
    let env = start().await;
    let t = test_environment(&env).await;
    let alice = login(&t, "c:alice").await;
    let a = t.authed(&alice, Some("acme"));

    // Three devices: a connected Mac that can carry others, and two that never connect.
    let host = FakeDevice::pair(&t, "c:alice", DeviceOs::Macos, "Test Mac", &[]).await;
    pair_offline(&t, &alice, DeviceOs::Linux, "one").await;
    pair_offline(&t, &alice, DeviceOs::Android, "two").await;
    assert_eq!(a.devices(DeviceQuery::default()).await.unwrap().items.len(), 3);

    let mut codes = Vec::new();
    for _ in 0..4 {
        codes.push(t.enroll(&enrollment(DeviceOs::Linux)).await.unwrap().pairing_code);
    }
    let claims = codes.iter().enumerate().map(|(i, code)| {
        let c = claim(code, &format!("claim {i}"), &[]);
        async move { a.pair(&c).await.map(|_| ()) }
    });
    let attaches = (0..4).map(|i| {
        let host = host.id.clone();
        async move {
            a.attach(
                &host,
                &AttachmentCreate {
                    os: DeviceOs::Tvos,
                    name: format!("attach {i}"),
                    visibility: None,
                    pair_ttl_days: None,
                    address: None,
                },
            )
            .await
            .map(|_| ())
        }
    });
    let (claimed, attached) = futures::join!(futures::future::join_all(claims), futures::future::join_all(attaches));
    let results: Vec<_> = claimed.into_iter().chain(attached).collect();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 2, "{results:?}");
    for r in results.into_iter().filter_map(Result::err) {
        let e = r.api().expect("an API error");
        assert_eq!(e.code, ErrorCode::TestDeviceLimit, "{e:?}");
        assert_eq!(e.message, TEST_DEVICE_LIMIT_MESSAGE);
        assert!(e.hint.as_deref().unwrap_or_default().contains("extend device rm"));
    }
    assert_eq!(a.devices(DeviceQuery::default()).await.unwrap().items.len(), 5);
    drop(host);
}

/// A burst of claims into one test environment, more than the service has database connections
/// (32), takes turns without holding connections while it waits: the burst ends in well under a
/// second or two, nobody gets a 5xx, the limit holds, and production reads during the burst stay
/// fast. (Waiting on the lock with a pooled connection in hand, while the turn's holder needed a
/// second connection, stalled the whole service for the 30 s pool timeout.)
#[tokio::test]
async fn test_device_limit_burst_keeps_the_service_responsive() {
    const CLAIMS: usize = 54; // + 3 below stays under the 60 enrollments an address may start an hour
    let env = start().await;
    let t = test_environment(&env).await;
    let alice = login(&t, "c:alice").await;
    let a = t.authed(&alice, Some("acme"));
    for i in 0..3 {
        pair_offline(&t, &alice, DeviceOs::Linux, &format!("before {i}")).await;
    }
    let mut codes = Vec::new();
    for _ in 0..CLAIMS {
        codes.push(t.enroll(&enrollment(DeviceOs::Linux)).await.unwrap().pairing_code);
    }
    // A production Carbon with a device, reading their list while the burst runs.
    let bob = login(&env.client, "c:bob").await;
    let b = env.client.authed(&bob, Some("acme"));
    pair_offline(&env.client, &bob, DeviceOs::Android, "bob's phone").await;

    let done = std::sync::atomic::AtomicBool::new(false);
    let burst = async {
        let started = std::time::Instant::now();
        let results = futures::future::join_all(codes.iter().enumerate().map(|(i, code)| {
            let c = claim(code, &format!("burst {i}"), &[]);
            async move { a.pair(&c).await.map(|_| ()) }
        }))
        .await;
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        (results, started.elapsed())
    };
    let production_reads = async {
        let mut slowest = Duration::ZERO;
        let mut reads = 0;
        tokio::time::sleep(Duration::from_millis(10)).await;
        loop {
            let started = std::time::Instant::now();
            let list = b
                .devices(DeviceQuery::default())
                .await
                .expect("production read during the burst");
            assert_eq!(list.items.len(), 1);
            slowest = slowest.max(started.elapsed());
            reads += 1;
            if done.load(std::sync::atomic::Ordering::SeqCst) {
                return (reads, slowest);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let ((results, took), (reads, slowest)) = futures::join!(burst, production_reads);

    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 2, "{results:?}");
    for r in results.iter().filter_map(|r| r.as_ref().err()) {
        let e = r.api().unwrap_or_else(|| panic!("an API error, got {r:?}"));
        assert_eq!(e.code, ErrorCode::TestDeviceLimit, "no 5xx or other refusal: {e:?}");
        assert_eq!(e.message, TEST_DEVICE_LIMIT_MESSAGE);
    }
    assert!(took < Duration::from_secs(5), "{CLAIMS} claims took {took:?}");
    assert!(
        slowest < Duration::from_secs(2),
        "a production read took {slowest:?} during the burst ({reads} reads)"
    );
    assert_eq!(a.devices(DeviceQuery::default()).await.unwrap().items.len(), 5);
    // A second burst into the full environment is refused up front, just as fast.
    let e = api_err(a.pair(&claim("ABCDEF", "late", &[])).await);
    assert_eq!(e.code, ErrorCode::TestDeviceLimit);
    eprintln!("{CLAIMS} claims in {took:?}; slowest of {reads} production reads {slowest:?}");
}

/// The advisory lock another Extend process would hold while it adds a device to `envid`.
async fn hold_add_lock(env: &Env, envid: Uuid) -> sqlx::PgConnection {
    let mut conn = sqlx::PgConnection::connect(&env.db).await.unwrap();
    sqlx::query("BEGIN").execute(&mut conn).await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(7342010, hashtext($1))")
        .bind(format!("extend_test_{}", envid.simple()))
        .execute(&mut conn)
        .await
        .unwrap();
    conn
}

/// Service connections waiting on an advisory lock right now.
async fn advisory_waiters(env: &Env) -> i64 {
    let mut conn = sqlx::PgConnection::connect(&env.db).await.unwrap();
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity
         WHERE datname = current_database() AND wait_event_type = 'Lock' AND wait_event = 'advisory'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap()
}

/// While another process holds an environment's add lock, forty claims into it wait in memory:
/// one database connection waits on the lock, not forty, and production keeps its connections.
/// When the lock goes, they finish, and the limit holds.
#[tokio::test]
async fn test_environment_adds_wait_in_memory_not_on_connections() {
    let env = start().await;
    let (t, envid) = test_environment_with_id(&env).await;
    let alice = login(&t, "c:alice").await;
    let a = t.authed(&alice, Some("acme"));
    let mut codes = Vec::new();
    for _ in 0..40 {
        codes.push(t.enroll(&enrollment(DeviceOs::Linux)).await.unwrap().pairing_code);
    }
    let bob = login(&env.client, "c:bob").await;
    let b = env.client.authed(&bob, Some("acme"));

    let other_process = hold_add_lock(&env, envid).await;
    let claims = futures::future::join_all(codes.iter().enumerate().map(|(i, code)| {
        let c = claim(code, &format!("waiting {i}"), &[]);
        async move { a.pair(&c).await.map(|_| ()) }
    }));
    let watch = async {
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let waiting = advisory_waiters(&env).await;
        let started = std::time::Instant::now();
        let list = b
            .devices(DeviceQuery::default())
            .await
            .expect("production read while claims wait");
        let read = started.elapsed();
        drop(other_process); // the other process's transaction ends
        (waiting, read, list.items.len())
    };
    let (results, (waiting, read, listed)) = futures::join!(claims, watch);
    assert_eq!(
        waiting, 1,
        "only the turn's holder may wait on the lock with a connection"
    );
    assert!(
        read < Duration::from_secs(1),
        "a production read took {read:?} while claims waited"
    );
    assert_eq!(listed, 0);
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 5, "{results:?}");
    for e in results.iter().filter_map(|r| r.as_ref().err()) {
        assert_eq!(e.api().expect("an API error").code, ErrorCode::TestDeviceLimit, "{e:?}");
    }
    assert_eq!(a.devices(DeviceQuery::default()).await.unwrap().items.len(), 5);
}

/// A claim that can't get its turn (another process kept the environment's add lock) is refused
/// with rate_limited, saying what happened, why and what to do, instead of hanging; once the lock
/// goes, claims succeed again. Waiting in memory is bounded too (10 s) for the ones queued behind.
#[tokio::test]
async fn test_environment_add_without_a_turn_is_refused_as_busy() {
    let env = start().await;
    let (t, envid) = test_environment_with_id(&env).await;
    let alice = login(&t, "c:alice").await;
    let a = t.authed(&alice, Some("acme"));
    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(t.enroll(&enrollment(DeviceOs::Linux)).await.unwrap().pairing_code);
    }
    let other_process = hold_add_lock(&env, envid).await;
    let started = std::time::Instant::now();
    // Four at once: the first waits on the lock (5 s), the next gets the turn after it (5 s more),
    // and the ones behind give up waiting for the turn after 10 s.
    let results = futures::future::join_all(codes[..4].iter().enumerate().map(|(i, code)| {
        let c = claim(code, &format!("busy {i}"), &[]);
        async move {
            let r = a.pair(&c).await.map(|_| ());
            (r, started.elapsed())
        }
    }))
    .await;
    let took = started.elapsed();
    let mut waits: Vec<Duration> = Vec::new();
    for (r, at) in results {
        let e = api_err(r);
        assert_eq!(e.code, ErrorCode::RateLimited, "{e:?}");
        assert!(e.message.contains("busy adding other devices"), "{}", e.message);
        assert!(e.message.contains("one at a time"), "{}", e.message);
        assert_eq!(e.hint.as_deref(), Some("Try again in a few seconds."));
        assert_eq!(e.details["retry_after_s"], 2);
        waits.push(at);
    }
    waits.sort();
    // The turn's holder gives up on the lock after 5 s; the one after it at 10 s; the ones queued
    // behind give up waiting for the turn at 10 s (one of them may just get the turn instead and
    // give up on the lock at 15 s).
    assert!(
        waits[0] >= Duration::from_millis(4_900) && waits[0] < Duration::from_secs(7),
        "{waits:?}"
    );
    assert!(
        waits[2] >= Duration::from_millis(9_900) && waits[2] < Duration::from_secs(12),
        "{waits:?}"
    );
    assert!(took < Duration::from_secs(17), "four claims took {took:?}");
    drop(other_process);
    // The lock is gone: the next claim gets straight in.
    let d = a
        .pair(&claim(&codes[4], "after", &[]))
        .await
        .expect("a claim once the lock is free");
    assert_eq!(d.name, "after");
}

/// Attach → the host hears about it → setup code relayed → the host reports the device → a session
/// on it reaches the host with `target` → the host's Stop for that target ends only that session.
#[tokio::test]
async fn hosted_device_end_to_end() {
    let env = start().await;
    let alice = login(&env.client, "c:alice").await;
    let chef = login(&env.client, "si:chef").await;
    let sous = login(&env.client, "si:sous").await;
    let a = env.client.authed(&alice, Some("acme"));
    let c = env.client.authed(&chef, Some("acme"));
    let s = env.client.authed(&sous, Some("acme"));

    let mut host = FakeDevice::pair(&env.client, "c:alice", DeviceOs::Macos, "Studio Mac", &["si:sous"]).await;
    let host_id = host.id.clone();
    let linux = pair_offline(&env.client, &alice, DeviceOs::Linux, "Linux box").await;

    // Refusals: a device that runs the app itself, the wrong host OS, an offline host, a Silicon.
    let tv = |os| AttachmentCreate {
        os,
        name: "Living room TV".into(),
        visibility: None,
        pair_ttl_days: None,
        address: Some("192.168.1.40".into()),
    };
    assert_eq!(
        api_err(a.attach(&host_id, &tv(DeviceOs::Android)).await).code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        api_err(a.attach(&linux, &tv(DeviceOs::Tvos)).await).code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        api_err(a.attach(&linux, &tv(DeviceOs::SamsungTv)).await).code,
        ErrorCode::DeviceOffline
    );
    assert_eq!(
        api_err(c.attach(&host_id, &tv(DeviceOs::Tvos)).await).code,
        ErrorCode::CarbonOnly
    );

    // Attach: the host is told to set it up.
    let atv = a.attach(&host_id, &tv(DeviceOs::Tvos)).await.unwrap();
    let atv_id = atv.device_id.to_string();
    assert_eq!(
        atv.host_device_id.as_ref().map(ToString::to_string).as_deref(),
        Some(host_id.as_str())
    );
    assert!(!atv.online, "not online until the host reports it");
    let f = host
        .expect("attach", |f| matches!(f, ServiceFrame::Attach { .. }))
        .await;
    assert_eq!(
        f,
        ServiceFrame::Attach {
            device_id: atv.device_id.clone(),
            os: DeviceOs::Tvos,
            name: "Living room TV".into(),
            address: Some("192.168.1.40".into()),
            removed: false,
            in_use_indicator: InUseIndicator::Shown,
        }
    );
    // A carried device can't carry others.
    assert_eq!(
        api_err(a.attach(&atv_id, &tv(DeviceOs::Tvos)).await).code,
        ErrorCode::InvalidInput
    );

    // The Apple TV's setup code reaches the host; malformed codes and non-hosted devices are refused.
    a.setup_code(&atv_id, " 4821 ").await.unwrap();
    let f = host
        .expect("setup_code", |f| matches!(f, ServiceFrame::SetupCode { .. }))
        .await;
    assert_eq!(
        f,
        ServiceFrame::SetupCode {
            device_id: atv.device_id.clone(),
            code: "4821".into(),
        }
    );
    assert_eq!(
        api_err(a.setup_code(&atv_id, "48a1").await).code,
        ErrorCode::InvalidInput
    );
    let e = api_err(a.setup_code(&host_id, "4821").await);
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert!(e.hint.is_some(), "{e:?}");

    // Before the host reports it, nobody can use it.
    a.grant(&atv_id, "si:chef").await.unwrap();
    assert_eq!(
        api_err(c.start_session(&atv.device_id).await).code,
        ErrorCode::DeviceOffline
    );

    // The host reports it online and set up.
    host.send(&DeviceFrame::Attached(AttachedStatus {
        device_id: atv.device_id.clone(),
        online: true,
        os_version: Some("18.2".into()),
        model: Some("AppleTV14,1".into()),
        capabilities: DeviceOs::Tvos.full_capabilities().to_vec(),
        missing: vec![],
        setup: Setup::complete(),
        awake: None,
        sleep_state: None,
        hardware_key: None,
    }))
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let seen = c.device(&atv_id).await.unwrap();
    assert!(seen.online);
    assert_eq!(seen.state, DeviceState::Ready);
    assert_eq!(seen.os_version.as_deref(), Some("18.2"));
    assert_eq!(
        seen.host_device_id.as_ref().map(ToString::to_string).as_deref(),
        Some(host_id.as_str())
    );
    assert!(seen.commands.as_ref().unwrap().contains(&"tv-remote".to_owned()));
    assert!(seen.capabilities.as_ref().unwrap().contains(&Capability::InputRemote));
    let listed = c.devices(DeviceQuery::default()).await.unwrap();
    assert_eq!(listed.items.len(), 1, "chef has access to the TV only");
    assert!(listed.items[0].online);

    // sous uses the Mac itself; chef uses the TV through it.
    let mac_sid = s.start_session(&host_id.parse().unwrap()).await.unwrap().session_id;
    let f = host
        .expect("session_started", |f| matches!(f, ServiceFrame::SessionStarted { .. }))
        .await;
    assert!(matches!(f, ServiceFrame::SessionStarted { target: None, ref session_id, .. } if *session_id == mac_sid));
    let tv_sid = c.start_session(&atv.device_id).await.unwrap().session_id;
    let f = host
        .expect("session_started", |f| matches!(f, ServiceFrame::SessionStarted { .. }))
        .await;
    assert!(
        matches!(f, ServiceFrame::SessionStarted { target: Some(ref t), ref session_id, ref silicon_id, .. }
            if *t == atv.device_id && *session_id == tv_sid && silicon_id == "si:chef"),
        "{f:?}"
    );

    // A command on the TV travels to the host with target set, and the host's answer comes back.
    let tv_sid_s = tv_sid.to_string();
    let run = tokio::spawn({
        let base = env.base.clone();
        let chef = chef.clone();
        let sid = tv_sid_s.clone();
        async move {
            let client = Client::connect(&base).await.unwrap();
            client
                .authed(&chef, Some("acme"))
                .run(&sid, &cmd("tv-remote", &["select"]))
                .await
        }
    });
    let f = host.expect("command", |f| matches!(f, ServiceFrame::Command(_))).await;
    let ServiceFrame::Command(frame) = f else {
        unreachable!()
    };
    assert_eq!(frame.target.as_ref(), Some(&atv.device_id));
    assert_eq!(frame.session_id, tv_sid);
    assert_eq!(frame.command, "tv-remote");
    assert_eq!(frame.args, vec!["select".to_owned()]);
    host.send(&DeviceFrame::Result(CommandOutcome {
        id: frame.id,
        ok: true,
        output: serde_json::json!({"pressed": "select"}),
        text: Some("Pressed Select".into()),
        error: None,
        files: vec![],
    }))
    .await;
    let res = run.await.unwrap().unwrap();
    assert!(res.ok);
    assert_eq!(res.text.as_deref(), Some("Pressed Select"));
    // A Mac-only command is refused on the TV without bothering the host.
    assert_eq!(
        api_err(c.run(&tv_sid_s, &cmd("terminal", &["run", "ls"])).await).code,
        ErrorCode::UnsupportedOnDevice
    );
    // The command is in the TV's log, not the Mac's.
    let tv_log = a.activity(&atv_id, ActivityQuery::default()).await.unwrap();
    assert!(tv_log.items.iter().any(|e| e.command.as_deref() == Some("tv-remote")));
    let mac_log = a.activity(&host_id, ActivityQuery::default()).await.unwrap();
    assert!(!mac_log.items.iter().any(|e| e.command.as_deref() == Some("tv-remote")));

    // A Stop for a device this host doesn't carry does nothing.
    host.send(&DeviceFrame::Stop {
        target: Some(linux.parse().unwrap()),
    })
    .await;
    // The host's Stop for the TV ends the TV's session only.
    host.send(&DeviceFrame::Stop {
        target: Some(atv.device_id.clone()),
    })
    .await;
    let f = host
        .expect("session_ended", |f| matches!(f, ServiceFrame::SessionEnded { .. }))
        .await;
    assert!(
        matches!(f, ServiceFrame::SessionEnded { target: Some(ref t), ref session_id, reason: EndReason::StoppedByCarbon }
            if *t == atv.device_id && *session_id == tv_sid),
        "{f:?}"
    );
    let ended = c.session(&tv_sid_s).await.unwrap();
    assert_eq!(ended.state, SessionState::Ended);
    assert_eq!(ended.end_reason, Some(EndReason::StoppedByCarbon));
    let mac_session = s.session(mac_sid.as_str()).await.unwrap();
    assert_eq!(
        mac_session.state,
        SessionState::Active,
        "the Mac's own session keeps running"
    );
    assert!(a.device(&host_id).await.unwrap().in_use.is_some());
    assert!(a.device(&atv_id).await.unwrap().in_use.is_none());

    // Removing the TV tells the host to forget it; the Mac stays paired and in use.
    a.remove_device(&atv_id, None).await.unwrap();
    let f = host
        .expect("attach removed", |f| {
            matches!(f, ServiceFrame::Attach { removed: true, .. })
        })
        .await;
    assert!(matches!(f, ServiceFrame::Attach { ref device_id, .. } if *device_id == atv.device_id));
    assert!(a.device(&host_id).await.unwrap().removed_at.is_none());
    assert_eq!(s.session(mac_sid.as_str()).await.unwrap().state, SessionState::Active);

    // Removing the host removes what it carries.
    let second = a.attach(&host_id, &tv(DeviceOs::LgTv)).await.unwrap();
    // Only an Apple TV takes a setup code.
    let e = api_err(a.setup_code(second.device_id.as_str(), "4821").await);
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert!(e.message.contains("only an Apple TV"), "{}", e.message);
    a.remove_device(&host_id, None).await.unwrap();
    let gone = a.device(second.device_id.as_str()).await.unwrap();
    assert_eq!(gone.removed_reason, Some(EndReason::DeviceRemoved));
    let f = host
        .expect("unpaired", |f| matches!(f, ServiceFrame::Unpaired { .. }))
        .await;
    assert_eq!(
        f,
        ServiceFrame::Unpaired {
            reason: EndReason::DeviceRemoved
        }
    );
}

/// Collects every page of a device list, checking that each page but the last is full.
async fn all_pages(a: silicon_extend_client::Authed<'_>, online: Option<bool>, limit: u32) -> Vec<Device> {
    let mut out = Vec::new();
    let mut cursor = None;
    for _ in 0..50 {
        let page = a
            .devices(DeviceQuery {
                online,
                limit: Some(limit),
                cursor: cursor.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        for d in &page.items {
            if let Some(o) = online {
                assert_eq!(d.online, o, "{} doesn't match online={o}", d.name);
            }
        }
        let n = page.items.len();
        out.extend(page.items);
        match page.next_cursor {
            Some(c) => {
                assert_eq!(n, limit as usize, "a page with a next_cursor must be full");
                cursor = Some(c);
            }
            None => return out,
        }
    }
    panic!("paging never ended");
}

/// The online filter is applied before paging: pages are full while more matching devices exist.
#[tokio::test]
async fn device_list_online_filter_pages_fully() {
    let env = start().await;
    let alice = login(&env.client, "c:alice").await;
    let a = env.client.authed(&alice, Some("acme"));
    let mut online = Vec::new();
    for i in 0..2 {
        online.push(FakeDevice::pair(&env.client, "c:alice", DeviceOs::Linux, &format!("online {i}"), &[]).await);
    }
    let mut offline = Vec::new();
    for i in 0..7 {
        offline.push(pair_offline(&env.client, &alice, DeviceOs::Linux, &format!("offline {i}")).await);
    }

    let on = all_pages(a, Some(true), 1).await;
    let mut got: Vec<String> = on.iter().map(|d| d.device_id.to_string()).collect();
    let mut want: Vec<String> = online.iter().map(|d| d.id.clone()).collect();
    got.sort();
    want.sort();
    assert_eq!(got, want);

    let off = all_pages(a, Some(false), 2).await;
    let mut got: Vec<String> = off.iter().map(|d| d.device_id.to_string()).collect();
    got.sort();
    offline.sort();
    assert_eq!(got, offline);

    // An unknown OS says which ones exist.
    let e = api_err(
        a.devices(DeviceQuery {
            os: Some("toaster".into()),
            ..Default::default()
        })
        .await,
    );
    assert_eq!(e.code, ErrorCode::InvalidInput);
    assert!(e.hint.unwrap_or_default().contains("android_tv"));

    // Unfiltered paging is unchanged: every device once.
    let every = all_pages(a, None, 4).await;
    assert_eq!(every.len(), 9);

    // A Silicon's list pages the same way.
    let chef = login(&env.client, "si:chef").await;
    for id in online
        .iter()
        .map(|d| d.id.clone())
        .chain(offline.iter().take(3).cloned())
    {
        a.grant(&id, "si:chef").await.unwrap();
    }
    let c = env.client.authed(&chef, Some("acme"));
    assert_eq!(all_pages(c, Some(true), 1).await.len(), 2);
    assert_eq!(all_pages(c, Some(false), 1).await.len(), 3);
    drop(online);
}
