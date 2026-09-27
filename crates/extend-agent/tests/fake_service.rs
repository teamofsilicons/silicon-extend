//! End-to-end against an in-process fake Extend service (axum HTTP + WebSockets), following
//! `docs/device-protocol.md` exactly: enrollment → code rotation → paired → hello → commands and
//! uploads → sessions → takeover → attach → unpaired → a fresh enrollment; and the close codes.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use extend_agent::agent::{Agent, AgentDeps, AgentHandle, UiAction};
use extend_agent::config::Config;
use extend_agent::credential;
use extend_agent::hosted::DriverFactory;
use extend_agent::status::Phase;
use extend_driver::{Driver, Invocation, LocalFile, Output, Probe};
use extend_protocol::model::{FileKind, MissingCapability, Setup};
use extend_protocol::{Capability, DeviceOs};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc;

const CREDENTIAL: &str = "edc_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const SECRET: &str = "ees_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const DEVICE_ID: &str = "7c1e09ab";

/// Everything the fake service saw, in order.
#[derive(Debug, Clone)]
enum Seen {
    CreateEnrollment(Value),
    EnrollmentSocket {
        auth: String,
    },
    DeviceSocket {
        auth: String,
    },
    DeviceSelf {
        auth: String,
    },
    Upload {
        id: String,
        name: String,
        content_type: String,
        sha_ok: bool,
        bytes: usize,
    },
    Revoke,
    Stop,
    Frame(Value),
}

#[derive(Default)]
struct Inner {
    seen: Vec<Seen>,
    /// Frames to push down whichever socket is open.
    to_socket: Option<mpsc::UnboundedSender<String>>,
    /// Close codes to use on the next device-socket connections (0 = none).
    next_device_close: VecDeque<u16>,
    enrollments: u32,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Inner>>);

impl Fake {
    fn seen(&self) -> Vec<Seen> {
        self.0.lock().unwrap().seen.clone()
    }
    fn push(&self, s: Seen) {
        self.0.lock().unwrap().seen.push(s);
    }
    fn send(&self, frame: Value) {
        let tx = self.0.lock().unwrap().to_socket.clone().expect("a socket is open");
        // A bare string is a control word for the relay ("__drop", "__close_4401").
        let text = match frame {
            Value::String(s) => s,
            other => other.to_string(),
        };
        tx.send(text).unwrap();
    }
    fn frames(&self) -> Vec<Value> {
        self.seen()
            .into_iter()
            .filter_map(|s| if let Seen::Frame(v) = s { Some(v) } else { None })
            .collect()
    }
    fn count(&self, f: impl Fn(&Seen) -> bool) -> usize {
        self.seen().iter().filter(|s| f(s)).count()
    }
}

fn auth(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

async fn create_enrollment(State(f): State<Fake>, Json(body): Json<Value>) -> Response {
    f.push(Seen::CreateEnrollment(body));
    let n = {
        let mut g = f.0.lock().unwrap();
        g.enrollments += 1;
        g.enrollments
    };
    (
        StatusCode::CREATED,
        Json(json!({"type":"enrollment","data":{
            "enrollment_id": format!("0192f000-0000-7000-8000-00000000000{n}"),
            "enrollment_secret": SECRET,
            "pairing_code": "4F9C2A",
            "code_expires_at": expires_in(300),
            "rotates_every_s": 300
        }})),
    )
        .into_response()
}

fn expires_in(secs: i64) -> String {
    (time::OffsetDateTime::now_utc() + time::Duration::seconds(secs))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

async fn get_enrollment() -> Response {
    Json(json!({"type":"enrollment","data":{"state":"waiting","pairing_code":"4F9C2A","code_expires_at":expires_in(300)}})).into_response()
}

async fn enrollment_socket(
    State(f): State<Fake>,
    headers: HeaderMap,
    Path(_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if auth(&headers) != format!("Extend-Enrollment {SECRET}") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    f.push(Seen::EnrollmentSocket { auth: auth(&headers) });
    ws.on_upgrade(move |socket| relay(f, socket, None))
}

async fn device_socket(State(f): State<Fake>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if auth(&headers) != format!("Extend-Device {CREDENTIAL}") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    f.push(Seen::DeviceSocket { auth: auth(&headers) });
    let close = f.0.lock().unwrap().next_device_close.pop_front();
    ws.on_upgrade(move |socket| relay(f, socket, close))
}

/// Records every frame from the app and pushes the test's frames to it.
async fn relay(f: Fake, mut socket: WebSocket, close_with: Option<u16>) {
    if let Some(code) = close_with {
        let _ = socket
            .send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                code,
                reason: "test".into(),
            })))
            .await;
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    f.0.lock().unwrap().to_socket = Some(tx);
    loop {
        tokio::select! {
            m = socket.recv() => match m {
                Some(Ok(WsMessage::Text(t))) => f.push(Seen::Frame(serde_json::from_str(t.as_str()).unwrap())),
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            out = rx.recv() => match out {
                Some(text) if text == "__close_4401" => {
                    let _ = socket.send(WsMessage::Close(Some(axum::extract::ws::CloseFrame { code: 4401, reason: "unpaired".into() }))).await;
                    break;
                }
                Some(text) if text == "__drop" => break,
                Some(text) => { if socket.send(WsMessage::Text(text.into())).await.is_err() { break; } }
                None => break,
            }
        }
    }
}

async fn device_self(State(f): State<Fake>, headers: HeaderMap) -> Response {
    f.push(Seen::DeviceSelf { auth: auth(&headers) });
    Json(json!({"type":"device_self","data":{
        "device_id": DEVICE_ID, "name": "Test Mac", "owner": {"type":"carbon","id":"c:alice"},
        "team": "acme", "os": "macos", "in_use": null, "takeover": null,
        "setup": {"state":"complete","steps":[]}, "environment": null
    }}))
    .into_response()
}

async fn revoke(State(f): State<Fake>) -> StatusCode {
    f.push(Seen::Revoke);
    StatusCode::NO_CONTENT
}

async fn stop(State(f): State<Fake>) -> StatusCode {
    f.push(Seen::Stop);
    StatusCode::NO_CONTENT
}

async fn upload(
    State(f): State<Fake>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let digest = extend_protocol::ids::hex_lower(&Sha256::digest(&body));
    let sha_ok = h("x-content-sha256") == digest;
    f.push(Seen::Upload {
        id,
        name: h("x-file-name"),
        content_type: h("content-type"),
        sha_ok,
        bytes: body.len(),
    });
    if h("authorization") != format!("Extend-Device {CREDENTIAL}") {
        return StatusCode::UNAUTHORIZED;
    }
    if sha_ok {
        StatusCode::CREATED
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    }
}

async fn start_fake() -> (Fake, SocketAddr) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/api/v1/enrollments", post(create_enrollment))
        .route(
            "/api/v1/enrollments/{id}",
            get(get_enrollment).delete(|| async { StatusCode::NO_CONTENT }),
        )
        .route("/api/v1/enrollments/{id}/connect", get(enrollment_socket))
        .route("/api/v1/device", get(device_self).delete(revoke))
        .route("/api/v1/device/stop", post(stop))
        .route("/api/v1/device/connect", get(device_socket))
        .route("/api/v1/device/artifacts/{id}", put(upload))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, addr)
}

/// A computer that can take screenshots and snapshots, and run a slow command.
#[derive(Default)]
struct FakeComputer {
    started: Mutex<Vec<String>>,
    ended: Mutex<Vec<String>>,
    /// Commands and lifecycle hooks in the order they happened.
    events: Mutex<Vec<String>>,
    /// An ended session still holds the screen (a failed release): only the terminal works.
    /// Session setup releases it; session cleanup leaves it held again.
    held: std::sync::atomic::AtomicBool,
    /// The screen is locked: only the terminal works, and the screen watch says so.
    locked: std::sync::atomic::AtomicBool,
}

const LOCKED_REASON: &str = "This computer is locked. Unlock it to let a Silicon use it.";

/// The agent's screen watch, reading the fake computer's lock.
fn screen_watch(computer: &Arc<FakeComputer>) -> Option<extend_agent::agent::ScreenWatch> {
    let computer = computer.clone();
    Some(Arc::new(move || {
        computer
            .locked
            .load(std::sync::atomic::Ordering::SeqCst)
            .then_some(extend_agent::drivers::screen_lock::ScreenBlock::Locked)
    }))
}

#[async_trait]
impl Driver for FakeComputer {
    async fn probe(&self) -> Probe {
        let held = self.held.load(std::sync::atomic::Ordering::SeqCst);
        let locked = self.locked.load(std::sync::atomic::Ordering::SeqCst);
        let (capabilities, missing) = if locked {
            (
                vec![Capability::Terminal],
                vec![MissingCapability {
                    capability: Capability::ScreenRead,
                    reason: LOCKED_REASON.into(),
                }],
            )
        } else if held {
            let reason = "Session 0ld ended, but agent-device couldn't release this computer".to_owned();
            (
                vec![Capability::Terminal],
                vec![MissingCapability {
                    capability: Capability::ScreenRead,
                    reason,
                }],
            )
        } else {
            (
                vec![Capability::ScreenRead, Capability::ScreenCapture, Capability::Terminal],
                vec![],
            )
        };
        Probe {
            os: extend_agent::sysinfo::device_os(),
            os_version: Some("99.0".into()),
            model: Some("Test".into()),
            capabilities,
            missing,
            setup: Setup::complete(),
            agent_device_version: Some("0.21.15".into()),
            online: true,
        }
    }
    async fn run(&self, inv: Invocation<'_>) -> Output {
        match inv.command {
            "screenshot" => {
                let p = inv.workdir.join("screenshot.png");
                std::fs::write(&p, b"\x89PNG fake image bytes").unwrap();
                Output::ok(json!({"width": 10, "height": 5}), "screenshot.png (10x5)").with_file(LocalFile {
                    path: p,
                    name: "screenshot.png".into(),
                    content_type: "image/png".into(),
                    kind: FileKind::Screenshot,
                })
            }
            "wait" => {
                self.events.lock().unwrap().push(format!("wait {}", inv.session_id));
                inv.cancel.cancelled().await;
                self.events.lock().unwrap().push(format!("waited {}", inv.session_id));
                Output::fail("cancelled", "stopped")
            }
            other => Output::ok(
                json!({"ran": other, "args": inv.args}),
                format!("@e1 [button] \"{other}\""),
            ),
        }
    }
    async fn session_started(&self, s: &str) {
        self.started.lock().unwrap().push(s.into());
        self.held.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    async fn session_ended(&self, s: &str) {
        self.ended.lock().unwrap().push(s.into());
        self.events.lock().unwrap().push(format!("ended {s}"));
        if s == "b40" {
            self.held.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

struct Tv;
#[async_trait]
impl Driver for Tv {
    async fn probe(&self) -> Probe {
        Probe {
            os: DeviceOs::SamsungTv,
            os_version: None,
            model: Some("QN90".into()),
            capabilities: vec![Capability::InputRemote, Capability::AppsLaunch],
            missing: vec![],
            setup: Setup::complete(),
            agent_device_version: None,
            online: true,
        }
    }
    async fn run(&self, inv: Invocation<'_>) -> Output {
        Output::ok(json!({"pressed": inv.args}), "pressed on the TV")
    }
}

struct Harness {
    fake: Fake,
    handle: AgentHandle,
    computer: Arc<FakeComputer>,
    _dir: tempfile::TempDir,
    state: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

async fn start(paired: bool) -> Harness {
    start_with(paired, &[]).await
}

async fn start_with_held(held: bool) -> Harness {
    start_inner(true, &[], held).await
}

async fn start_with(paired: bool, closes: &[u16]) -> Harness {
    start_inner(paired, closes, false).await
}

async fn start_inner(paired: bool, closes: &[u16], held: bool) -> Harness {
    start_hosting(paired, closes, held, Arc::new(|_d| Ok(Box::new(Tv) as Box<dyn Driver>))).await
}

async fn start_hosting(paired: bool, closes: &[u16], held: bool, factory: DriverFactory) -> Harness {
    let (fake, addr) = start_fake().await;
    fake.0.lock().unwrap().next_device_close.extend(closes.iter().copied());
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_tests(dir.path(), &format!("http://{addr}"));
    let state = config.state_dir.clone();
    let store = credential::store_for(&config);
    if paired {
        store
            .save(&credential::StoredCredential {
                device_id: DEVICE_ID.parse().unwrap(),
                device_credential: CREDENTIAL.into(),
                service_url: config.service_url.to_string(),
            })
            .unwrap();
    }
    let computer = Arc::new(FakeComputer::default());
    computer.held.store(held, std::sync::atomic::Ordering::SeqCst);
    let (agent, handle) = Agent::new(AgentDeps {
        config,
        local: computer.clone(),
        hosted_factory: factory,
        credentials: store,
        probe_interval: Duration::from_secs(3600),
        screen_watch: screen_watch(&computer),
    });
    let task = tokio::spawn(agent.run());
    Harness {
        fake,
        handle,
        computer,
        _dir: dir,
        state,
        task,
    }
}

async fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..300 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

fn frame_of(fake: &Fake, kind: &str, n: usize) -> Option<Value> {
    fake.frames().into_iter().filter(|f| f["type"] == kind).nth(n)
}

#[tokio::test]
async fn full_life_of_a_paired_computer() {
    let h = start(false).await;
    let fake = h.fake.clone();

    // 1. Enrollment: the app asks for a code without logging in.
    let body = eventually("enrollment", || {
        fake.seen().into_iter().find_map(|s| {
            if let Seen::CreateEnrollment(b) = s {
                Some(b)
            } else {
                None
            }
        })
    })
    .await;
    assert_eq!(body["type"], "enrollment");
    assert_eq!(body["data"]["os"], extend_agent::sysinfo::device_os().as_str());
    assert_eq!(body["data"]["app_version"], extend_agent::config::APP_VERSION);
    eventually("code shown", || {
        (h.handle.status.get().pairing.map(|p| p.code) == Some("4F9C2A".into())).then_some(())
    })
    .await;
    eventually("enrollment socket", || {
        (fake.count(|s| matches!(s, Seen::EnrollmentSocket { auth } if auth == &format!("Extend-Enrollment {SECRET}")))
            == 1)
            .then_some(())
    })
    .await;

    // 2. The code rotates; pings are answered.
    fake.send(json!({"type":"code","pairing_code":"7b21e0","code_expires_at":expires_in(300)}));
    eventually("rotated code", || {
        (h.handle.status.get().pairing.map(|p| p.code) == Some("7B21E0".into())).then_some(())
    })
    .await;
    fake.send(json!({"type":"ping","nonce":17}));
    eventually("pong 17", || {
        fake.frames()
            .into_iter()
            .find(|f| f["type"] == "pong" && f["nonce"] == 17)
    })
    .await;

    // 3. Paired: the credential is stored and the device socket opens with it.
    fake.send(json!({"type":"paired","device_id":DEVICE_ID,"device_credential":CREDENTIAL,
        "environment":{"environment_id":"9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c","name":"checkout-e2e","state":"ready","paired_devices":1,"device_limit":5}}));
    let hello = eventually("hello", || frame_of(&fake, "hello", 0)).await;
    assert_eq!(hello["app_version"], extend_agent::config::APP_VERSION);
    assert_eq!(hello["agent_device_version"], "0.21.15");
    assert_eq!(
        hello["capabilities"],
        json!(["screen.read", "screen.capture", "terminal"])
    );
    assert_eq!(hello["setup"]["state"], "complete");
    let stored = std::fs::read_to_string(h.state.join("credential.json")).unwrap();
    assert!(stored.contains(CREDENTIAL));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(h.state.join("credential.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    // The device socket and GET /api/v1/device both carry the credential.
    assert!(
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::DeviceSocket { auth } if auth == &format!("Extend-Device {CREDENTIAL}")))
    );
    eventually("device details", || {
        let s = h.handle.status.get();
        (s.phase == Phase::Online && s.device.as_ref().and_then(|d| d.owner.clone()) == Some("c:alice".into()))
            .then_some(())
    })
    .await;
    assert!(
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::DeviceSelf { auth } if auth == &format!("Extend-Device {CREDENTIAL}")))
    );
    // The environment from `paired` was replaced by GET /api/v1/device (null): production.
    let status_file: Value = serde_json::from_slice(&std::fs::read(h.state.join("status.json")).unwrap()).unwrap();
    assert_eq!(status_file["pid"], std::process::id());

    // 4. A session starts: the indicator names the Silicon.
    fake.send(
        json!({"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":expires_in(0)}),
    );
    eventually("in use", || {
        h.handle.status.get().in_use.filter(|u| u.silicon_id == "si:chef")
    })
    .await;
    eventually("driver told", || {
        (h.computer.started.lock().unwrap().as_slice() == ["a3f"]).then_some(())
    })
    .await;
    assert!(h.handle.status.get().headline().contains("si:chef is using this"));

    // 5. A command that makes a file: upload first, then the result listing it.
    let upload_id = "0192f000-0000-7000-8000-0000000000aa";
    fake.send(
        json!({"type":"command","id":"0192f000-0000-7000-8000-000000000001","session_id":"a3f","target":null,
        "command":"screenshot","args":[],"attachments":[],"timeout_ms":30000,"upload_ids":[upload_id]}),
    );
    let result = eventually("screenshot result", || frame_of(&fake, "result", 0)).await;
    assert_eq!(result["id"], "0192f000-0000-7000-8000-000000000001");
    assert_eq!(result["ok"], true);
    assert_eq!(result["text"], "screenshot.png (10x5)");
    assert_eq!(result["files"][0]["upload_id"], upload_id);
    assert_eq!(result["files"][0]["kind"], "screenshot");
    assert_eq!(result["files"][0]["content_type"], "image/png");
    assert_eq!(result["files"][0]["size_bytes"], 21);
    let seen = fake.seen();
    let upload_at = seen
        .iter()
        .position(|s| matches!(s, Seen::Upload { .. }))
        .expect("upload");
    let result_at = seen
        .iter()
        .position(|s| matches!(s, Seen::Frame(f) if f["type"] == "result"))
        .unwrap();
    assert!(
        upload_at < result_at,
        "the file must be uploaded before the result is sent"
    );
    match &seen[upload_at] {
        Seen::Upload {
            id,
            name,
            content_type,
            sha_ok,
            bytes,
        } => {
            assert_eq!(id, upload_id);
            assert_eq!(name, "screenshot.png");
            assert_eq!(content_type, "image/png");
            assert!(sha_ok);
            assert_eq!(*bytes, 21);
        }
        _ => unreachable!(),
    }

    // 6. A reserved flag is refused with a precise message; the driver never sees it.
    fake.send(
        json!({"type":"command","id":"0192f000-0000-7000-8000-000000000002","session_id":"a3f","command":"snapshot",
        "args":["-i","--platform","ios"],"timeout_ms":30000,"upload_ids":[]}),
    );
    let r = eventually("refused", || frame_of(&fake, "result", 1)).await;
    assert_eq!(r["ok"], false);
    assert_eq!(r["error"]["code"], "invalid_args");
    assert!(r["error"]["message"].as_str().unwrap().contains("--platform"));

    // 7. Cancel stops a running command.
    fake.send(
        json!({"type":"command","id":"0192f000-0000-7000-8000-000000000003","session_id":"a3f","command":"wait",
        "args":["60000"],"timeout_ms":120000,"upload_ids":[]}),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    fake.send(json!({"type":"cancel","id":"0192f000-0000-7000-8000-000000000003"}));
    let r = eventually("cancelled", || frame_of(&fake, "result", 2)).await;
    assert_eq!(r["id"], "0192f000-0000-7000-8000-000000000003");
    assert_eq!(r["error"]["code"], "cancelled");

    // 8. Stop from the tray goes up the socket.
    h.handle.actions.send(UiAction::Stop { target: None }).unwrap();
    eventually("stop frame", || frame_of(&fake, "stop", 0)).await;

    // 9. Takeover shows the reason; Done goes up the socket.
    fake.send(json!({"type":"takeover","target":null,"session_id":"a3f","reason":"Please approve the admin prompt","expires_at":expires_in(1800)}));
    eventually("takeover", || {
        h.handle
            .status
            .get()
            .takeover
            .filter(|t| t.reason == "Please approve the admin prompt")
    })
    .await;
    h.handle.actions.send(UiAction::TakeoverDone { target: None }).unwrap();
    eventually("takeover_done", || frame_of(&fake, "takeover_done", 0)).await;
    fake.send(json!({"type":"takeover_ended","target":null,"session_id":"a3f"}));
    eventually("takeover cleared", || {
        h.handle.status.get().takeover.is_none().then_some(())
    })
    .await;

    // 10. The session ends: the indicator clears and the driver closes its session.
    fake.send(json!({"type":"session_ended","target":null,"session_id":"a3f","reason":"stopped_by_carbon"}));
    eventually("not in use", || h.handle.status.get().in_use.is_none().then_some(())).await;
    eventually("driver ended", || {
        (h.computer.ended.lock().unwrap().as_slice() == ["a3f"]).then_some(())
    })
    .await;

    // 11. Test environment banner comes and goes with `environment`.
    fake.send(json!({"type":"environment","environment":{"environment_id":"9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c","name":"checkout-e2e","state":"ready"}}));
    eventually("env banner", || {
        h.handle.status.get().environment.filter(|e| e.name == "checkout-e2e")
    })
    .await;
    fake.send(json!({"type":"environment","environment":null}));
    eventually("env cleared", || {
        h.handle.status.get().environment.is_none().then_some(())
    })
    .await;

    // 12. Attach a TV: the host reports it with the TV driver's probe and routes its commands.
    fake.send(
        json!({"type":"attach","device_id":"0000aaaa","os":"samsung_tv","name":"Lounge TV","address":"10.0.0.5"}),
    );
    let attached = eventually("attached", || frame_of(&fake, "attached", 0)).await;
    assert_eq!(attached["device_id"], "0000aaaa");
    assert_eq!(attached["online"], true);
    assert_eq!(attached["capabilities"], json!(["input.remote", "apps.launch"]));
    fake.send(
        json!({"type":"command","id":"0192f000-0000-7000-8000-000000000004","session_id":"b40","target":"0000aaaa",
        "command":"tv-remote","args":["press","home"],"timeout_ms":30000,"upload_ids":[]}),
    );
    let r = eventually("tv result", || frame_of(&fake, "result", 3)).await;
    assert_eq!(r["ok"], true);
    assert_eq!(r["text"], "pressed on the TV");
    fake.send(
        json!({"type":"command","id":"0192f000-0000-7000-8000-000000000005","session_id":"b40","target":"0000bbbb",
        "command":"tv-remote","args":["press","home"],"timeout_ms":30000,"upload_ids":[]}),
    );
    let r = eventually("unknown target", || frame_of(&fake, "result", 4)).await;
    assert_eq!(r["error"]["code"], "unsupported_on_device");

    // 13. `refresh` re-reads the device.
    let before = fake.count(|s| matches!(s, Seen::DeviceSelf { .. }));
    fake.send(json!({"type":"refresh"}));
    eventually("refreshed", || {
        (fake.count(|s| matches!(s, Seen::DeviceSelf { .. })) > before).then_some(())
    })
    .await;

    // 14. Service pings are answered with the same nonce.
    fake.send(json!({"type":"ping","nonce":42}));
    eventually("pong 42", || {
        fake.frames()
            .into_iter()
            .find(|f| f["type"] == "pong" && f["nonce"] == 42)
    })
    .await;

    // 15. Unpaired: the credential is forgotten and a new pairing code appears.
    fake.send(json!({"type":"unpaired","reason":"device_removed"}));
    eventually("second enrollment", || {
        (fake.count(|s| matches!(s, Seen::CreateEnrollment(_))) == 2).then_some(())
    })
    .await;
    eventually("pairing again", || {
        (h.handle.status.get().phase == Phase::Enrolling).then_some(())
    })
    .await;
    assert!(!h.state.join("credential.json").exists());
    assert!(h.handle.status.get().device.is_none());

    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .expect("agent stops")
        .unwrap();
}

#[tokio::test]
async fn revoke_pair_from_the_app() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || frame_of(&fake, "hello", 0)).await;
    h.handle.actions.send(UiAction::RevokePair).unwrap();
    eventually("DELETE /api/v1/device", || {
        fake.seen().iter().any(|s| matches!(s, Seen::Revoke)).then_some(())
    })
    .await;
    eventually("back to pairing", || {
        (fake.count(|s| matches!(s, Seen::CreateEnrollment(_))) == 1).then_some(())
    })
    .await;
    assert!(!h.state.join("credential.json").exists());
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn revoke_pair_from_the_app_stops_a_running_command_before_cleanup() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || frame_of(&fake, "hello", 0)).await;
    fake.send(
        json!({"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":expires_in(0)}),
    );
    fake.send(
        json!({"type":"command","id":"0192f000-0000-7000-8000-000000000009","session_id":"a3f","command":"wait",
        "args":["300000"],"timeout_ms":300000,"upload_ids":[]}),
    );
    eventually("command running", || {
        h.computer
            .events
            .lock()
            .unwrap()
            .contains(&"wait a3f".to_string())
            .then_some(())
    })
    .await;
    // Revoke over HTTP: the service's own session_ended frame is never read on this path.
    h.handle.actions.send(UiAction::RevokePair).unwrap();
    eventually("DELETE /api/v1/device", || {
        fake.seen().iter().any(|s| matches!(s, Seen::Revoke)).then_some(())
    })
    .await;
    eventually("cleanup", || {
        h.computer
            .events
            .lock()
            .unwrap()
            .contains(&"ended a3f".to_string())
            .then_some(())
    })
    .await;
    // The running command was stopped first, and cleanup ran after it, not beside it.
    assert_eq!(
        *h.computer.events.lock().unwrap(),
        ["wait a3f", "waited a3f", "ended a3f"]
    );
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn close_4401_means_unpaired() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || frame_of(&fake, "hello", 0)).await;
    fake.send(json!("__close_4401"));
    eventually("new enrollment", || {
        (fake.count(|s| matches!(s, Seen::CreateEnrollment(_))) == 1).then_some(())
    })
    .await;
    assert!(!h.state.join("credential.json").exists());
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn close_4409_means_superseded_and_no_reconnect() {
    let h = start_with(true, &[4409]).await;
    let fake = h.fake.clone();
    eventually("superseded", || {
        (h.handle.status.get().phase == Phase::Superseded).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        fake.count(|s| matches!(s, Seen::DeviceSocket { .. })),
        1,
        "a superseded app must not reconnect"
    );
    // The Carbon can take the connection back.
    h.handle.actions.send(UiAction::Reconnect).unwrap();
    eventually("hello after reconnect", || frame_of(&fake, "hello", 0)).await;
    assert!(h.state.join("credential.json").exists());
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn close_4426_means_upgrade_required() {
    let h = start_with(true, &[4426]).await;
    eventually("upgrade required", || {
        (h.handle.status.get().phase == Phase::UpgradeRequired).then_some(())
    })
    .await;
    assert!(h.state.join("credential.json").exists());
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

/// The periodic check is an hour apart here, so only the check after a session's setup and
/// cleanup can tell the service what changed: a computer released when a session starts, or
/// held again after one ends.
#[tokio::test]
async fn capabilities_are_checked_again_after_session_setup_and_cleanup() {
    let h = start_with_held(true).await;
    let fake = h.fake.clone();
    let first = eventually("hello", || frame_of(&fake, "hello", 0)).await;
    assert_eq!(first["capabilities"], json!(["terminal"]), "{first}");
    assert_eq!(
        first["setup"]["state"], "complete",
        "a held computer stays ready: {first}"
    );
    fake.send(
        json!({"type":"session_started","target":null,"session_id":"b40","silicon_id":"si:chef","since":expires_in(0)}),
    );
    let released = eventually("a hello after the session's setup", || frame_of(&fake, "hello", 1)).await;
    assert_eq!(
        released["capabilities"],
        json!(["screen.read", "screen.capture", "terminal"]),
        "{released}"
    );
    fake.send(json!({"type":"session_ended","target":null,"session_id":"b40","reason":"stopped_by_carbon"}));
    let held = eventually("a hello after the session's cleanup", || frame_of(&fake, "hello", 2)).await;
    assert_eq!(held["capabilities"], json!(["terminal"]), "{held}");
    assert!(
        held["missing"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("couldn't release"),
        "{held}"
    );
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_dropped_socket_reconnects_and_says_hello_again() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || frame_of(&fake, "hello", 0)).await;
    fake.send(json!("__drop"));
    // Backoff starts at up to 1 s with jitter.
    let second = async {
        loop {
            if frame_of(&fake, "hello", 1).is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .expect("reconnected within 10 s");
    assert_eq!(fake.count(|s| matches!(s, Seen::DeviceSocket { .. })), 2);
    // Stop while connected again still works.
    h.handle.actions.send(UiAction::Stop { target: None }).unwrap();
    eventually("stop", || frame_of(&fake, "stop", 0)).await;
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_refused_credential_on_connect_means_unpaired() {
    let (fake, addr) = start_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_tests(dir.path(), &format!("http://{addr}"));
    let store = credential::store_for(&config);
    store
        .save(&credential::StoredCredential {
            device_id: DEVICE_ID.parse().unwrap(),
            device_credential: "edc_wrongwrongwrongwrongwrongwrongwrongwrongwro".into(),
            service_url: config.service_url.to_string(),
        })
        .unwrap();
    let state = config.state_dir.clone();
    let (agent, handle) = Agent::new(AgentDeps {
        config,
        local: Arc::new(FakeComputer::default()),
        hosted_factory: Arc::new(|_d| Err("none".into())),
        credentials: store,
        probe_interval: Duration::from_secs(3600),
        screen_watch: None,
    });
    let task = tokio::spawn(agent.run());
    eventually("enrollment after 401", || {
        (fake.count(|s| matches!(s, Seen::CreateEnrollment(_))) == 1).then_some(())
    })
    .await;
    assert!(!state.join("credential.json").exists());
    handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
}

/// Locking or unlocking the screen reaches Extend within seconds, not at the next periodic check
/// (an hour here).
#[tokio::test]
async fn locking_and_unlocking_the_screen_is_reported_at_once() {
    let h = start(true).await;
    let fake = h.fake.clone();
    let hello = eventually("hello", || frame_of(&fake, "hello", 0)).await;
    assert_eq!(
        hello["capabilities"],
        json!(["screen.read", "screen.capture", "terminal"])
    );
    h.computer.locked.store(true, std::sync::atomic::Ordering::SeqCst);
    let locked = eventually("hello after locking", || frame_of(&fake, "hello", 1)).await;
    assert_eq!(locked["capabilities"], json!(["terminal"]));
    assert_eq!(locked["missing"][0]["reason"], LOCKED_REASON);
    eventually("status shows the lock", || {
        h.handle
            .status
            .get()
            .missing
            .iter()
            .any(|m| m.reason == LOCKED_REASON)
            .then_some(())
    })
    .await;
    h.computer.locked.store(false, std::sync::atomic::Ordering::SeqCst);
    let unlocked = eventually("hello after unlocking", || frame_of(&fake, "hello", 2)).await;
    assert_eq!(
        unlocked["capabilities"],
        json!(["screen.read", "screen.capture", "terminal"])
    );
    h.handle.shutdown.cancel();
    let _ = h.task.await;
}

/// A carried TV that notes each session end it is told about, by device (and each note that a
/// session is live though no command runs).
struct EndsTv {
    device: String,
    ends: Arc<Mutex<Vec<(String, String)>>>,
    actives: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl Driver for EndsTv {
    async fn probe(&self) -> Probe {
        Tv.probe().await
    }
    async fn run(&self, inv: Invocation<'_>) -> Output {
        Tv.run(inv).await
    }
    async fn session_ended(&self, session_id: &str) {
        self.ends
            .lock()
            .unwrap()
            .push((self.device.clone(), session_id.to_owned()));
    }
    async fn session_active(&self, session_id: &str) {
        self.actives
            .lock()
            .unwrap()
            .push((self.device.clone(), session_id.to_owned()));
    }
}

#[tokio::test]
async fn sessions_that_ended_while_away_are_ended_on_the_devices_carried() {
    let ends: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let actives: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let (log, active_log) = (ends.clone(), actives.clone());
    let factory: DriverFactory = Arc::new(move |d| {
        Ok(Box::new(EndsTv {
            device: d.device_id.clone(),
            ends: log.clone(),
            actives: active_log.clone(),
        }) as Box<dyn Driver>)
    });
    let h = start_hosting(true, &[], false, factory).await;
    let fake = h.fake.clone();
    let ended = |device: &str| -> Vec<String> {
        ends.lock()
            .unwrap()
            .iter()
            .filter(|(d, _)| d == device)
            .map(|(_, s)| s.clone())
            .collect()
    };
    let in_use = |device: &str| -> Option<String> {
        h.handle
            .status
            .get()
            .attached
            .iter()
            .find(|a| a.device_id == device)
            .and_then(|a| a.in_use.as_ref().map(|u| u.session_id.clone()))
    };
    // The service's greeting: two carried TVs, each with a live session, then its first ping.
    let greet = |sessions: &[(&str, &str)]| {
        for d in ["0000aaaa", "0000bbbb"] {
            fake.send(json!({"type":"attach","device_id":d,"os":"samsung_tv","name":d,"address":"10.0.0.5"}));
            for (device, session) in sessions {
                if *device == d {
                    fake.send(json!({"type":"session_started","target":d,"session_id":session,"silicon_id":"si:chef","since":"2026-09-27T10:00:00Z"}));
                }
            }
        }
        fake.send(json!({"type":"ping","nonce":1}));
    };
    eventually("hello", || frame_of(&fake, "hello", 0)).await;
    greet(&[("0000aaaa", "a1f"), ("0000bbbb", "b2e")]);
    eventually("both in use", || {
        (in_use("0000aaaa").is_some() && in_use("0000bbbb").is_some()).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ends.lock().unwrap().is_empty(), "announced sessions are left alone");

    // Away for a while: the service ended a1f meanwhile, so its greeting announces only b2e.
    fake.send(json!("__drop"));
    eventually("reconnected", || frame_of(&fake, "hello", 1)).await;
    eventually("socket", || {
        (fake.count(|s| matches!(s, Seen::DeviceSocket { .. })) == 2).then_some(())
    })
    .await;
    greet(&[("0000bbbb", "b2e")]);
    eventually("a1f ended on its TV", || {
        (ended("0000aaaa") == ["a1f", ""]).then_some(())
    })
    .await;
    assert_eq!(in_use("0000aaaa"), None);
    assert_eq!(in_use("0000bbbb").as_deref(), Some("b2e"));
    assert!(ended("0000bbbb").is_empty());

    // A takeover's start and end tell the device's driver its session is still live.
    fake.send(json!({"type":"takeover","target":"0000bbbb","session_id":"b2e","reason":"Face ID","expires_at":"2026-09-27T10:30:00Z"}));
    fake.send(json!({"type":"takeover_ended","target":"0000bbbb","session_id":"b2e"}));
    eventually("takeover noted", || (actives.lock().unwrap().len() == 2).then_some(())).await;
    assert_eq!(
        *actives.lock().unwrap(),
        [
            ("0000bbbb".to_owned(), "b2e".to_owned()),
            ("0000bbbb".to_owned(), "b2e".to_owned())
        ]
    );

    // A greeting that attaches no device (the service couldn't read them) ends nothing.
    fake.send(json!("__drop"));
    eventually("reconnected again", || frame_of(&fake, "hello", 2)).await;
    eventually("third socket", || {
        (fake.count(|s| matches!(s, Seen::DeviceSocket { .. })) == 3).then_some(())
    })
    .await;
    let pongs = || fake.count(|s| matches!(s, Seen::Frame(f) if f["type"] == "pong"));
    let before = pongs();
    fake.send(json!({"type":"ping","nonce":1}));
    fake.send(json!({"type":"ping","nonce":2}));
    eventually("pongs", || (pongs() >= before + 2).then_some(())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ended("0000bbbb").is_empty());
    assert_eq!(in_use("0000bbbb").as_deref(), Some("b2e"));

    // Quitting closes the carried device's live session too.
    h.handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(15), h.task)
        .await
        .expect("agent stops")
        .unwrap();
    assert_eq!(ended("0000bbbb"), ["b2e"]);
}
