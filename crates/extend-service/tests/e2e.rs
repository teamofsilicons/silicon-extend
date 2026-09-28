//! End-to-end tests of the service: a real PostgreSQL, the real HTTP and WebSocket stack, the
//! official client crate, and a scripted fake device speaking docs/device-protocol.md.
//!
//! Needs a PostgreSQL the tests can create databases on:
//! `EXTEND_TEST_ADMIN_URL` (default `postgres://extend:extend@127.0.0.1:5440/postgres`).

#[path = "common/readiness.rs"]
mod readiness;

use std::net::SocketAddr;
use std::time::Duration;

use extend_protocol::frames::{CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ProducedFile, ServiceFrame};
use extend_protocol::model::*;
use extend_protocol::{Capability, DeviceOs, ErrorCode};
use extend_service::config::{Config, Environment, FilesMode, IamMode, TingMode};
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::{Client, DeviceQuery, ListQuery};
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
            ("si:stranger".into(), vec!["labs".into()]),
        ],
        web_dir: None,
        trusted_proxies: vec![],
        tuning: Default::default(),
    };
    let state = extend_service::build(cfg).await.unwrap();
    let pool = state.pool.clone();
    tokio::spawn(extend_service::serve_on(listener, state));
    let client = Client::connect(&base).await.unwrap();
    Env { base, pool, client }
}

async fn login(c: &Client, who: &str) -> String {
    c.login(who).await.unwrap().access_token
}

/// A scripted Extend app.
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

impl Device {
    /// Enrolls, lets `carbon` pair it (with access for `silicons`), connects and says hello.
    async fn pair(env: &Env, carbon: &str, os: DeviceOs, silicons: &[&str], secret: Option<&str>) -> Device {
        let client = match secret {
            Some(s) => Client::builder(&env.base).testing_secret(s).connect().await.unwrap(),
            None => env.client.clone(),
        };
        let e = client
            .enroll(&EnrollmentCreate {
                os,
                os_version: Some("1".into()),
                model: Some("Fake".into()),
                app_version: "1.0.0".into(),
                engine_version: None,
            })
            .await
            .unwrap();
        assert_eq!(e.pairing_code.len(), 6);
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
        let claim = PairingClaim {
            pairing_code: pairing_code.to_lowercase(),
            name: format!("{} device", os.as_str()),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: silicons.iter().map(|s| (*s).to_owned()).collect(),
        };
        client.authed(&token, Some("acme")).pair(&claim).await.unwrap();
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
                &client.ws_url("/api/v1/device/connect"),
                &format!("Extend-Device {credential}"),
            )
            .await,
            id,
            credential,
        };
        d.hello(os, &client.authed(&token, Some("acme"))).await;
        d
    }

    async fn hello(&mut self, os: DeviceOs, owner: &silicon_extend_client::Authed<'_>) {
        let caps: Vec<Capability> = os.full_capabilities().to_vec();
        let hello = DeviceFrame::Hello(Hello {
            app_version: "1.0.0".into(),
            os,
            os_version: Some("15".into()),
            model: Some("Fake".into()),
            engine_version: None,
            capabilities: caps,
            missing: vec![],
            setup: Setup::complete(),
            features: vec![],
        });
        self.send(&hello).await;
        readiness::ready(owner, &self.id).await;
    }

    async fn send(&mut self, f: &DeviceFrame) {
        self.ws
            .send(Message::Text(serde_json::to_string(f).unwrap().into()))
            .await
            .unwrap();
    }

    /// Next frame that isn't a ping (pings are answered).
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

    /// Answers commands forever in the background like a well-behaved device.
    fn serve(mut self, base: String) -> tokio::task::JoinHandle<Vec<ServiceFrame>> {
        tokio::spawn(async move {
            let mut seen = Vec::new();
            let client = Client::connect(&base).await.unwrap();
            loop {
                let next = tokio::time::timeout(Duration::from_secs(30), self.recv()).await;
                let Ok(f) = next else { break };
                if let ServiceFrame::Command(c) = &f {
                    let mut out = CommandOutcome {
                        id: c.id,
                        ok: true,
                        output: serde_json::json!({"echo": c.args}),
                        text: Some(format!("ran {} {}", c.command, c.args.join(" "))),
                        error: None,
                        files: vec![],
                    };
                    if c.command == "screenshot" {
                        let bytes = b"\x89PNG fake".to_vec();
                        client
                            .upload_artifact(
                                &self.credential,
                                c.upload_ids[0],
                                "shot.png",
                                "image/png",
                                bytes.clone(),
                            )
                            .await
                            .unwrap();
                        out.files.push(ProducedFile {
                            upload_id: c.upload_ids[0],
                            name: "shot.png".into(),
                            content_type: "image/png".into(),
                            kind: FileKind::Screenshot,
                            size_bytes: bytes.len() as i64,
                        });
                    }
                    if c.command == "is" {
                        out.ok = false;
                        out.error = Some(CommandError {
                            code: "assertion_failed".into(),
                            message: "not visible".into(),
                            details: serde_json::Value::Null,
                        });
                    }
                    if c.command == "wait" {
                        tokio::time::sleep(Duration::from_millis(3000)).await;
                    }
                    self.send(&DeviceFrame::Result(out)).await;
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

fn code_of<T: std::fmt::Debug>(r: Result<T, silicon_extend_client::Error>) -> ErrorCode {
    r.expect_err("expected an error").code()
}

#[tokio::test]
async fn pairing_sessions_commands_and_files() {
    let env = start().await;
    let alice = login(&env.client, "c:alice").await;
    let chef = login(&env.client, "si:chef").await;
    let sous = login(&env.client, "si:sous").await;
    let a = env.client.authed(&alice, Some("acme"));
    let c = env.client.authed(&chef, Some("acme"));
    let s = env.client.authed(&sous, Some("acme"));

    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef", "si:sous"], None).await;
    let id = device.id.clone();
    let seen = device.serve(env.base.clone());

    // Silicon sees it, online, with the commands Android allows.
    let list = c.devices(DeviceQuery::default()).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert!(list.items[0].online);
    let d = c.device(&id).await.unwrap();
    assert!(d.commands.as_ref().unwrap().contains(&"snapshot".to_owned()));
    assert!(!d.commands.as_ref().unwrap().contains(&"terminal".to_owned()));
    // A Silicon outside the team can't see it.
    let stranger = login(&env.client, "si:stranger").await;
    assert_eq!(
        code_of(env.client.authed(&stranger, Some("acme")).device(&id).await),
        ErrorCode::NotATeamMember
    );

    // Session: 3 hexadecimal characters.
    let sess = c.start_session(&id.parse().unwrap()).await.unwrap();
    let sid = sess.session_id.to_string();
    assert_eq!(sid.len(), 3);
    assert!(sid.bytes().all(|b| b.is_ascii_hexdigit()));

    // One Silicon at a time; the other gets the holder and the request command.
    let busy = s.start_session(&id.parse().unwrap()).await.unwrap_err();
    assert_eq!(busy.code(), ErrorCode::DeviceInUse);
    assert!(
        busy.api()
            .unwrap()
            .hint
            .as_ref()
            .unwrap()
            .contains(&format!("extend --team acme request send {id}"))
    );
    let r = s.send_request(&id, "Need it for an OTP, 2 minutes").await.unwrap();
    assert_eq!(r.to, "si:chef");
    assert_eq!(r.delivery, Delivery::Delivered);
    let long = "x".repeat(301);
    assert_eq!(code_of(s.send_request(&id, &long).await), ErrorCode::InvalidInput);

    // Commands relay; typed text is redacted in the log.
    let cmd = |name: &str, args: &[&str]| CommandRequest {
        command: name.into(),
        args: args.iter().map(|x| (*x).to_owned()).collect(),
        timeout_ms: None,
        self_destruct_minutes: None,
        permanent: false,
        attachments: vec![],
    };
    let res = c.run(&sid, &cmd("snapshot", &["-i"])).await.unwrap();
    assert!(res.ok);
    assert_eq!(res.text.as_deref(), Some("ran snapshot -i"));
    c.run(&sid, &cmd("fill", &["@e3", "hunter2"])).await.unwrap();
    let failed = c.run(&sid, &cmd("is", &["visible", "label=\"x\""])).await.unwrap();
    assert!(!failed.ok);
    assert_eq!(failed.error.unwrap().code, "assertion_failed");

    // Screenshot → uploaded → stored → downloadable, with a 1-day self-destruct.
    let shot = c.run(&sid, &cmd("screenshot", &[])).await.unwrap();
    assert_eq!(shot.files.len(), 1);
    let f = &shot.files[0];
    let left = f.self_destruct_at.unwrap() - time::OffsetDateTime::now_utc();
    assert!(left > time::Duration::hours(23) && left <= time::Duration::hours(24));
    let bytes = env.client.download(&f.url, &chef).await.unwrap();
    assert_eq!(bytes, b"\x89PNG fake");
    let kept = c.keep_file(&f.file_id.to_string()).await.unwrap();
    assert!(kept.permanent && kept.self_destruct_at.is_none());
    // The Carbon who owns the device sees the file too.
    assert_eq!(a.files(ListQuery::default()).await.unwrap().items.len(), 1);

    // Refusals with precise codes.
    assert_eq!(
        code_of(c.run(&sid, &cmd("terminal", &["run", "ls"])).await),
        ErrorCode::UnsupportedOnDevice
    );
    assert_eq!(code_of(c.run(&sid, &cmd("boot", &[])).await), ErrorCode::UnknownCommand);
    assert_eq!(
        code_of(c.run(&sid, &cmd("click", &["@e1", "--platform", "ios"])).await),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        code_of(s.run(&sid, &cmd("snapshot", &[])).await),
        ErrorCode::SessionNotFound
    );

    // A command that outlives its deadline.
    let mut slow = cmd("wait", &["2500"]);
    slow.timeout_ms = Some(1000);
    assert_eq!(code_of(c.run(&sid, &slow).await), ErrorCode::CommandTimeout);

    // Takeover pauses the session.
    c.takeover(&sid, "Please approve the payment").await.unwrap();
    assert_eq!(
        code_of(c.run(&sid, &cmd("snapshot", &[])).await),
        ErrorCode::SessionPaused
    );
    c.release_takeover(&sid).await.unwrap();
    assert!(c.run(&sid, &cmd("snapshot", &[])).await.unwrap().ok);

    // Activity log: redacted text, commands, the request.
    let act = a.activity(&id, Default::default()).await.unwrap();
    let fill = act.items.iter().find(|e| e.command.as_deref() == Some("fill")).unwrap();
    assert_eq!(
        fill.args.as_ref().unwrap(),
        &vec!["@e3".to_owned(), "[redacted 7 chars]".to_owned()]
    );
    assert!(act.items.iter().any(|e| e.action == "request_sent"));
    assert!(act.items.iter().any(|e| e.action == "takeover_started"));

    // The Carbon stops it.
    let stopped = a.stop_device(&id).await.unwrap();
    assert_eq!(stopped.end_reason, Some(EndReason::StoppedByCarbon));
    assert_eq!(
        code_of(c.run(&sid, &cmd("snapshot", &[])).await),
        ErrorCode::SessionEnded
    );

    // Now sous can use it; revoking access mid-session ends it at once.
    let s2 = s
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    a.revoke(&id, "si:sous").await.unwrap();
    let ended = s.session(&s2).await.unwrap();
    assert_eq!(ended.end_reason, Some(EndReason::AccessRemoved));

    // Idle timeout (the scheduler honours idle_ends_at).
    let s3 = c
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    sqlx::query("UPDATE extend.sessions SET idle_ends_at = now() - interval '1 second' WHERE session_id = $1")
        .bind(&s3)
        .execute(&env.pool)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(c.session(&s3).await.unwrap().end_reason, Some(EndReason::IdleTimeout));

    // Silicon logs out (IAM revocation) → its session ends.
    let s4 = c
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    reqwest::Client::new()
        .post(format!("{}/dev/iam/members", env.base))
        .json(&serde_json::json!({"type":"member","data":{"id":"si:chef","teams":["acme"],"revoke":true}}))
        .send()
        .await
        .unwrap();
    let chef2 = login(&env.client, "si:chef").await;
    let ended = env.client.authed(&chef2, Some("acme")).session(&s4).await.unwrap();
    assert_eq!(ended.state, SessionState::Ended);
    assert!(matches!(
        ended.end_reason,
        Some(EndReason::SiliconLoggedOut | EndReason::AccessRemoved)
    ));

    // The device saw sessions start and end.
    a.remove_device(&id, None).await.unwrap();
    let frames = seen.await.unwrap();
    assert!(frames.iter().any(|f| matches!(f, ServiceFrame::SessionStarted { .. })));
    assert!(frames.iter().any(|f| matches!(
        f,
        ServiceFrame::SessionEnded {
            reason: EndReason::StoppedByCarbon,
            ..
        }
    )));
    assert!(frames.iter().any(|f| matches!(f, ServiceFrame::Takeover { .. })));
    assert!(matches!(
        frames.last(),
        Some(ServiceFrame::Unpaired {
            reason: EndReason::DeviceRemoved
        })
    ));
    // The credential no longer works.
    assert!(env.client.device_self("edc_").await.is_err());
}

#[tokio::test]
async fn pairing_rules_and_device_management() {
    let env = start().await;
    let alice = login(&env.client, "c:alice").await;
    let bob = login(&env.client, "c:bob").await;
    let chef = login(&env.client, "si:chef").await;
    let a = env.client.authed(&alice, Some("acme"));
    let b = env.client.authed(&bob, Some("acme"));
    let c = env.client.authed(&chef, Some("acme"));

    // Silicons can't pair; wrong codes are rate limited after 5.
    let claim = |code: &str| PairingClaim {
        pairing_code: code.into(),
        name: "x".into(),
        visibility: None,
        pair_ttl_days: None,
        silicon_ids: vec![],
    };
    assert_eq!(code_of(c.pair(&claim("000000")).await), ErrorCode::CarbonOnly);
    for _ in 0..5 {
        assert_eq!(code_of(a.pair(&claim("000000")).await), ErrorCode::PairingCodeInvalid);
    }
    assert_eq!(code_of(a.pair(&claim("000000")).await), ErrorCode::RateLimited);
    assert_eq!(code_of(b.pair(&claim("zzz")).await), ErrorCode::InvalidInput);

    let mut d = Device::pair(&env, "c:bob", DeviceOs::Macos, &[], None).await;
    let id = d.id.clone();

    // 1.1: a device belongs to the Carbon who paired it, and nobody else sees it, whatever the
    // Team. The 1.0 website's "Team devices" list is empty, and visibility changes nothing.
    let team = a
        .devices(DeviceQuery {
            scope: Some("team".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(team.items.is_empty());
    assert_eq!(code_of(a.stop_device(&id).await), ErrorCode::DeviceNotFound);
    assert_eq!(code_of(a.device(&id).await), ErrorCode::DeviceNotFound);
    let still = b
        .update_device(
            &id,
            None,
            &DevicePatch {
                visibility: Some(Visibility::Team),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(still.visibility, Visibility::Personal);
    assert!(
        a.devices(DeviceQuery {
            scope: Some("team".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .items
        .is_empty()
    );

    // Rename, TTL bounds, stale version.
    let v = b.device(&id).await.unwrap().version.unwrap();
    let renamed = b
        .update_device(
            &id,
            Some(v),
            &DevicePatch {
                name: Some("Studio Mac".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "Studio Mac");
    assert_eq!(
        code_of(
            b.update_device(
                &id,
                Some(v),
                &DevicePatch {
                    pair_ttl_days: Some(20),
                    ..Default::default()
                }
            )
            .await
        ),
        ErrorCode::VersionConflict
    );
    assert_eq!(
        code_of(
            b.update_device(
                &id,
                None,
                &DevicePatch {
                    pair_ttl_days: Some(31),
                    ..Default::default()
                }
            )
            .await
        ),
        ErrorCode::InvalidInput
    );
    let t = b
        .update_device(
            &id,
            None,
            &DevicePatch {
                pair_ttl_days: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(t.pair_ttl_days, Some(1));
    // The device was told to refresh.
    assert!(matches!(d.recv().await, ServiceFrame::Refresh));

    // Access: only team Silicons.
    assert_eq!(code_of(b.grant(&id, "si:stranger").await), ErrorCode::InvalidInput);
    assert_eq!(code_of(b.grant(&id, "c:alice").await), ErrorCode::InvalidInput);
    b.grant(&id, "si:chef").await.unwrap();
    assert_eq!(b.access(&id).await.unwrap().len(), 1);

    // A new connection supersedes the old one.
    let mut d2 = Device {
        ws: ws_connect(
            &env.client.ws_url("/api/v1/device/connect"),
            &format!("Extend-Device {}", d.credential),
        )
        .await,
        id: id.clone(),
        credential: d.credential.clone(),
    };
    loop {
        match d.recv().await {
            ServiceFrame::Refresh => continue,
            f => {
                assert!(matches!(f, ServiceFrame::Superseded), "{f:?}");
                break;
            }
        }
    }
    d2.hello(DeviceOs::Macos, &b).await;

    // Pair expiry after inactivity.
    sqlx::query("UPDATE extend.devices SET last_activity_at = now() - interval '2 days' WHERE device_id = $1")
        .bind(&id)
        .execute(&env.pool)
        .await
        .unwrap();
    let f = loop {
        let f = tokio::time::timeout(Duration::from_secs(40), d2.recv())
            .await
            .expect("unpaired in time");
        if !matches!(f, ServiceFrame::Refresh) {
            break f;
        }
    };
    assert!(matches!(
        f,
        ServiceFrame::Unpaired {
            reason: EndReason::PairExpired
        }
    ));
    assert!(env.client.device_self(&d.credential).await.is_err());

    // Device-side revoke pair.
    let d3 = Device::pair(&env, "c:bob", DeviceOs::Linux, &["si:chef"], None).await;
    env.client.revoke_pair(&d3.credential).await.unwrap();
    // Its owner can still read it, marked removed (tests/devices_gaps.rs covers the rest).
    assert_eq!(
        b.device(&d3.id).await.unwrap().removed_reason,
        Some(EndReason::PairRevoked)
    );

    // Session ids grow once all 3-character ids are used.
    let d4 = Device::pair(&env, "c:bob", DeviceOs::Linux, &["si:chef"], None).await;
    let _serve = d4.serve(env.base.clone());
    sqlx::query("INSERT INTO extend.session_ids SELECT lpad(to_hex(g), 3, '0') FROM generate_series(0, 4095) g ON CONFLICT DO NOTHING").execute(&env.pool).await.unwrap();
    let dev4: String = sqlx::query_scalar(
        "SELECT device_id FROM extend.devices WHERE removed_at IS NULL ORDER BY paired_at DESC LIMIT 1",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    let s = c.start_session(&dev4.parse().unwrap()).await.unwrap();
    assert_eq!(s.session_id.as_str().len(), 4);
}

async fn honeycomb(env: &Env, envid: Uuid, action: &str, revision: i64, generation: i64) -> reqwest::Response {
    let op = Uuid::new_v4();
    reqwest::Client::new()
        .put(format!(
            "{}/internal/honeycomb/organizations/acme/testing-environments/{envid}/operations/{op}",
            env.base
        ))
        .bearer_auth("hck_test")
        .json(&serde_json::json!({
            "operation_id": op, "environment_id": envid, "org_id": "acme", "app_id": "extend",
            "environment_revision": revision, "generation": generation, "key_version": 1, "action": action,
            "testing_key": "abcdefghijklmnopqrstuvwxyz012345", "name": "checkout-e2e"
        }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn test_environments_are_isolated() {
    let env = start().await;
    let envid = Uuid::new_v4();
    let secret = extend_protocol::ids::new_secret("ask_");
    // Wrong service credential.
    let bad = reqwest::Client::new()
        .put(format!(
            "{}/internal/honeycomb/organizations/acme/testing-environments/{envid}/operations/{}",
            env.base,
            Uuid::new_v4()
        ))
        .bearer_auth("nope")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    let r = honeycomb(&env, envid, "prepare", 1, 1).await;
    assert_eq!(r.status(), 200);
    let receipt: serde_json::Value = r.json().await.unwrap();
    assert_eq!(receipt["state"], "completed");
    assert!(receipt.get("testing_key").is_none());
    reqwest::Client::new()
        .post(format!("{}/dev/iam/test-apps", env.base))
        .json(&serde_json::json!({"type":"test_app","data":{"secret": secret, "environment_id": envid}}))
        .send()
        .await
        .unwrap();

    // An unknown secret never falls back to production.
    let wrong = Client::builder(&env.base)
        .testing_secret(extend_protocol::ids::new_secret("ask_"))
        .connect()
        .await
        .unwrap();
    assert_eq!(code_of(wrong.login("c:alice").await), ErrorCode::TestingSecretInvalid);

    let t = Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    let te = t.testing_environment().await.unwrap();
    assert_eq!(te.name, "checkout-e2e");
    // Member-id login works in testing only.
    let alice_t = t.login("c:alice").await.unwrap();
    assert_eq!(alice_t.testing_environment.as_ref().unwrap().environment_id, envid);
    // Production and test logins don't cross.
    let alice_p = login(&env.client, "c:alice").await;
    assert_eq!(
        code_of(t.authed(&alice_p, Some("acme")).me().await),
        ErrorCode::TokenExpired
    );

    // Pair five devices, the sixth is refused with the exact message.
    let mut devices = Vec::new();
    for _ in 0..5 {
        devices.push(Device::pair(&env, "c:alice", DeviceOs::Linux, &[], Some(&secret)).await);
    }
    let e = t
        .enroll(&EnrollmentCreate {
            os: DeviceOs::Linux,
            os_version: None,
            model: None,
            app_version: "1.0.0".into(),
            engine_version: None,
        })
        .await
        .unwrap();
    let err = t
        .authed(&alice_t.access_token, Some("acme"))
        .pair(&PairingClaim {
            pairing_code: e.pairing_code,
            name: "six".into(),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: vec![],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::TestDeviceLimit);
    assert_eq!(err.api().unwrap().message, extend_protocol::TEST_DEVICE_LIMIT_MESSAGE);

    // Production sees none of it.
    let prod = env
        .client
        .authed(&alice_p, Some("acme"))
        .devices(DeviceQuery::default())
        .await
        .unwrap();
    assert!(prod.items.is_empty());
    assert_eq!(
        t.authed(&alice_t.access_token, Some("acme"))
            .devices(DeviceQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        5
    );

    // Clean: every device is unpaired and data is gone; the environment stays.
    let r = honeycomb(&env, envid, "clean", 2, 2).await;
    assert_eq!(r.status(), 200);
    let mut first = devices.remove(0);
    let f = first.recv().await;
    assert!(
        matches!(
            f,
            ServiceFrame::Unpaired {
                reason: EndReason::EnvironmentCleaned
            }
        ),
        "{f:?}"
    );
    let alice_t2 = t.login("c:alice").await.unwrap();
    assert!(
        t.authed(&alice_t2.access_token, Some("acme"))
            .devices(DeviceQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // Stale instructions are refused; replays return the stored receipt.
    assert_eq!(honeycomb(&env, envid, "clean", 1, 1).await.status(), 409);

    // Disable blocks access immediately.
    assert_eq!(honeycomb(&env, envid, "disable", 3, 2).await.status(), 200);
    assert_eq!(code_of(t.login("c:alice").await), ErrorCode::TestingSecretInvalid);
    assert_eq!(honeycomb(&env, envid, "restore", 4, 2).await.status(), 200);
    assert!(t.login("c:alice").await.is_ok());
    assert_eq!(honeycomb(&env, envid, "purge", 5, 2).await.status(), 200);
    assert_eq!(code_of(t.login("c:alice").await), ErrorCode::TestingSecretInvalid);
}

#[tokio::test]
async fn versioning_and_errors() {
    let env = start().await;
    let r = reqwest::Client::new()
        .get(format!("{}/api/version", env.base))
        .header("Silicon-Extend-Supported-API-Versions", "2, 3")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["data"]["code"], "api_version_unsupported");
    let r = reqwest::Client::new()
        .get(format!("{}/api/v1/iam", env.base))
        .header("Silicon-Extend-API-Version", "2")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let iam = env.client.iam().await.unwrap();
    assert_eq!(iam.app_id, "extend");
    // Every error carries a request id and a docs link.
    let e = env.client.login("nobody").await.unwrap_err();
    let api = e.api().unwrap();
    assert!(!api.request_id.is_empty());
    assert!(api.docs_url.as_ref().unwrap().contains("slt_invalid"));
    // Reports.
    let t = login(&env.client, "si:chef").await;
    let rep = env
        .client
        .authed(&t, Some("acme"))
        .report(&ReportInput {
            message: "snapshot misses a button".into(),
            pr: Some("https://github.com/teamofsilicons/silicon-extend/pull/1".into()),
            client_version: "test".into(),
            context: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert_eq!(rep.notification, "simulated");
}
