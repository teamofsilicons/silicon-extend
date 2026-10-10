//! End-to-end tests of the service: a real PostgreSQL, the real HTTP and WebSocket stack, the
//! local Silicon Accounts stand-in, and a scripted fake device speaking docs/device-protocol.md.
//!
//! Needs a PostgreSQL the tests can create databases on:
//! `EXTEND_TEST_ADMIN_URL` (default `postgres://extend:extend@127.0.0.1:5440/postgres`).

mod common;
#[path = "common/readiness.rs"]
mod readiness;

use std::time::Duration;

use common::{Env, deliver, login, start, uuid};
use extend_protocol::frames::{CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ProducedFile, ServiceFrame};
use extend_protocol::model::*;
use extend_protocol::{Capability, DeviceOs, ErrorCode};
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::{Client, DeviceQuery, ListQuery};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
    /// Enrolls, lets the Carbon signed in as `carbon` pair it (with access for `silicons`),
    /// connects and says hello.
    async fn pair(env: &Env, carbon: &str, os: DeviceOs, silicons: &[&str]) -> Device {
        let client = env.client.clone();
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
        let claim = PairingClaim {
            pairing_code: pairing_code.to_lowercase(),
            name: format!("{} device", os.as_str()),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: silicons.iter().map(|s| (*s).to_owned()).collect(),
        };
        env.v2(carbon).pair(&claim).await.unwrap();
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
        d.hello(os, env, carbon).await;
        d
    }

    async fn hello(&mut self, os: DeviceOs, env: &Env, owner: &str) {
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
        readiness::ready(&env.base, owner, &self.id).await;
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
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let a = env.v2(&alice);
    let c = env.v2(&chef);
    let s = env.v2(&sous);

    let device = Device::pair(&env, &alice, DeviceOs::Android, &["si:chef", "si:sous"]).await;
    let id = device.id.clone();
    let credential = device.credential.clone();
    let seen = device.serve(env.base.clone());

    // Silicon sees it, online, with the commands Android allows.
    let list = c.devices(DeviceQuery::default()).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert!(list.items[0].online);
    let d = c.device(&id).await.unwrap();
    assert!(d.commands.as_ref().unwrap().contains(&"snapshot".to_owned()));
    assert!(!d.commands.as_ref().unwrap().contains(&"terminal".to_owned()));
    // A Silicon without access can't see it.
    let rover = login(&env, "si:rover").await;
    assert_eq!(code_of(env.v2(&rover).device(&id).await), ErrorCode::DeviceNotFound);

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
            .contains(&format!("extend request send {id}"))
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
    assert_eq!(code_of(s.session(&s2).await), ErrorCode::SessionNotFound);
    let ended = a.session(&s2).await.unwrap();
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

    // The Silicon signs out everywhere (Silicon Accounts tells Extend) → its session ends.
    let s4 = c
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let status = deliver(
        &env,
        "membership.signed_out",
        serde_json::json!({"uuid": uuid("si:chef"), "membership_id": "m1", "reason": "session_revoked"}),
    )
    .await;
    assert_eq!(status, 204);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let chef2 = login(&env, "si:chef").await;
    let ended = env.v2(&chef2).session(&s4).await.unwrap();
    assert_eq!(ended.state, SessionState::Ended);
    assert!(matches!(
        ended.end_reason,
        Some(EndReason::SiliconLoggedOut | EndReason::AccessRemoved)
    ));

    // Removing the device unpairs it: the app is told, and its credential stops working.
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
    assert!(
        matches!(
            frames.last(),
            Some(ServiceFrame::Unpaired {
                reason: EndReason::DeviceRemoved
            })
        ),
        "{:?}",
        frames.last()
    );
    // The credential no longer works.
    assert!(env.client.device_self(&credential).await.is_err());
}

#[tokio::test]
async fn pairing_rules_and_device_management() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let a = env.v2(&alice);
    let b = env.v2(&bob);
    let c = env.v2(&chef);

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

    let mut d = Device::pair(&env, &bob, DeviceOs::Macos, &[]).await;
    let id = d.id.clone();

    // Devices are private to the Carbon who paired them: another Carbon sees nothing of it, and
    // there is no shared visibility.
    assert!(a.devices(DeviceQuery::default()).await.unwrap().items.is_empty());
    assert_eq!(code_of(a.stop_device(&id).await), ErrorCode::DeviceNotFound);
    assert_eq!(code_of(a.device(&id).await), ErrorCode::DeviceNotFound);
    assert_eq!(
        code_of(
            b.update_device(
                &id,
                None,
                &DevicePatch {
                    visibility: Some(Visibility::Team),
                    ..Default::default()
                },
            )
            .await
        ),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        code_of(
            a.devices(DeviceQuery {
                scope: Some("team".into()),
                ..Default::default()
            })
            .await
        ),
        ErrorCode::InvalidInput
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

    // Access: any Silicon by its current id; never a Carbon, never an id nobody has.
    assert_eq!(code_of(b.grant(&id, "si:nobody").await), ErrorCode::InvalidInput);
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
    d2.hello(DeviceOs::Macos, &env, &bob).await;

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
    let d3 = Device::pair(&env, &bob, DeviceOs::Linux, &["si:chef"]).await;
    env.client.revoke_pair(&d3.credential).await.unwrap();
    // Its owner can still read it, marked removed (tests/devices_gaps.rs covers the rest).
    assert_eq!(
        b.device(&d3.id).await.unwrap().removed_reason,
        Some(EndReason::PairRevoked)
    );

    // Session ids grow once all 3-character ids are used.
    let d4 = Device::pair(&env, &bob, DeviceOs::Linux, &["si:chef"]).await;
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

#[tokio::test]
async fn test_environments_are_gone() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    // A request that still selects a test environment is refused, never run in production.
    let r = reqwest::Client::new()
        .get(format!("{}/api/v2/devices", env.base))
        .bearer_auth(&alice)
        .header(
            extend_protocol::TESTING_SECRET_HEADER,
            extend_protocol::ids::new_secret("ask_"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["data"]["code"], "testing_secret_invalid");
    assert!(
        v["data"]["message"]
            .as_str()
            .unwrap()
            .contains("no longer has test environments"),
        "{v}"
    );
    // Nor does a device app, through the device wire.
    let e = reqwest::Client::new()
        .post(format!("{}/api/v1/enrollments", env.base))
        .header(
            extend_protocol::TESTING_SECRET_HEADER,
            extend_protocol::ids::new_secret("ask_"),
        )
        .json(&serde_json::json!({"type": "enrollment", "data": {"os": "linux", "app_version": "1.0.0"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(e.status(), 401);
    // The operations Honeycomb drove are gone.
    let gone = reqwest::Client::new()
        .put(format!(
            "{}/internal/honeycomb/organizations/acme/testing-environments/{}/operations/{}",
            env.base,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ))
        .bearer_auth("hck_test")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 404);
}

#[tokio::test]
async fn versioning_and_errors() {
    let env = start().await;
    let r = reqwest::Client::new()
        .get(format!("{}/api/version", env.base))
        .header("Silicon-Extend-Supported-API-Versions", "3, 4")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["data"]["code"], "api_version_unsupported");
    // A client that speaks 2 gets 2.
    let r = reqwest::Client::new()
        .get(format!("{}/api/version", env.base))
        .header("Silicon-Extend-Supported-API-Versions", "1, 2")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["data"]["api_version"], 2);
    // The account routes of API v1 are retired, with the update command.
    let r = reqwest::Client::new()
        .get(format!("{}/api/v1/iam", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 410);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(
        (v["data"]["code"].as_str(), v["data"]["hint"].as_str()),
        (Some("api_version_sunset"), Some("silicon-apps update extend"))
    );
    // Where to sign in.
    let r = reqwest::Client::new()
        .get(format!("{}/api/v2/accounts", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["data"]["app_id"], "extend");
    // Every error carries a request id and a docs link.
    let r = reqwest::Client::new()
        .get(format!("{}/api/v2/me", env.base))
        .bearer_auth("not-a-token")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let v: serde_json::Value = r.json().await.unwrap();
    assert!(!v["data"]["request_id"].as_str().unwrap().is_empty());
    assert!(v["data"]["docs_url"].as_str().unwrap().contains("unauthorized"), "{v}");
    // Reports.
    let t = login(&env, "si:chef").await;
    let rep = env
        .v2(&t)
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
