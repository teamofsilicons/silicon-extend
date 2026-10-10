//! Service behaviour closed in round 2 (service core): a command in flight holds the idle timer
//! and hears at once when its session ends (device removed, pair revoked, access removed, Stop);
//! signing out ends sessions; Ting enrolment and request retries without a running session;
//! self-destruct that keeps a file's record until it is really gone; storage warnings in command
//! results; the file download route; request reasons.
//!
//! A real PostgreSQL the tests can create databases on (`EXTEND_TEST_ADMIN_URL`, default
//! `postgres://extend:extend@127.0.0.1:5440/postgres`), the real HTTP and WebSocket stack, the
//! local Silicon Accounts stand-in, and scripted fake devices.

mod common;
#[path = "common/readiness.rs"]
mod readiness;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use common::{Env, deliver, login, uuid};
use extend_protocol::frames::{
    CommandFrame, CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ProducedFile, ServiceFrame,
};
use extend_protocol::model::*;
use extend_protocol::{DeviceOs, ErrorCode};
use extend_service::accounts::Principal;
use extend_service::config::{Config, Tuning};
use extend_service::db::World;
use extend_service::error::{AppError, AppResult};
use extend_service::files::{DynFiles, FileStore, LocalFiles, NewFile, Stored};
use extend_service::scheduler::{self, Backoff};
use extend_service::state::Shared;
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::{Client, ListQuery};
use time::OffsetDateTime;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ───────────────────────────── Harness ─────────────────────────────

/// Builds the service's state; `files` may swap in another file store.
async fn build(cfg: Config, files: Option<DynFiles>) -> Shared {
    let state = extend_service::build(cfg).await.unwrap();
    let state = match files {
        None => state,
        Some(files) => {
            let mut inner = Arc::try_unwrap(state).ok().expect("nothing else holds the state yet");
            inner.files = files;
            Arc::new(inner)
        }
    };
    common::seed_accounts(&state);
    state
}

async fn start() -> Env {
    start_with(|_, _| None).await
}

/// Serves on a free port. `files` gets the public URL and data directory and may return a store.
async fn start_with(files: impl FnOnce(&str, &Path) -> Option<DynFiles>) -> Env {
    let (url, data) = common::database("core").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let store = files(&base, &data);
    let state = build(common::config(url, addr, data, Tuning::default()), store).await;
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

/// The state alone, without serving (so no background scheduler runs): for driving scheduler
/// passes by hand.
async fn state_only(files: impl FnOnce(&Path) -> Option<DynFiles>) -> Shared {
    let (url, data) = common::database("core").await;
    let store = files(&data);
    build(
        common::config(url, "127.0.0.1:9".parse().unwrap(), data, Tuning::default()),
        store,
    )
    .await
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

/// Runs a command in the background, as `token`.
fn run_in_background(
    env: &Env,
    token: &str,
    session_id: &str,
    req: CommandRequest,
) -> tokio::task::JoinHandle<Result<CommandResult, silicon_extend_client::Error>> {
    let (base, token, sid) = (env.base.clone(), token.to_owned(), session_id.to_owned());
    tokio::spawn(async move { common::v2::V2::new(&base, &token).run(&sid, &req).await })
}

/// Waits (at most 20 s) for a background command's answer.
async fn answer(
    run: tokio::task::JoinHandle<Result<CommandResult, silicon_extend_client::Error>>,
) -> Result<CommandResult, silicon_extend_client::Error> {
    tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .expect("the command answered within 20 s, not at its deadline")
        .unwrap()
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

/// A scripted Extend app, before it starts serving.
struct Device {
    id: String,
    credential: String,
    ws: Ws,
}

impl Device {
    /// Enrolls, lets the Carbon `carbon` (an id) pair it (with access for `silicons`), connects and
    /// says hello.
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
        let mut ews = ws_connect(
            &client.ws_url(&format!("/api/v1/enrollments/{}/connect", e.enrollment_id)),
            &format!("Extend-Enrollment {}", e.enrollment_secret),
        )
        .await;
        let first: EnrollmentFrame = serde_json::from_str(&next_text(&mut ews).await).unwrap();
        let EnrollmentFrame::Code { pairing_code, .. } = first else {
            panic!("expected code")
        };
        let token = login(env, carbon).await;
        let claim = PairingClaim {
            pairing_code: pairing_code.to_lowercase(),
            name: format!("{} device", os.as_str()),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: silicons.iter().map(|s| (*s).to_owned()).collect(),
        };
        env.v2(&token).pair(&claim).await.unwrap();
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
        let mut ws = ws_connect(
            &client.ws_url("/api/v1/device/connect"),
            &format!("Extend-Device {credential}"),
        )
        .await;
        let hello = DeviceFrame::Hello(Hello {
            app_version: "1.0.0".into(),
            os,
            os_version: Some("15".into()),
            model: Some("Fake".into()),
            engine_version: None,
            capabilities: os.full_capabilities().to_vec(),
            missing: vec![],
            setup: Setup::complete(),
            features: vec![],
        });
        ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
            .await
            .unwrap();
        readiness::ready(&env.base, &token, &id).await;
        Device { id, credential, ws }
    }

    /// Serves in the background: answers pings, records every other frame, and answers commands
    /// (`wait <ms>` after that long, `wait forever` never; `screenshot` makes the files its
    /// arguments name: `name` uploaded, `missing:name` listed but never uploaded, `foreign:name`
    /// listed under an upload id the service never issued). Stops at `unpaired` or `superseded`.
    fn run(self, base: &str) -> Running {
        let frames: Arc<Mutex<Vec<ServiceFrame>>> = Arc::default();
        let (tx, mut rx) = mpsc::unbounded_channel::<DeviceFrame>();
        let (log, answers, credential, base) = (frames.clone(), tx.clone(), self.credential.clone(), base.to_owned());
        let mut ws = self.ws;
        let task = tokio::spawn(async move {
            let client = Client::connect(&base).await.unwrap();
            loop {
                tokio::select! {
                    out = rx.recv() => {
                        let Some(f) = out else { break };
                        if ws.send(Message::Text(serde_json::to_string(&f).unwrap().into())).await.is_err() {
                            break;
                        }
                    }
                    msg = ws.next() => {
                        let Some(Ok(msg)) = msg else { break };
                        let Message::Text(t) = msg else { continue };
                        let f: ServiceFrame = serde_json::from_str(&t).unwrap();
                        match &f {
                            ServiceFrame::Ping { nonce } => {
                                let _ = answers.send(DeviceFrame::Pong { nonce: *nonce });
                                continue;
                            }
                            ServiceFrame::Environment { .. } => continue,
                            ServiceFrame::Command(c) => {
                                tokio::spawn(execute(client.clone(), credential.clone(), c.clone(), answers.clone()));
                            }
                            _ => {}
                        }
                        let end = matches!(f, ServiceFrame::Unpaired { .. } | ServiceFrame::Superseded);
                        log.lock().unwrap().push(f);
                        if end {
                            break;
                        }
                    }
                }
            }
        });
        Running {
            id: self.id,
            credential: self.credential,
            frames,
            out: tx,
            task,
        }
    }
}

async fn execute(client: Client, credential: String, c: CommandFrame, out: mpsc::UnboundedSender<DeviceFrame>) {
    let mut outcome = CommandOutcome {
        id: c.id,
        ok: true,
        output: serde_json::json!({"echo": c.args}),
        text: Some(format!("ran {}", c.command)),
        error: None,
        files: vec![],
    };
    match c.command.as_str() {
        "wait" => match c.args.first().map(String::as_str) {
            Some("forever") => return,
            Some(ms) => tokio::time::sleep(Duration::from_millis(ms.parse().unwrap())).await,
            None => {}
        },
        "screenshot" => {
            let specs = if c.args.is_empty() {
                vec!["shot.png".to_owned()]
            } else {
                c.args.clone()
            };
            let mut ids = c.upload_ids.iter();
            for spec in &specs {
                let (how, name) = spec.split_once(':').unwrap_or(("upload", spec.as_str()));
                let mut bytes = b"\x89PNG ".to_vec();
                bytes.extend_from_slice(name.as_bytes());
                bytes.extend_from_slice(b" 0123456789");
                let upload_id = if how == "foreign" {
                    Uuid::now_v7()
                } else {
                    *ids.next().expect("an upload id left")
                };
                if how == "upload" {
                    client
                        .upload_artifact(&credential, upload_id, name, "image/png", bytes.clone())
                        .await
                        .unwrap();
                }
                outcome.files.push(ProducedFile {
                    upload_id,
                    name: name.into(),
                    content_type: "image/png".into(),
                    kind: FileKind::Screenshot,
                    size_bytes: bytes.len() as i64,
                });
            }
        }
        _ => {}
    }
    let _ = out.send(DeviceFrame::Result(outcome));
}

/// A device serving in the background.
struct Running {
    id: String,
    credential: String,
    frames: Arc<Mutex<Vec<ServiceFrame>>>,
    out: mpsc::UnboundedSender<DeviceFrame>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    fn frames(&self) -> Vec<ServiceFrame> {
        self.frames.lock().unwrap().clone()
    }
    fn send(&self, f: DeviceFrame) {
        self.out.send(f).unwrap();
    }
    async fn wait_for(&self, what: &str, pred: impl Fn(&ServiceFrame) -> bool) -> ServiceFrame {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(f) = self.frames().into_iter().find(|f| pred(f)) {
                return f;
            }
            assert!(
                Instant::now() < deadline,
                "the device never got {what}; it got {:?}",
                self.frames()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    /// Waits for the device to be told it is unpaired, and returns everything it got.
    async fn finished(self) -> Vec<ServiceFrame> {
        tokio::time::timeout(Duration::from_secs(20), self.task)
            .await
            .expect("the device's connection ended")
            .unwrap();
        self.frames.lock().unwrap().clone()
    }
}

fn position(frames: &[ServiceFrame], pred: impl Fn(&ServiceFrame) -> bool) -> usize {
    frames
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("frame missing from {frames:?}"))
}

async fn session_row(pool: &sqlx::PgPool, sid: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, end_reason FROM extend.sessions WHERE session_id = $1")
        .bind(sid)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A paired Android device serving in the background, a session on it for si:chef, and a command
/// that never answers, in flight.
struct InFlight {
    env: Env,
    alice: String,
    device: Running,
    session_id: String,
    run: tokio::task::JoinHandle<Result<CommandResult, silicon_extend_client::Error>>,
    started: Instant,
}

async fn command_in_flight() -> InFlight {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let session_id = env
        .v2(&chef)
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let mut wait = cmd("wait", &["forever"]);
    // Without an early answer, the Silicon would wait out this whole deadline.
    wait.timeout_ms = Some(120_000);
    let started = Instant::now();
    let run = run_in_background(&env, &chef, &session_id, wait);
    device
        .wait_for("the command", |f| matches!(f, ServiceFrame::Command(_)))
        .await;
    InFlight {
        env,
        alice,
        device,
        session_id,
        run,
        started,
    }
}

/// Checks the in-flight command's answer: `session_ended` naming `reason`, within seconds.
fn assert_ended_mid_command(
    result: Result<CommandResult, silicon_extend_client::Error>,
    started: Instant,
    reason: &str,
    explain: &str,
) -> Uuid {
    let e = result.expect_err("the command's session ended under it");
    assert!(started.elapsed() < Duration::from_secs(20), "answered at the deadline");
    assert_eq!(e.code(), ErrorCode::SessionEnded, "{e}");
    let api = e.api().unwrap();
    assert_eq!(api.details["end_reason"], reason, "{api:?}");
    assert_eq!(api.details["may_have_run"], true);
    assert!(
        api.message.contains("ended while `wait` was running") && api.message.contains(explain),
        "{}",
        api.message
    );
    assert!(api.hint.as_deref().is_some_and(|h| !h.is_empty()));
    api.details["command_id"].as_str().unwrap().parse().unwrap()
}

// ───────────────────────────── Idle timer ─────────────────────────────

#[tokio::test]
async fn a_running_command_holds_the_idle_timer() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    // Only a second of the idle window is left when a command that takes 6 s starts.
    sqlx::query("UPDATE extend.sessions SET idle_ends_at = now() + interval '1 second' WHERE session_id = $1")
        .bind(&sid)
        .execute(&env.pool)
        .await
        .unwrap();
    let mut wait = cmd("wait", &["6000"]);
    wait.timeout_ms = Some(20_000);
    let run = run_in_background(&env, &chef, &sid, wait);
    // The scheduler (every 2 s) has passed the old idle end twice by now.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let during = c.session(&sid).await.unwrap();
    assert_eq!(during.state, SessionState::Active, "{during:?}");
    let held = during.idle_ends_at.unwrap() - OffsetDateTime::now_utc();
    assert!(held > time::Duration::seconds(300), "held for {held}");

    let res = answer(run).await.unwrap();
    assert!(res.ok);
    assert!(res.warnings.is_empty());
    let after = c.session(&sid).await.unwrap();
    assert_eq!(after.state, SessionState::Active);
    let left = after.idle_ends_at.unwrap() - OffsetDateTime::now_utc();
    assert!(
        left > time::Duration::seconds(290) && left <= time::Duration::seconds(300),
        "{left}"
    );
    // The same time the result gave (the database keeps microseconds).
    let gap = res.idle_ends_at.unwrap() - after.idle_ends_at.unwrap();
    assert!(gap.abs() < time::Duration::milliseconds(1), "{gap}");
    assert!(
        !device
            .frames()
            .iter()
            .any(|f| matches!(f, ServiceFrame::SessionEnded { .. }))
    );
}

// ───────────────────────────── Access ending mid-command ─────────────────────────────

#[tokio::test]
async fn removing_the_device_mid_command_ends_the_session_and_answers_at_once() {
    let t = command_in_flight().await;
    let a = t.env.v2(&t.alice);
    a.remove_device(&t.device.id, None).await.unwrap();
    assert_ended_mid_command(
        answer(t.run).await,
        t.started,
        "device_removed",
        "the device was removed",
    );
    assert_eq!(
        session_row(&t.env.pool, &t.session_id).await,
        ("ended".into(), Some("device_removed".into()))
    );
    // Removing a device unpairs it: the session ends first, then the app is told.
    let (pool, credential, client) = (t.env.pool.clone(), t.device.credential.clone(), t.env.client.clone());
    let session_id = t.session_id.clone();
    let frames = t.device.finished().await;
    let ended = position(&frames, |f| {
        matches!(
            f,
            ServiceFrame::SessionEnded {
                reason: EndReason::DeviceRemoved,
                ..
            }
        )
    });
    let unpaired = position(&frames, |f| {
        matches!(
            f,
            ServiceFrame::Unpaired {
                reason: EndReason::DeviceRemoved
            }
        )
    });
    assert!(ended < unpaired, "{frames:?}");
    assert!(client.device_self(&credential).await.is_err());
    // The command is in the activity log with an unknown outcome: it may have run.
    let (outcome, error): (String, Option<String>) = sqlx::query_as(
        "SELECT outcome, details->>'error' FROM extend.activity WHERE action = 'command' AND session_id = $1",
    )
    .bind(&session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((outcome.as_str(), error.as_deref()), ("unknown", Some("session_ended")));
}

#[tokio::test]
async fn revoking_the_pair_on_the_device_mid_command_ends_the_session_and_answers_at_once() {
    let t = command_in_flight().await;
    let credential = t.device.credential.clone();
    t.env.client.revoke_pair(&credential).await.unwrap();
    assert_ended_mid_command(
        answer(t.run).await,
        t.started,
        "pair_revoked",
        "the pair was revoked on the device",
    );
    assert_eq!(
        session_row(&t.env.pool, &t.session_id).await,
        ("ended".into(), Some("pair_revoked".into()))
    );
    let frames = t.device.finished().await;
    let ended = position(&frames, |f| {
        matches!(
            f,
            ServiceFrame::SessionEnded {
                reason: EndReason::PairRevoked,
                ..
            }
        )
    });
    let unpaired = position(&frames, |f| {
        matches!(
            f,
            ServiceFrame::Unpaired {
                reason: EndReason::PairRevoked
            }
        )
    });
    assert!(ended < unpaired, "{frames:?}");
    assert!(t.env.client.device_self(&credential).await.is_err());
}

#[tokio::test]
async fn removing_the_silicons_access_mid_command_ends_the_session_and_cancels_the_command() {
    let t = command_in_flight().await;
    let a = t.env.v2(&t.alice);
    a.revoke(&t.device.id, "si:chef").await.unwrap();
    let command_id = assert_ended_mid_command(
        answer(t.run).await,
        t.started,
        "access_removed",
        "the Silicon's access to the device was removed",
    );
    assert_eq!(
        session_row(&t.env.pool, &t.session_id).await,
        ("ended".into(), Some("access_removed".into()))
    );
    t.device
        .wait_for("session_ended", |f| {
            matches!(
                f,
                ServiceFrame::SessionEnded {
                    reason: EndReason::AccessRemoved,
                    ..
                }
            )
        })
        .await;
    // The device stays paired and is told to drop the command.
    t.device
        .wait_for(
            "cancel",
            |f| matches!(f, ServiceFrame::Cancel { id } if *id == command_id),
        )
        .await;
    assert!(
        !t.device
            .frames()
            .iter()
            .any(|f| matches!(f, ServiceFrame::Unpaired { .. }))
    );
}

#[tokio::test]
async fn stop_on_the_device_by_frame_and_by_endpoint() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let device_id: extend_protocol::DeviceId = device.id.parse().unwrap();

    // The socket's `stop` frame.
    let s1 = c.start_session(&device_id).await.unwrap().session_id.to_string();
    device.send(DeviceFrame::Stop { target: None });
    device
        .wait_for("session_ended after stop", |f| {
            matches!(f, ServiceFrame::SessionEnded { session_id, reason: EndReason::StoppedByCarbon, .. } if session_id.as_str() == s1)
        })
        .await;
    assert_eq!(
        session_row(&env.pool, &s1).await,
        ("ended".into(), Some("stopped_by_carbon".into()))
    );
    assert_eq!(
        c.run(&s1, &cmd("snapshot", &[])).await.unwrap_err().code(),
        ErrorCode::SessionEnded
    );

    // POST /api/v1/device/stop, with a command in flight.
    let s2 = c.start_session(&device_id).await.unwrap().session_id.to_string();
    let mut wait = cmd("wait", &["forever"]);
    wait.timeout_ms = Some(120_000);
    let started = Instant::now();
    let run = run_in_background(&env, &chef, &s2, wait);
    device
        .wait_for("the command", |f| matches!(f, ServiceFrame::Command(_)))
        .await;
    env.client.device_stop(&device.credential).await.unwrap();
    assert_ended_mid_command(
        answer(run).await,
        started,
        "stopped_by_carbon",
        "the device's Carbon stopped it",
    );
    device
        .wait_for("session_ended after the stop endpoint", |f| {
            matches!(f, ServiceFrame::SessionEnded { session_id, reason: EndReason::StoppedByCarbon, .. } if session_id.as_str() == s2)
        })
        .await;
    // Nothing running: a no-op. A credential that isn't paired: refused.
    env.client.device_stop(&device.credential).await.unwrap();
    assert_eq!(
        env.client
            .device_stop("edc_notpairednotpairednotpairednotpairednotpaire")
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Unauthorized
    );
}

#[tokio::test]
async fn a_command_queued_behind_one_whose_session_ended_is_refused_not_relayed() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let mut wait = cmd("wait", &["forever"]);
    wait.timeout_ms = Some(120_000);
    let first = run_in_background(&env, &chef, &sid, wait);
    device
        .wait_for("the first command", |f| matches!(f, ServiceFrame::Command(_)))
        .await;
    // Commands in one session run one at a time: this one waits for the first.
    let queued = run_in_background(&env, &chef, &sid, cmd("snapshot", &[]));
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.end_session(&sid).await.unwrap();
    assert_eq!(answer(first).await.unwrap_err().code(), ErrorCode::SessionEnded);
    let e = answer(queued).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::SessionEnded, "{e}");
    assert_eq!(e.api().unwrap().details["end_reason"], "ended_by_silicon");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let relayed = device
        .frames()
        .iter()
        .filter(|f| matches!(f, ServiceFrame::Command(_)))
        .count();
    assert_eq!(relayed, 1, "the queued command reached the device of an ended session");
}

// ───────────────────────────── Sign-ins ending mid-session ─────────────────────────────

#[tokio::test]
async fn signing_out_without_a_webhook_stops_commands_and_the_event_ends_the_session() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    assert!(c.run(&sid, &cmd("snapshot", &[])).await.unwrap().ok);

    // si:chef signs out in Silicon Accounts; the event hasn't reached Extend. Once the cached
    // answer is gone (Extend asks Silicon Accounts again after at most 30 s), commands are refused.
    let chef_uuid = uuid("si:chef");
    env.state.accounts.local.as_deref().unwrap().sign_out(&chef_uuid);
    env.state.accounts.forget(&chef_uuid).await;
    let e = c.run(&sid, &cmd("snapshot", &[])).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::TokenExpired, "{e}");
    assert!(e.api().unwrap().message.contains("no longer active"), "{e}");
    // The session itself waits for the event (or its idle timeout).
    assert_eq!(session_row(&env.pool, &sid).await.0, "active");

    // The event ends it at once, and the device is told.
    let status = deliver(
        &env,
        "membership.signed_out",
        serde_json::json!({"uuid": chef_uuid, "membership_id": "m-chef", "reason": "session_revoked"}),
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(
        session_row(&env.pool, &sid).await,
        ("ended".into(), Some("silicon_logged_out".into()))
    );
    let seen = env.v2(&alice).session(&sid).await.unwrap();
    assert_eq!(seen.end_reason, Some(EndReason::SiliconLoggedOut));
    device
        .wait_for("session_ended", |f| {
            matches!(
                f,
                ServiceFrame::SessionEnded {
                    reason: EndReason::SiliconLoggedOut,
                    ..
                }
            )
        })
        .await;
}

#[tokio::test]
async fn removing_extends_access_in_silicon_accounts_ends_the_silicons_sessions() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let status = deliver(
        &env,
        "membership.access_removed",
        serde_json::json!({"uuid": uuid("si:chef")}),
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(
        session_row(&env.pool, &sid).await,
        ("ended".into(), Some("silicon_logged_out".into()))
    );
    device
        .wait_for("session_ended", |f| {
            matches!(
                f,
                ServiceFrame::SessionEnded {
                    reason: EndReason::SiliconLoggedOut,
                    ..
                }
            )
        })
        .await;
    // Its earlier tokens are refused; the grant stays, so a new sign-in can start again.
    assert_eq!(c.session(&sid).await.unwrap_err().code(), ErrorCode::TokenExpired);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let again = login(&env, "si:chef").await;
    assert!(env.v2(&again).start_session(&device.id.parse().unwrap()).await.is_ok());
}

#[tokio::test]
async fn an_expired_token_followed_by_a_fresh_one_keeps_the_session() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();

    // An access token past its expiry is refused; nothing else changes.
    let local = env.state.accounts.local.as_deref().unwrap();
    let account = local.ensure("si:chef", Some("c:alice")).unwrap();
    let expired = local.mint(&account, -120);
    let e = env.v2(&expired).session(&sid).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::TokenExpired);
    assert!(
        e.api().unwrap().hint.as_deref().unwrap_or("").contains("refresh"),
        "{e}"
    );
    // The CLI refreshes and retries with a fresh access token.
    let fresh = local.mint(&account, 1800);
    assert!(env.v2(&fresh).run(&sid, &cmd("snapshot", &[])).await.unwrap().ok);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(session_row(&env.pool, &sid).await, ("active".into(), None));
    assert!(
        !device
            .frames()
            .iter()
            .any(|f| matches!(f, ServiceFrame::SessionEnded { .. }))
    );
}

#[tokio::test]
async fn a_silicon_that_turned_telemetry_off_leaves_no_command_events() {
    let env = start().await;
    let token = login(&env, "si:chef").await;
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let on = env.v2(&token);
    let sid = on
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM extend.telemetry WHERE event->>'event' = 'command'")
            .fetch_one(&env.pool)
            .await
            .unwrap()
    };
    assert!(on.run(&sid, &cmd("snapshot", &[])).await.unwrap().ok);
    assert_eq!(count().await, 1, "telemetry on: the command is recorded");
    let quiet = reqwest::Client::new()
        .post(format!("{}/api/v2/sessions/{sid}/commands", env.base))
        .bearer_auth(&token)
        .header("x-extend-telemetry", "off")
        .json(&serde_json::json!({"type": "command", "data": cmd("snapshot", &[])}))
        .send()
        .await
        .unwrap();
    assert_eq!(quiet.status(), 200);
    assert_eq!(count().await, 1, "telemetry off: nothing more is recorded");
}

// ───────────────────────────── Ting ─────────────────────────────

#[tokio::test]
async fn starting_a_session_enrols_the_silicon_with_ting() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let ting = env.state.local_ting.clone().unwrap();
    let chef_uuid = uuid("si:chef");
    // (Pairing enrols the Carbon; the Silicon comes at its session.)
    assert!(!ting.registered.lock().await.contains(&chef_uuid));
    env.v2(&chef).start_session(&device.id.parse().unwrap()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ting.registered.lock().await.contains(&chef_uuid) {
        assert!(Instant::now() < deadline, "si:chef was never enrolled with Ting");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn insert_device(state: &Shared, id: &str) {
    sqlx::query(
        "INSERT INTO extend.devices (device_id, owner_id, name, os, state) VALUES ($1, $2, 'Pixel', 'android', 'ready')",
    )
    .bind(id)
    .bind(uuid("c:alice"))
    .execute(&state.pool)
    .await
    .unwrap();
}

/// A request as Extend 4 records it: from `from` to si:chef (the holder), with its frozen Ting
/// body, after one failed attempt.
async fn insert_request(state: &Shared, device_id: &str, from: &str, reason: &str) -> Uuid {
    let id = Uuid::now_v7();
    let body = extend_service::ting::body(
        "extend",
        extend_service::ting::DEVICE_REQUESTED,
        &uuid("si:chef"),
        "si:chef",
        &id.to_string(),
        serde_json::json!({"request_id": id, "from": from, "reason": reason, "routed_to": "holder"}),
    );
    sqlx::query(
        "INSERT INTO extend.requests (request_id, device_id, from_id, to_id, session_id, reason, attempts, last_error, routed_to, ting_body)
         VALUES ($1, $2, $3, $4, 'a3f', $5, 1, 'Ting is unavailable right now.', 'holder', $6)",
    )
    .bind(id)
    .bind(device_id)
    .bind(uuid(from))
    .bind(uuid("si:chef"))
    .bind(reason)
    .bind(&body)
    .execute(&state.pool)
    .await
    .unwrap();
    id
}

async fn request_row(state: &Shared, id: Uuid) -> (String, i32, Option<String>) {
    sqlx::query_as("SELECT delivery, attempts, last_error FROM extend.requests WHERE request_id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn pending_requests_are_retried_without_a_running_session_and_fail_with_a_reason() {
    let state = state_only(|_| None).await;
    let world = World::production();
    insert_device(&state, "0a1b2c3d").await;
    // Extend sends as itself: neither Silicon needs to be signed in or using Extend.
    let known = insert_request(&state, "0a1b2c3d", "si:sous", "Need it for an OTP").await;
    scheduler::retry_requests(&state, &world).await.unwrap();
    assert_eq!(request_row(&state, known).await, ("delivered".into(), 2, None));
    let ting = state.local_ting.clone().unwrap();
    assert!(
        ting.sent
            .lock()
            .await
            .iter()
            .any(|t| t["data"]["request_id"] == known.to_string()
                && t["data"]["reason"] == "Need it for an OTP"
                && t["for"] == uuid("si:chef").as_str())
    );

    // A recipient that never enrolled with Ting: the attempt counts, and the request waits for
    // the enrolment instead of being retried.
    ting.require_registration.store(true, Ordering::Relaxed);
    let unknown = insert_request(&state, "0a1b2c3d", "si:sous", "Quick check").await;
    scheduler::retry_requests(&state, &world).await.unwrap();
    let (delivery, attempts, error) = request_row(&state, unknown).await;
    assert_eq!((delivery.as_str(), attempts), ("pending", 2));
    assert!(error.unwrap().contains("has not enrolled"));
    scheduler::retry_requests(&state, &world).await.unwrap();
    assert_eq!(request_row(&state, unknown).await.1, 2, "it waits for the enrolment");
    // After 24 hours it gives up anyway.
    sqlx::query("UPDATE extend.requests SET created_at = now() - interval '25 hours' WHERE request_id = $1")
        .bind(unknown)
        .execute(&state.pool)
        .await
        .unwrap();
    scheduler::retry_requests(&state, &world).await.unwrap();
    let (delivery, _, error) = request_row(&state, unknown).await;
    assert_eq!(delivery, "failed");
    let error = error.unwrap();
    assert!(
        error.contains("has not enrolled") && error.contains("24 hours"),
        "{error}"
    );
    ting.require_registration.store(false, Ordering::Relaxed);

    // Counted attempts (Ting unreachable) back off, and give up after 6.
    let flaky = insert_request(&state, "0a1b2c3d", "si:sous", "Another reason").await;
    ting.unavailable.store(true, std::sync::atomic::Ordering::Relaxed);
    for _ in 0..8 {
        sqlx::query("UPDATE extend.requests SET ting_next_at = now() - interval '1 second' WHERE request_id = $1 AND delivery = 'pending'")
            .bind(flaky)
            .execute(&state.pool)
            .await
            .unwrap();
        scheduler::retry_requests(&state, &world).await.unwrap();
    }
    let (delivery, attempts, error) = request_row(&state, flaky).await;
    assert_eq!((delivery.as_str(), attempts), ("failed", scheduler::REQUEST_ATTEMPTS));
    let error = error.unwrap();
    assert!(error.contains("gave up after 6 attempts"), "{error}");
    // Nothing more is tried, and the device's log says each failed.
    scheduler::retry_requests(&state, &world).await.unwrap();
    assert_eq!(request_row(&state, flaky).await.1, scheduler::REQUEST_ATTEMPTS);
    let logged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM extend.activity WHERE action = 'request_failed' AND details->>'request_id' IN ($1, $2)",
    )
    .bind(unknown.to_string())
    .bind(flaky.to_string())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(logged, 2);
}

// ───────────────────────────── Files ─────────────────────────────

/// Local files, except that a name starting `refused` can't be stored, a name starting `unshared`
/// is stored but not shared, and deleting needs a proof Extend holds for the Silicon that made
/// the file (as with Briefcase). Records every deletion try as (creator, had a proof).
struct GatedFiles {
    local: LocalFiles,
    destroys: Mutex<Vec<(String, bool)>>,
    proof_held: AtomicBool,
    read_gate: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

impl GatedFiles {
    fn new(data: &Path, base: &str) -> Arc<Self> {
        Arc::new(Self {
            local: LocalFiles::new(&data.join("gated"), base).unwrap(),
            destroys: Mutex::default(),
            proof_held: AtomicBool::new(false),
            read_gate: Mutex::default(),
        })
    }
}

#[async_trait]
impl FileStore for GatedFiles {
    async fn store(&self, silicon: &Principal, file: NewFile<'_>) -> AppResult<Stored> {
        if file.name.starts_with("refused") {
            return Err(AppError::new(
                ErrorCode::NoAccess,
                format!(
                    "Briefcase refused to store {} for {} (/api/v1/obo/uploads/reserve answered 403 forbidden).",
                    file.name,
                    silicon.public_id()
                ),
            )
            .hint("Briefcase decides which apps may act for an account and where; ask a Briefcase Carbon to allow Extend's scopes."));
        }
        let unshared = file.name.starts_with("unshared");
        let mut stored = self.local.store(silicon, file).await?;
        if unshared {
            stored.shared_with = None;
            stored.share_error = Some("Briefcase does not know the recipient yet.".into());
        }
        Ok(stored)
    }
    async fn destroy(&self, creator: &str, file_id: Uuid) -> AppResult<()> {
        let held = self.proof_held.load(Ordering::SeqCst);
        self.destroys.lock().unwrap().push((creator.to_owned(), held));
        if !held {
            return Err(AppError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "Extend holds no Briefcase proof for the Silicon that made file {file_id}, so it can't trash it yet."
                ),
            ));
        }
        self.local.destroy(creator, file_id).await
    }
    async fn read_bounded(&self, member: &Principal, file_id: Uuid, max_bytes: usize) -> AppResult<(Vec<u8>, String)> {
        let gate = self.read_gate.lock().unwrap().clone();
        if let Some((entered, resume)) = gate {
            entered.notify_one();
            resume.notified().await;
        }
        self.local.read_bounded(member, file_id, max_bytes).await
    }
    async fn read_local(&self, file_id: Uuid) -> Option<(Vec<u8>, String)> {
        self.local.read_local(file_id).await
    }
}

async fn insert_file(state: &Shared, due: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO extend.files (file_id, device_id, created_by, name, kind, content_type, size_bytes, url, self_destruct_at)
         VALUES ($1, '0a1b2c3d', $3, 'shot.png', 'screenshot', 'image/png', 4, 'http://files.test/x',
                 now() + CASE WHEN $2 THEN interval '-1 minute' ELSE interval '1 day' END)",
    )
    .bind(id)
    .bind(due)
    .bind(uuid("si:chef"))
    .execute(&state.pool)
    .await
    .unwrap();
    id
}

async fn file_exists(state: &Shared, id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM extend.files WHERE file_id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
        == 1
}

#[tokio::test]
async fn self_destruct_keeps_the_record_until_the_file_is_gone_and_needs_no_session() {
    let mut gated = None;
    let state = state_only(|data| {
        let g = GatedFiles::new(data, "http://127.0.0.1:9");
        gated = Some(g.clone());
        Some(g as DynFiles)
    })
    .await;
    let gated = gated.unwrap();
    let world = World::production();
    insert_device(&state, "0a1b2c3d").await;
    let due = insert_file(&state, true).await;
    let later = insert_file(&state, false).await;
    let chef = uuid("si:chef");

    // Extend holds no proof for si:chef: the file can't be deleted yet, so its record stays.
    let mut now = Backoff::new(Duration::ZERO, Duration::ZERO);
    assert_eq!(scheduler::self_destruct(&state, &world, &mut now).await.unwrap(), 0);
    assert!(file_exists(&state, due).await);
    assert_eq!(*gated.destroys.lock().unwrap(), vec![(chef.clone(), false)]);
    // Still hidden from the Silicon while it waits.
    let listed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM extend.files WHERE file_id = $1 AND (self_destruct_at IS NULL OR self_destruct_at > now())",
    )
    .bind(due)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(listed, 0);

    // Failures back off: the next pass leaves it alone until its wait is over.
    let mut paced = Backoff::default();
    scheduler::self_destruct(&state, &world, &mut paced).await.unwrap();
    assert_eq!(paced.tries(&world, due), 1);
    scheduler::self_destruct(&state, &world, &mut paced).await.unwrap();
    assert_eq!(gated.destroys.lock().unwrap().len(), 2);
    assert_eq!(paced.tries(&world, due), 1);

    // si:chef uses Extend again (no session needed): Extend holds a proof, and the file goes, and
    // so does its record.
    gated.proof_held.store(true, Ordering::SeqCst);
    assert_eq!(scheduler::self_destruct(&state, &world, &mut now).await.unwrap(), 1);
    assert!(!file_exists(&state, due).await);
    assert_eq!(gated.destroys.lock().unwrap().last().cloned(), Some((chef, true)));
    assert!(file_exists(&state, later).await);
}

#[tokio::test]
async fn files_that_cant_be_stored_or_shared_are_reported_as_warnings() {
    let env = start_with(|base, data| Some(GatedFiles::new(data, base) as DynFiles)).await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let res = c
        .run(
            &sid,
            &cmd(
                "screenshot",
                &[
                    "shot.png",
                    "missing:gone.png",
                    "foreign:stray.png",
                    "refused.png",
                    "unshared.png",
                ],
            ),
        )
        .await
        .unwrap();
    assert!(res.ok);
    let names: Vec<&str> = res.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["shot.png", "unshared.png"]);
    assert_eq!(res.files[1].shared_with, None);
    assert_eq!(res.warnings.len(), 4, "{:#?}", res.warnings);
    let warning = |name: &str| {
        res.warnings
            .iter()
            .find(|w| w.starts_with(name))
            .unwrap_or_else(|| panic!("no warning for {name}: {:#?}", res.warnings))
            .clone()
    };
    assert!(warning("gone.png").contains("never uploaded it"));
    assert!(warning("stray.png").contains("Extend did not issue for this command"));
    let refused = warning("refused.png");
    assert!(
        refused.contains("was not stored: Briefcase refused")
            && refused.contains("Briefcase Carbon")
            && refused.contains("Run the command again"),
        "{refused}"
    );
    assert!(warning("unshared.png").contains("is stored, but not shared with c:alice"));
    // The device's activity log keeps them too.
    let logged: serde_json::Value = sqlx::query_scalar("SELECT details->'warnings' FROM extend.activity WHERE id = $1")
        .bind(res.command_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(logged.as_array().unwrap().len(), 4);
}

#[test]
fn command_results_from_older_services_have_no_warnings() {
    let old = serde_json::json!({
        "command_id": Uuid::now_v7(), "session_id": "a3f", "command": "snapshot", "ok": true,
        "started_at": "2026-09-27T10:00:00Z", "duration_ms": 12, "idle_ends_at": null
    });
    let r: CommandResult = serde_json::from_value(old.clone()).unwrap();
    assert!(r.warnings.is_empty());
    let mut new = old;
    new["warnings"] = serde_json::json!(["x.png was not stored: ..."]);
    let r: CommandResult = serde_json::from_value(new).unwrap();
    assert_eq!(r.warnings, vec!["x.png was not stored: ...".to_owned()]);
    let back = serde_json::to_value(&r).unwrap();
    assert_eq!(back["warnings"][0], "x.png was not stored: ...");
}

#[tokio::test]
async fn file_content_is_served_to_its_silicon_and_owner_with_ranges() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let c = env.v2(&chef);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&device.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let shot = c.run(&sid, &cmd("screenshot", &[])).await.unwrap();
    let f = &shot.files[0];
    let id = f.file_id.to_string();
    let expected = b"\x89PNG shot.png 0123456789".to_vec();

    let whole = c.file_content(&id).await.unwrap();
    assert_eq!(whole.bytes, expected);
    assert_eq!(whole.content_type, "image/png");
    assert_eq!(whole.name.as_deref(), Some("shot.png"));
    assert_eq!(whole.range, None);
    let total = expected.len() as u64;
    let part = c.file_range(&id, Some((2, Some(5)))).await.unwrap();
    assert_eq!(part.bytes, expected[2..=5].to_vec());
    assert_eq!(part.range, Some((2, 5, total)));
    let rest = c.file_range(&id, Some((10, None))).await.unwrap();
    assert_eq!(rest.bytes, expected[10..].to_vec());
    assert_eq!(rest.range, Some((10, total - 1, total)));

    // The Carbon who owns the device reads it, and so does the Silicon's custodian (alice too);
    // another Silicon can't see it, nor can a Carbon with nothing to do with it.
    assert_eq!(env.v2(&alice).file_content(&id).await.unwrap().bytes, expected);
    assert_eq!(
        env.v2(&sous).file_content(&id).await.unwrap_err().code(),
        ErrorCode::FileNotFound
    );
    let bob = login(&env, "c:bob").await;
    assert_eq!(
        env.v2(&bob).file_content(&id).await.unwrap_err().code(),
        ErrorCode::FileNotFound
    );

    // Headers, and a range past the end.
    let http = reqwest::Client::new();
    let url = format!("{}/api/v2/files/{id}/content", env.base);
    let resp = http.get(&url).bearer_auth(&chef).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let h = |n: &str| resp.headers().get(n).unwrap().to_str().unwrap().to_owned();
    assert_eq!(h("content-length"), total.to_string());
    assert_eq!(
        h("content-disposition"),
        "attachment; filename=\"shot.png\"; filename*=UTF-8''shot.png"
    );
    assert_eq!(h("accept-ranges"), "bytes");
    assert_eq!(h("x-content-type-options"), "nosniff");
    let resp = http
        .get(&url)
        .bearer_auth(&chef)
        .header("Range", format!("bytes={}-", total + 5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 416);
    assert_eq!(
        resp.headers().get("content-range").unwrap(),
        &format!("bytes */{total}")
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["code"], "invalid_input");
    assert!(body["data"]["hint"].as_str().unwrap().contains("Range"));

    // Once its self-destruct time passes, it is gone for everyone.
    sqlx::query("UPDATE extend.files SET self_destruct_at = now() - interval '1 second' WHERE file_id = $1")
        .bind(f.file_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(c.file_content(&id).await.unwrap_err().code(), ErrorCode::FileNotFound);
}

#[tokio::test]
async fn display_resolves_own_stored_files_into_compatible_device_attachments() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let source = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef"])
        .await
        .run(&env.base);
    let source_sid = c
        .start_session(&source.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let f = c
        .run(&source_sid, &cmd("screenshot", &[]))
        .await
        .unwrap()
        .files
        .remove(0);
    c.end_session(&source_sid).await.unwrap();
    let tv = Device::pair(&env, "c:alice", DeviceOs::AndroidTv, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&tv.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    for value in [
        format!("file:{}", f.file_id),
        f.file_id.to_string(),
        f.url,
        format!("{}/api/v2/files/{}/content", env.base, f.file_id),
    ] {
        let result = c
            .run(&sid, &cmd("display", &["show", &format!("--image={value}")]))
            .await
            .unwrap();
        assert!(result.ok);
        let frame = tv
            .frames()
            .into_iter()
            .find_map(|f| match f {
                ServiceFrame::Command(c) if c.id == result.command_id => Some(c),
                _ => None,
            })
            .unwrap();
        assert_eq!(frame.attachments.len(), 1);
        let a = &frame.attachments[0];
        assert_eq!(frame.args, ["show", &format!("--image=attachment:{}", a.name)]);
        assert_eq!(a.content_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&a.content_base64)
                .unwrap(),
            b"\x89PNG shot.png 0123456789"
        );
        assert!(
            !serde_json::to_string(&frame).unwrap().contains(&chef),
            "no caller credential reaches the TV"
        );
    }
    // A public URL is passed through unchanged, without fetching it from the service.
    let private_url = format!("https://briefcase.example/si:chef/apps/extend/shot-{}.png", f.file_id);
    sqlx::query("UPDATE extend.files SET url = $2 WHERE file_id = $1")
        .bind(f.file_id)
        .bind(&private_url)
        .execute(&env.pool)
        .await
        .unwrap();
    assert!(
        c.run(&sid, &cmd("display", &["show", "--image", &private_url]))
            .await
            .unwrap()
            .ok
    );
    let result = c
        .run(
            &sid,
            &cmd("display", &["show", "--image", "https://example.invalid/public.png"]),
        )
        .await
        .unwrap();
    let frame = tv
        .frames()
        .into_iter()
        .find_map(|f| match f {
            ServiceFrame::Command(c) if c.id == result.command_id => Some(c),
            _ => None,
        })
        .unwrap();
    assert!(frame.attachments.is_empty());
    assert_eq!(frame.args[2], "https://example.invalid/public.png");
}

#[tokio::test]
async fn display_files_enforce_owner_expiry_type_and_attachment_limits_before_relay() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let c = env.v2(&chef);
    let tv = Device::pair(&env, "c:alice", DeviceOs::AndroidTv, &["si:chef", "si:sous"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&tv.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let f = c.run(&sid, &cmd("screenshot", &[])).await.unwrap().files.remove(0);
    let display = cmd("display", &["show", "--image", &format!("file:{}", f.file_id)]);
    let private_url = format!("https://briefcase.example/si:chef/apps/extend/shot-{}.png", f.file_id);
    sqlx::query("UPDATE extend.files SET url = $2 WHERE file_id = $1")
        .bind(f.file_id)
        .bind(&private_url)
        .execute(&env.pool)
        .await
        .unwrap();
    c.end_session(&sid).await.unwrap();
    let other = env.v2(&sous);
    let other_sid = other
        .start_session(&tv.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    assert_eq!(
        other.run(&other_sid, &display).await.unwrap_err().code(),
        ErrorCode::FileNotFound
    );
    assert_eq!(
        other
            .run(&other_sid, &cmd("display", &["show", "--image", &f.url]))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::FileNotFound
    );
    assert_eq!(
        other
            .run(&other_sid, &cmd("display", &["show", "--image", &private_url]))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::FileNotFound
    );
    assert_eq!(
        c.run(&other_sid, &display).await.unwrap_err().code(),
        ErrorCode::SessionNotFound
    );
    other.end_session(&other_sid).await.unwrap();
    let sid = c
        .start_session(&tv.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    sqlx::query("UPDATE extend.files SET self_destruct_at = now() - interval '1 second' WHERE file_id = $1")
        .bind(f.file_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(c.run(&sid, &display).await.unwrap_err().code(), ErrorCode::FileNotFound);
    sqlx::query("UPDATE extend.files SET self_destruct_at = NULL, content_type = 'text/plain' WHERE file_id = $1")
        .bind(f.file_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(c.run(&sid, &display).await.unwrap_err().code(), ErrorCode::InvalidInput);
    sqlx::query("UPDATE extend.files SET content_type = 'image/png' WHERE file_id = $1")
        .bind(f.file_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let mut full = display.clone();
    full.attachments = vec![
        Attachment {
            name: "existing".into(),
            content_type: "text/plain".into(),
            content_base64: String::new()
        };
        8
    ];
    assert_eq!(c.run(&sid, &full).await.unwrap_err().code(), ErrorCode::InvalidInput);
    full.attachments = vec![Attachment {
        name: "existing".into(),
        content_type: "image/png".into(),
        content_base64: base64::engine::general_purpose::STANDARD.encode(vec![0u8; 8 << 20]),
    }];
    assert_eq!(c.run(&sid, &full).await.unwrap_err().code(), ErrorCode::InvalidInput);
    // Stale small metadata cannot bypass the actual store's read limit.
    tokio::fs::write(
        env.state.cfg.data_dir.join("files").join(f.file_id.to_string()),
        vec![0u8; (8 << 20) + 1],
    )
    .await
    .unwrap();
    assert_eq!(c.run(&sid, &display).await.unwrap_err().code(), ErrorCode::InvalidInput);
    assert!(
        tv.frames()
            .iter()
            .all(|f| !matches!(f, ServiceFrame::Command(c) if c.command == "display"))
    );
}

#[tokio::test]
async fn briefcase_reads_carry_a_proof_and_are_bounded_even_without_content_length() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let principal = env.state.accounts.authenticate(&chef).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/api/v1/obo/files/read",
        axum::routing::post(
            |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| async move {
                let auth = headers["authorization"].to_str().unwrap().to_owned();
                assert!(auth.starts_with("Proof sap_"), "{auth}");
                for gone in ["x-iam-obo-access-token", "x-iam-obo-access-proof", "x-org-id"] {
                    assert!(!headers.contains_key(gone), "{gone}");
                }
                assert!(body["entry_id"].as_str().unwrap().parse::<Uuid>().is_ok());
                let chunks = futures::stream::iter([Ok::<_, std::io::Error>("123"), Ok("456")]);
                ([("Content-Type", "image/png")], axum::body::Body::from_stream(chunks))
            },
        ),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let files = extend_service::files::BriefcaseFiles::new(
        base,
        "https://briefcase.example".into(),
        "extend".into(),
        env.state.proofs.clone(),
    );
    use extend_service::files::FileStore as _;
    assert_eq!(
        files.read_bounded(&principal, Uuid::new_v4(), 6).await.unwrap(),
        (b"123456".to_vec(), "image/png".into())
    );
    assert_eq!(
        files
            .read_bounded(&principal, Uuid::new_v4(), 5)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidInput
    );
    // A User verification proof for Briefcase, as si:chef, to read only; reused, not reissued.
    let issued = env.state.accounts.local.as_deref().unwrap().issued_proofs();
    let reads: Vec<_> = issued.iter().filter(|p| p["receiving_app"] == "briefcase").collect();
    assert_eq!(reads.len(), 1, "{issued:?}");
    assert_eq!(
        (reads[0]["kind"].as_str(), reads[0]["user"].as_str()),
        (Some("user_verification"), Some(uuid("si:chef").as_str()))
    );
    assert_eq!(
        reads[0]["scopes"],
        serde_json::json!(extend_service::proofs::BRIEFCASE_READ_SCOPES)
    );
    server.abort();
}

#[tokio::test]
async fn display_file_read_respects_timeout_and_stop_before_relay() {
    let mut files = None;
    let env = start_with(|base, data| {
        let store = GatedFiles::new(data, base);
        files = Some(store.clone());
        Some(store)
    })
    .await;
    let files = files.unwrap();
    let chef = login(&env, "si:chef").await;
    let c = env.v2(&chef);
    let tv = Device::pair(&env, "c:alice", DeviceOs::AndroidTv, &["si:chef"])
        .await
        .run(&env.base);
    let sid = c
        .start_session(&tv.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let f = c.run(&sid, &cmd("screenshot", &[])).await.unwrap().files.remove(0);
    let mut display = cmd("display", &["show", "--image", &format!("file:{}", f.file_id)]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *files.read_gate.lock().unwrap() = Some((entered.clone(), resume.clone()));
    let reading = run_in_background(&env, &chef, &sid, display.clone());
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    c.end_session(&sid).await.unwrap();
    resume.notify_one();
    assert_eq!(answer(reading).await.unwrap_err().code(), ErrorCode::SessionEnded);

    let sid = c
        .start_session(&tv.id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    display.timeout_ms = Some(1000);
    let error = tokio::time::timeout(Duration::from_secs(3), c.run(&sid, &display))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CommandTimeout);
    assert!(
        tv.frames()
            .iter()
            .all(|f| !matches!(f, ServiceFrame::Command(c) if c.command == "display"))
    );
}

// ───────────────────────────── Requests ─────────────────────────────

async fn sent_reasons(ting: &extend_service::ting::LocalNotifier) -> Vec<serde_json::Value> {
    ting.sent
        .lock()
        .await
        .iter()
        .map(|t| t["data"]["reason"].clone())
        .collect()
}

#[tokio::test]
async fn each_new_reason_is_delivered_exactly_as_sent() {
    let env = start().await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let s = env.v2(&sous);
    let device = Device::pair(&env, "c:alice", DeviceOs::Android, &["si:chef", "si:sous", "si:line"])
        .await
        .run(&env.base);
    let id = device.id.clone();
    let chef_session = env
        .v2(&chef)
        .start_session(&id.parse().unwrap())
        .await
        .unwrap()
        .session_id
        .to_string();
    let ting = env.state.local_ting.clone().unwrap();

    // Surrounding whitespace is kept: the reason travels exactly as the Silicon wrote it.
    let raw = "  Need it for an OTP \n";
    let first = s.send_request(&id, raw).await.unwrap();
    assert_eq!(first.reason, raw);
    assert_eq!(first.delivery, Delivery::Delivered);
    assert_eq!(sent_reasons(&ting).await, vec![serde_json::json!(raw)]);
    // The same reason again within 60 s is a repeat: the same request, nothing sent.
    let again = s.send_request(&id, raw).await.unwrap();
    assert_eq!(again.request_id, first.request_id);
    assert_eq!(sent_reasons(&ting).await.len(), 1);
    // A new reason within 60 s is a new request, and it is delivered.
    let second = s.send_request(&id, "Different, urgent reason").await.unwrap();
    assert_ne!(second.request_id, first.request_id);
    assert_eq!(second.delivery, Delivery::Delivered);
    assert_eq!(
        sent_reasons(&ting).await,
        vec![serde_json::json!(raw), serde_json::json!("Different, urgent reason")]
    );

    // The length is counted without the whitespace around it.
    assert_eq!(
        s.send_request(&id, " \n\t ").await.unwrap_err().code(),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        s.send_request(&id, &"x".repeat(301)).await.unwrap_err().code(),
        ErrorCode::InvalidInput
    );
    let padded = format!("   {}   ", "y".repeat(300));
    assert_eq!(s.send_request(&id, &padded).await.unwrap().reason, padded);
    let flood = format!("{}{}", " ".repeat(900), "z".repeat(200));
    let e = s.send_request(&id, &flood).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::InvalidInput);
    assert!(e.api().unwrap().message.contains("at most 1000"));

    // An Idempotency-Key replays its own request and refuses another body.
    let post = |key: &str, reason: &str| {
        reqwest::Client::new()
            .post(format!("{}/api/v2/devices/{id}/requests", env.base))
            .bearer_auth(&sous)
            .header("Idempotency-Key", key)
            .json(&serde_json::json!({"type": "request", "data": {"reason": reason}}))
            .send()
    };
    let r = post("core-gaps-key-1", "Keyed reason").await.unwrap();
    assert_eq!(r.status(), 201);
    let keyed: serde_json::Value = r.json().await.unwrap();
    let delivered = sent_reasons(&ting).await.len();
    let r = post("core-gaps-key-1", "Keyed reason").await.unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(r.headers()["idempotency-replayed"], "true");
    let replay: serde_json::Value = r.json().await.unwrap();
    assert_eq!(replay, keyed);
    let r = post("core-gaps-key-1", "Another reason").await.unwrap();
    assert_eq!(r.status(), 409);
    // A reason already sent without a key must not hide a conflicting or malformed key.
    let r = post("core-gaps-key-1", raw).await.unwrap();
    assert_eq!(r.status(), 409);
    let r = post("short", raw).await.unwrap();
    assert_eq!(r.status(), 422);
    // A fresh key can fold an existing request; its retry replays that stored 200 response.
    let r = post("core-gaps-key-2", raw).await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(!r.headers().contains_key("idempotency-replayed"));
    let folded: serde_json::Value = r.json().await.unwrap();
    assert_eq!(folded["data"]["request_id"], first.request_id.to_string());
    let r = post("core-gaps-key-2", raw).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["idempotency-replayed"], "true");
    assert_eq!(r.json::<serde_json::Value>().await.unwrap(), folded);
    assert_eq!(sent_reasons(&ting).await.len(), delivered);
    // Everything delivered is listed with its reason as sent.
    let mine = s
        .requests(ListQuery {
            direction: Some("sent".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(mine.items.iter().any(|r| r.reason == raw));
    assert!(mine.items.iter().all(|r| r.last_error.is_none()));

    // Someone else is using the device now: the same reason goes to them.
    env.v2(&chef).end_session(&chef_session).await.unwrap();
    // Completing the original holder's session must not erase a successful keyed response.
    let r = post("core-gaps-key-1", "Keyed reason").await.unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(r.headers()["idempotency-replayed"], "true");
    assert_eq!(r.json::<serde_json::Value>().await.unwrap(), keyed);
    assert_eq!(
        s.send_request(&id, "Nobody is using it").await.unwrap_err().code(),
        ErrorCode::DeviceNotInUse
    );
    let line = login(&env, "si:line").await;
    let line_session = env.v2(&line).start_session(&id.parse().unwrap()).await.unwrap();
    let to_line = s.send_request(&id, raw).await.unwrap();
    assert_ne!(to_line.request_id, first.request_id);
    assert_eq!(
        (to_line.to.as_str(), to_line.delivery),
        ("si:line", Delivery::Delivered)
    );
    let r = post("core-gaps-key-1", "Keyed reason").await.unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(r.headers()["idempotency-replayed"], "true");
    assert_eq!(r.json::<serde_json::Value>().await.unwrap(), keyed);
    env.v2(&line)
        .end_session(line_session.session_id.as_ref())
        .await
        .unwrap();
    s.start_session(&id.parse().unwrap()).await.unwrap();
    let r = post("core-gaps-key-1", "Keyed reason").await.unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(r.headers()["idempotency-replayed"], "true");
    assert_eq!(r.json::<serde_json::Value>().await.unwrap(), keyed);
    assert_eq!(
        s.send_request(&id, "I am using it now").await.unwrap_err().code(),
        ErrorCode::Conflict
    );
    assert_eq!(sent_reasons(&ting).await.len(), delivered + 1);
}
