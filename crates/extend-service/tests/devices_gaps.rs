//! Service tests for device reads after removal, hosted devices end to end, and device-list
//! paging with the online filter.
//!
//! Real PostgreSQL, the real HTTP and WebSocket stack, the local Silicon Accounts stand-in, and
//! scripted fake devices speaking docs/device-protocol.md. Needs a PostgreSQL the tests can create
//! databases on: `EXTEND_TEST_ADMIN_URL` (default `postgres://extend:extend@127.0.0.1:5440/postgres`).
//!
//! (The test-environment device limit tests went with test environments in 4.0.)

mod common;
#[path = "common/readiness.rs"]
mod readiness;

use std::time::Duration;

use common::{Env, login, start};
use extend_protocol::frames::{AttachedStatus, CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ServiceFrame};
use extend_protocol::model::*;
use extend_protocol::{Capability, DeviceOs, ErrorCode};
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::{ActivityQuery, DeviceQuery, ListQuery};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
async fn pair_offline(env: &Env, token: &str, os: DeviceOs, name: &str) -> String {
    let e = env.client.enroll(&enrollment(os)).await.unwrap();
    env.v2(token)
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
    /// Enrolls, lets the Carbon signed in as `carbon` pair it (with access for `silicons`),
    /// connects and says hello.
    async fn pair(env: &Env, carbon: &str, os: DeviceOs, name: &str, silicons: &[&str]) -> FakeDevice {
        let client = &env.client;
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
        env.v2(carbon)
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
        readiness::ready(&env.base, carbon, &d.id).await;
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
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let a = env.v2(&alice);
    let b = env.v2(&bob);
    let c = env.v2(&chef);

    let phone = FakeDevice::pair(&env, &alice, DeviceOs::Android, "Alice's Pixel", &["si:chef"]).await;
    let id = phone.id.clone();
    let frames = phone.serve();
    let kept = pair_offline(&env, &alice, DeviceOs::Linux, "Alice's desk").await;

    // A session with a command, then removal while the session is still running.
    let sid = c
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    assert!(c.run(&sid, &cmd("snapshot", &["-i"])).await.unwrap().ok);
    a.remove_device(&id, None).await.unwrap();
    assert_eq!(api_err(c.session(&sid).await).code, ErrorCode::SessionNotFound);
    let ended = a.session(&sid).await.unwrap();
    assert_eq!(ended.state, SessionState::Ended);
    assert_eq!(ended.end_reason, Some(EndReason::DeviceRemoved));
    // Removing a device unpairs it: the app is told, and its connection ends.
    let seen = tokio::time::timeout(Duration::from_secs(10), frames)
        .await
        .expect("the app is told")
        .unwrap();
    assert!(
        matches!(
            seen.last(),
            Some(ServiceFrame::Unpaired {
                reason: EndReason::DeviceRemoved
            })
        ),
        "{:?}",
        seen.last()
    );

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
            scope: Some("accessible".into()),
            ..Default::default()
        })
        .await,
    );
    assert_eq!(e.code, ErrorCode::InvalidInput);
    let e = api_err(c.devices_including_removed(DeviceQuery::default()).await);
    assert_eq!(e.code, ErrorCode::InvalidInput);
    let raw = reqwest::Client::new()
        .get(format!("{}/api/v2/devices?include_removed=maybe", env.base))
        .bearer_auth(&alice)
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

/// Attach → the host hears about it → setup code relayed → the host reports the device → a session
/// on it reaches the host with `target` → the host's Stop for that target ends only that session.
#[tokio::test]
async fn hosted_device_end_to_end() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let a = env.v2(&alice);
    let c = env.v2(&chef);
    let s = env.v2(&sous);

    let mut host = FakeDevice::pair(&env, &alice, DeviceOs::Macos, "Studio Mac", &["si:sous"]).await;
    let host_id = host.id.clone();
    let linux = pair_offline(&env, &alice, DeviceOs::Linux, "Linux box").await;

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
    let seen = readiness::ready(&env.base, &chef, &atv_id).await;
    assert!(seen.online);
    assert_eq!(seen.state, DeviceState::Ready);
    assert_eq!(seen.os_version.as_deref(), Some("18.2"));
    assert!(
        seen.host_device_id.is_none(),
        "a Silicon must not discover an ungranted host"
    );
    assert_eq!(
        a.device(&atv_id)
            .await
            .unwrap()
            .host_device_id
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
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

    // A carried rename reaches the host immediately with the carried device's metadata. Refresh
    // alone only makes an app re-read its own pair, leaving the carried name stale until reconnect.
    let renamed = a
        .update_device(
            &atv_id,
            None,
            &DevicePatch {
                name: Some("Renamed TV".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "Renamed TV");
    let updated = host
        .expect(
            "renamed attachment",
            |f| matches!(f, ServiceFrame::Attach { device_id, .. } if *device_id == atv.device_id),
        )
        .await;
    assert_eq!(
        updated,
        ServiceFrame::Attach {
            device_id: atv.device_id.clone(),
            os: DeviceOs::Tvos,
            name: "Renamed TV".into(),
            address: Some("192.168.1.40".into()),
            removed: false,
            in_use_indicator: InUseIndicator::Shown,
        }
    );
    assert_eq!(c.session(tv_sid.as_str()).await.unwrap().state, SessionState::Active);
    assert_eq!(s.session(mac_sid.as_str()).await.unwrap().state, SessionState::Active);

    // A command on the TV travels to the host with target set, and the host's answer comes back.
    let tv_sid_s = tv_sid.to_string();
    let run = tokio::spawn({
        let base = env.base.clone();
        let chef = chef.clone();
        let sid = tv_sid_s.clone();
        async move {
            common::v2::V2::new(&base, &chef)
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

    // Removing the carried device unpairs it alone: the host and its session stay.
    a.remove_device(&atv_id, None).await.unwrap();
    assert!(a.device(&atv_id).await.unwrap().removed_at.is_some());
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
    assert_eq!(
        a.session(mac_sid.as_str()).await.unwrap().end_reason,
        Some(EndReason::DeviceRemoved)
    );
}

/// Collects every page of a device list, checking that each page but the last is full.
async fn all_pages(a: &common::v2::V2<'_>, online: Option<bool>, limit: u32) -> Vec<Device> {
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
    let alice = login(&env, "c:alice").await;
    let a = &env.v2(&alice);
    let mut online = Vec::new();
    for i in 0..2 {
        online.push(FakeDevice::pair(&env, &alice, DeviceOs::Linux, &format!("online {i}"), &[]).await);
    }
    let mut offline = Vec::new();
    for i in 0..7 {
        offline.push(pair_offline(&env, &alice, DeviceOs::Linux, &format!("offline {i}")).await);
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
    let chef = login(&env, "si:chef").await;
    for id in online
        .iter()
        .map(|d| d.id.clone())
        .chain(offline.iter().take(3).cloned())
    {
        a.grant(&id, "si:chef").await.unwrap();
    }
    let c = &env.v2(&chef);
    assert_eq!(all_pages(c, Some(true), 1).await.len(), 2);
    assert_eq!(all_pages(c, Some(false), 1).await.len(), 3);
    drop(online);
}
