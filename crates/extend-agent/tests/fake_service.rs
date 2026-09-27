//! End-to-end against an in-process fake Extend service (axum HTTP + WebSockets), following
//! `docs/device-protocol.md` exactly: enrollment → code rotation → paired → hello → commands and
//! uploads → sessions → takeover → attach → unpaired → a fresh enrollment; and the close codes.
//! Then 1.1: several Carbons' pairs on one computer (one connection each), "Pair with another
//! Carbon", revoking one pair, credential rotation, `awake`, wake requests and their redaction,
//! keeping the display on, the terminal rule for shared computers, and setup retries.

use std::collections::{HashMap, VecDeque};
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
use extend_agent::credential::{self, CredentialStore};
use extend_agent::drivers::screen_lock::{ScreenBlock, ScreenReading};
use extend_agent::hosted::DriverFactory;
use extend_agent::status::{PairPhase, Phase};
use extend_driver::{Driver, Invocation, LocalFile, Output, Probe};
use extend_protocol::model::{FileKind, MissingCapability, Setup};
use extend_protocol::{Capability, DeviceOs};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc;

const CREDENTIAL: &str = "edc_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const SECRET: &str = "ees_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const DEVICE_ID: &str = "7c1e09ab";
/// A second Carbon's pair of the same computer.
const CREDENTIAL_B: &str = "edc_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
const DEVICE_B: &str = "0d44e1f2";
/// A rotated credential for the first pair.
const CREDENTIAL_ROTATED: &str = "edc_DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD";

/// Everything the fake service saw, in order.
#[derive(Debug, Clone)]
enum Seen {
    CreateEnrollment(Value),
    /// POST /api/v1/device/enrollments ("Pair with another Carbon"), with the credential used.
    PairEnrollment {
        auth: String,
    },
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
    Revoke {
        auth: String,
    },
    Stop,
    /// A frame from the app; `__pair` names the pair's connection it came on.
    Frame(Value),
}

struct Inner {
    seen: Vec<Seen>,
    /// Open sockets by key (a pair's device id, or "enroll"), and the newest one's key.
    sockets: HashMap<String, mpsc::UnboundedSender<String>>,
    last_socket: Option<String>,
    /// Close codes to use on the next device-socket connections (0 = none).
    next_device_close: VecDeque<u16>,
    enrollments: u32,
    /// The credentials the fake accepts, and the pair each belongs to.
    credentials: HashMap<String, String>,
    /// `GET /api/v1/device` for each pair.
    selves: HashMap<String, Value>,
}

impl Default for Inner {
    fn default() -> Self {
        let mut credentials = HashMap::new();
        credentials.insert(CREDENTIAL.to_owned(), DEVICE_ID.to_owned());
        credentials.insert(CREDENTIAL_B.to_owned(), DEVICE_B.to_owned());
        let mut selves = HashMap::new();
        selves.insert(
            DEVICE_ID.to_owned(),
            device_self_json(DEVICE_ID, "Test Mac", "c:alice", true),
        );
        selves.insert(
            DEVICE_B.to_owned(),
            device_self_json(DEVICE_B, "Family Mac", "c:bob", false),
        );
        Self {
            seen: vec![],
            sockets: HashMap::new(),
            last_socket: None,
            next_device_close: VecDeque::new(),
            enrollments: 0,
            credentials,
            selves,
        }
    }
}

fn device_self_json(id: &str, name: &str, owner: &str, first_pair: bool) -> Value {
    json!({
        "device_id": id, "name": name, "owner": {"type":"carbon","id":owner},
        "team": "acme", "os": "macos", "in_use": null, "takeover": null,
        "setup": {"state":"complete","steps":[]}, "environment": null,
        "instance_id": "0192f3a4-5b6c-7d8e-9f00-112233445566",
        "hardware_salt": "world-salt", "first_pair": first_pair
    })
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
    /// Sends to the newest socket (the enrollment socket before pairing, the pair's after).
    fn send(&self, frame: Value) {
        let key = self.0.lock().unwrap().last_socket.clone().expect("a socket is open");
        self.send_to(&key, frame);
    }
    /// Sends to one pair's socket ("enroll" for the enrollment socket).
    fn send_to(&self, key: &str, frame: Value) {
        let tx = self
            .0
            .lock()
            .unwrap()
            .sockets
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("no socket open for {key}"));
        // A bare string is a control word for the relay ("__drop", "__close_4401").
        let text = match frame {
            Value::String(s) => s,
            other => other.to_string(),
        };
        tx.send(text).unwrap();
    }
    fn connected(&self, key: &str) -> bool {
        self.0.lock().unwrap().sockets.contains_key(key)
    }
    fn frames(&self) -> Vec<Value> {
        self.seen()
            .into_iter()
            .filter_map(|s| if let Seen::Frame(v) = s { Some(v) } else { None })
            .collect()
    }
    /// The frames that came on one pair's connection.
    fn frames_on(&self, pair: &str) -> Vec<Value> {
        self.frames().into_iter().filter(|f| f["__pair"] == pair).collect()
    }
    fn count(&self, f: impl Fn(&Seen) -> bool) -> usize {
        self.seen().iter().filter(|s| f(s)).count()
    }
    fn accept(&self, credential: &str, pair: &str) {
        self.0
            .lock()
            .unwrap()
            .credentials
            .insert(credential.to_owned(), pair.to_owned());
    }
}

fn auth(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

/// The pair a device credential belongs to, when the fake accepts it.
fn pair_of(f: &Fake, h: &HeaderMap) -> Option<String> {
    let a = auth(h);
    let cred = a.strip_prefix("Extend-Device ")?;
    f.0.lock().unwrap().credentials.get(cred).cloned()
}

async fn create_enrollment(State(f): State<Fake>, Json(body): Json<Value>) -> Response {
    f.push(Seen::CreateEnrollment(body));
    enrollment_created(&f)
}

fn enrollment_created(f: &Fake) -> Response {
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

/// "Pair with another Carbon": a code for this device, with a live pair's credential.
async fn pair_enrollment(State(f): State<Fake>, headers: HeaderMap) -> Response {
    f.push(Seen::PairEnrollment { auth: auth(&headers) });
    if pair_of(&f, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    enrollment_created(&f)
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
    ws.on_upgrade(move |socket| relay(f, socket, None, "enroll".into()))
}

async fn device_socket(State(f): State<Fake>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let Some(pair) = pair_of(&f, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    f.push(Seen::DeviceSocket { auth: auth(&headers) });
    let close = f.0.lock().unwrap().next_device_close.pop_front();
    ws.on_upgrade(move |socket| relay(f, socket, close, pair))
}

/// Records every frame from the app and pushes the test's frames to it.
async fn relay(f: Fake, mut socket: WebSocket, close_with: Option<u16>, key: String) {
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
    {
        let mut g = f.0.lock().unwrap();
        g.sockets.insert(key.clone(), tx.clone());
        g.last_socket = Some(key.clone());
    }
    loop {
        tokio::select! {
            m = socket.recv() => match m {
                Some(Ok(WsMessage::Text(t))) => {
                    let mut v: Value = serde_json::from_str(t.as_str()).unwrap();
                    v["__pair"] = key.clone().into();
                    f.push(Seen::Frame(v));
                }
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            out = rx.recv() => match out {
                Some(text) if text == "__close_4401" => {
                    let _ = socket.send(WsMessage::Close(Some(axum::extract::ws::CloseFrame { code: 4401, reason: "unpaired".into() }))).await;
                    break;
                }
                Some(text) if text == "__close_4409" => {
                    let _ = socket.send(WsMessage::Close(Some(axum::extract::ws::CloseFrame { code: 4409, reason: "superseded".into() }))).await;
                    break;
                }
                Some(text) if text == "__drop" => break,
                Some(text) => { if socket.send(WsMessage::Text(text.into())).await.is_err() { break; } }
                None => break,
            }
        }
    }
    let mut g = f.0.lock().unwrap();
    if g.sockets.get(&key).is_some_and(|t| t.same_channel(&tx)) {
        g.sockets.remove(&key);
    }
}

async fn device_self(State(f): State<Fake>, headers: HeaderMap) -> Response {
    f.push(Seen::DeviceSelf { auth: auth(&headers) });
    let Some(pair) = pair_of(&f, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let data = f.0.lock().unwrap().selves.get(&pair).cloned().unwrap();
    Json(json!({"type":"device_self","data":data})).into_response()
}

async fn revoke(State(f): State<Fake>, headers: HeaderMap) -> StatusCode {
    f.push(Seen::Revoke { auth: auth(&headers) });
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
    if pair_of(&f, &headers).is_none() {
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
        .route("/api/v1/device/enrollments", post(pair_enrollment))
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
    /// Seconds since the last input, as the screen watch reads it.
    idle_s: std::sync::atomic::AtomicU64,
    /// The wake notification on screen, read when each command runs (the redaction check).
    notifier: Mutex<Option<Arc<extend_agent::notify::Recorder>>>,
    at_command: Mutex<Vec<Option<extend_agent::notify::Notification>>>,
    /// Setup steps the Carbon asked to retry.
    retried: Mutex<Vec<Option<String>>>,
}

const LOCKED_REASON: &str = "This computer is locked. Unlock it to let a Silicon use it.";

/// The agent's screen watch, reading the fake computer's lock.
fn screen_watch(computer: &Arc<FakeComputer>) -> Option<extend_agent::agent::ScreenWatch> {
    let computer = computer.clone();
    Some(Arc::new(move || ScreenReading {
        block: computer
            .locked
            .load(std::sync::atomic::Ordering::SeqCst)
            .then_some(ScreenBlock::Locked),
        input_idle: Some(Duration::from_secs(
            computer.idle_s.load(std::sync::atomic::Ordering::SeqCst),
        )),
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
            let reason = "Session 0ld ended, but the device engine couldn't release this computer".to_owned();
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
            engine_version: Some("0.21.15".into()),
            online: true,
            awake: None,
            sleep_state: None,
            hardware_id: None,
        }
    }
    async fn run(&self, inv: Invocation<'_>) -> Output {
        let shown = self.notifier.lock().unwrap().as_ref().map(|r| r.current());
        if let Some(shown) = shown {
            self.at_command.lock().unwrap().push(shown);
        }
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
            "clipboard" => {
                // Leaves a file behind in its session's scratch space.
                std::fs::write(inv.workdir.join("left-behind.txt"), b"x").unwrap();
                Output::ok(json!({"workdir": inv.workdir}), inv.workdir.display().to_string())
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
    async fn retry_setup(&self, step: Option<&str>) {
        self.retried.lock().unwrap().push(step.map(str::to_owned));
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
            engine_version: None,
            online: true,
            awake: Some(true),
            sleep_state: None,
            hardware_id: None,
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
    notifier: Arc<extend_agent::notify::Recorder>,
    display: Arc<extend_agent::display::Recorder>,
    store: Arc<dyn CredentialStore>,
    _dir: tempfile::TempDir,
    state: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn stop(self) {
        self.handle.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(15), self.task)
            .await
            .expect("agent stops")
            .unwrap();
    }
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
    let pairs: &[(&str, &str, Option<bool>)] = if paired { &[(DEVICE_ID, CREDENTIAL, None)] } else { &[] };
    start_full(pairs, closes, held, factory).await
}

/// Two Carbons' pairs of one computer: alice's (the first) and bob's.
async fn start_shared() -> Harness {
    start_full(
        &[
            (DEVICE_ID, CREDENTIAL, Some(true)),
            (DEVICE_B, CREDENTIAL_B, Some(false)),
        ],
        &[],
        false,
        Arc::new(|_d| Ok(Box::new(Tv) as Box<dyn Driver>)),
    )
    .await
}

async fn start_full(
    pairs: &[(&str, &str, Option<bool>)],
    closes: &[u16],
    held: bool,
    factory: DriverFactory,
) -> Harness {
    let (fake, addr) = start_fake().await;
    fake.0.lock().unwrap().next_device_close.extend(closes.iter().copied());
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_tests(dir.path(), &format!("http://{addr}"));
    let state = config.state_dir.clone();
    let store = credential::store_for(&config);
    for (id, cred, first) in pairs {
        let mut c = credential::StoredCredential::new(id.parse().unwrap(), (*cred).into(), &config.service_url);
        c.first_pair = *first;
        store.save(&c).unwrap();
    }
    let computer = Arc::new(FakeComputer::default());
    computer.held.store(held, std::sync::atomic::Ordering::SeqCst);
    computer.idle_s.store(600, std::sync::atomic::Ordering::SeqCst);
    let notifier = Arc::new(extend_agent::notify::Recorder::default());
    *computer.notifier.lock().unwrap() = Some(notifier.clone());
    let display = Arc::new(extend_agent::display::Recorder::default());
    let (agent, handle) = Agent::new(AgentDeps {
        config,
        local: computer.clone(),
        hosted_factory: factory,
        credentials: store.clone(),
        probe_interval: Duration::from_secs(3600),
        screen_watch: screen_watch(&computer),
        notifier: notifier.clone(),
        display: display.clone(),
    });
    let task = tokio::spawn(agent.run());
    Harness {
        fake,
        handle,
        computer,
        notifier,
        display,
        store,
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
    assert_eq!(hello["engine_version"], "0.21.15");
    assert!(hello.get("agent_device_version").is_none(), "{hello}");
    assert_eq!(hello["features"], json!(["setup_retry"]));
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
        (s.phase == Phase::Online && s.pairs.first().and_then(|d| d.owner.clone()) == Some("c:alice".into()))
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
    assert!(h.handle.status.get().pairs.is_empty());

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
    h.handle
        .actions
        .send(UiAction::RevokePair {
            device_id: DEVICE_ID.parse().unwrap(),
        })
        .unwrap();
    eventually("DELETE /api/v1/device", || {
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::Revoke { .. }))
            .then_some(())
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
    h.handle
        .actions
        .send(UiAction::RevokePair {
            device_id: DEVICE_ID.parse().unwrap(),
        })
        .unwrap();
    eventually("DELETE /api/v1/device", || {
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::Revoke { .. }))
            .then_some(())
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
    h.handle.actions.send(UiAction::Reconnect { device_id: None }).unwrap();
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
        .save(&credential::StoredCredential::new(
            DEVICE_ID.parse().unwrap(),
            "edc_wrongwrongwrongwrongwrongwrongwrongwrongwro".into(),
            &config.service_url,
        ))
        .unwrap();
    let state = config.state_dir.clone();
    let (agent, handle) = Agent::new(AgentDeps {
        config,
        local: Arc::new(FakeComputer::default()),
        hosted_factory: Arc::new(|_d| Err("none".into())),
        credentials: store,
        probe_interval: Duration::from_secs(3600),
        screen_watch: None,
        notifier: Arc::new(extend_agent::notify::Silent("none".into())),
        display: Arc::new(extend_agent::display::NoKeeper),
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
        Probe {
            // Each TV is its own device.
            hardware_id: Some(format!("duid-{}", self.device)),
            ..Tv.probe().await
        }
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

// ───────────── 1.1: several Carbons, waking, rotation, retries ─────────────

fn hello_on(fake: &Fake, pair: &str, n: usize) -> Option<Value> {
    fake.frames_on(pair).into_iter().filter(|f| f["type"] == "hello").nth(n)
}

fn frames_of(fake: &Fake, pair: &str, kind: &str) -> Vec<Value> {
    fake.frames_on(pair).into_iter().filter(|f| f["type"] == kind).collect()
}

fn caps(hello: &Value) -> Vec<String> {
    serde_json::from_value(hello["capabilities"].clone()).unwrap()
}

#[tokio::test]
async fn two_pairs_run_two_connections_and_only_the_first_gets_the_terminal() {
    let h = start_shared().await;
    let fake = h.fake.clone();
    let alice = eventually("alice's hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    let bob = eventually("bob's hello", || hello_on(&fake, DEVICE_B, 0)).await;
    for hello in [&alice, &bob] {
        assert_eq!(hello["features"], json!(["setup_retry"]), "{hello}");
        assert_eq!(hello["engine_version"], "0.21.15");
    }
    // Alice installed Silicon Extend here: her Silicons get the terminal. Bob's don't, and are
    // told why without naming anyone.
    assert!(caps(&alice).contains(&"terminal".to_owned()), "{alice}");
    let bob = eventually("bob's hello without the terminal", || {
        fake.frames_on(DEVICE_B)
            .into_iter()
            .rfind(|f| f["type"] == "hello")
            .filter(|h| !caps(h).contains(&"terminal".to_owned()))
    })
    .await;
    let terminal = bob["missing"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["capability"] == "terminal")
        .expect("the terminal is reported missing");
    assert_eq!(terminal["reason"], extend_protocol::TERMINAL_NOT_SHARED_REASON);
    assert!(caps(&bob).contains(&"screen.read".to_owned()));

    // Each connection says whether the computer is awake right after hello; one app, one run,
    // rising numbers across both connections.
    let a = eventually("alice's awake", || {
        frames_of(&fake, DEVICE_ID, "awake").first().cloned()
    })
    .await;
    let b = eventually("bob's awake", || frames_of(&fake, DEVICE_B, "awake").first().cloned()).await;
    assert_eq!(a["run"], b["run"]);
    assert_ne!(a["seq"], b["seq"]);
    assert_eq!(a["awake"], true);

    let s = eventually("both pairs online", || {
        let s = h.handle.status.get();
        (s.pairs.len() == 2
            && s.pairs
                .iter()
                .all(|p| p.phase == PairPhase::Online && p.owner.is_some()))
        .then_some(s)
    })
    .await;
    assert_eq!(s.phase, Phase::Online);
    assert_eq!(s.owners(), vec!["c:alice".to_owned(), "c:bob".to_owned()]);
    assert_eq!(s.headline(), "Paired to c:alice and c:bob");
    assert!(!s.pair(DEVICE_ID).unwrap().terminal_withheld);
    assert!(s.pair(DEVICE_B).unwrap().terminal_withheld);

    // A terminal command through bob's pair is refused on the device too; through alice's it runs.
    fake.send_to(
        DEVICE_B,
        json!({"type":"command","id":"0192f000-0000-7000-8000-0000000000b1","session_id":"b01","command":"terminal",
        "args":["run","id"],"timeout_ms":30000,"upload_ids":[]}),
    );
    let refused = eventually("bob's terminal refused", || {
        frames_of(&fake, DEVICE_B, "result").first().cloned()
    })
    .await;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "unsupported_on_device");
    assert_eq!(refused["error"]["message"], extend_protocol::TERMINAL_NOT_SHARED_REASON);
    fake.send_to(
        DEVICE_ID,
        json!({"type":"command","id":"0192f000-0000-7000-8000-0000000000a1","session_id":"a01","command":"terminal",
        "args":["run","id"],"timeout_ms":30000,"upload_ids":[]}),
    );
    let ran = eventually("alice's terminal", || {
        frames_of(&fake, DEVICE_ID, "result").first().cloned()
    })
    .await;
    assert_eq!(ran["ok"], true, "{ran}");

    // A command's result and files go back through the pair it came through, with its credential.
    let upload_id = "0192f000-0000-7000-8000-0000000000bb";
    fake.send_to(
        DEVICE_B,
        json!({"type":"command","id":"0192f000-0000-7000-8000-0000000000b2","session_id":"b01","command":"screenshot",
        "args":[],"timeout_ms":30000,"upload_ids":[upload_id]}),
    );
    let shot = eventually("bob's screenshot", || {
        frames_of(&fake, DEVICE_B, "result").get(1).cloned()
    })
    .await;
    assert_eq!(shot["ok"], true, "{shot}");
    assert_eq!(shot["files"][0]["upload_id"], upload_id);
    assert!(
        frames_of(&fake, DEVICE_ID, "result").len() == 1,
        "nothing of bob's went to alice"
    );
    h.stop().await;
}

/// A carried TV that notes each session end it is told about, and each setup retry.
struct CountingTv {
    device: String,
    ends: Log<(String, String)>,
    retries: Log<(String, Option<String>)>,
}

#[async_trait]
impl Driver for CountingTv {
    async fn probe(&self) -> Probe {
        Probe {
            hardware_id: Some(format!("duid-{}", self.device)),
            ..Tv.probe().await
        }
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
    async fn retry_setup(&self, step: Option<&str>) {
        self.retries
            .lock()
            .unwrap()
            .push((self.device.clone(), step.map(str::to_owned)));
    }
}

type Log<T> = Arc<Mutex<Vec<T>>>;

type Counting = (DriverFactory, Log<(String, String)>, Log<(String, Option<String>)>);

fn counting_factory() -> Counting {
    let ends: Log<(String, String)> = Arc::default();
    let retries: Log<(String, Option<String>)> = Arc::default();
    let (e, r) = (ends.clone(), retries.clone());
    let factory: DriverFactory = Arc::new(move |d| {
        Ok(Box::new(CountingTv {
            device: d.device_id.clone(),
            ends: e.clone(),
            retries: r.clone(),
        }) as Box<dyn Driver>)
    });
    (factory, ends, retries)
}

#[tokio::test]
async fn one_pair_reconnecting_leaves_the_other_pairs_carried_session_alone() {
    let (factory, ends, _) = counting_factory();
    let h = start_full(
        &[
            (DEVICE_ID, CREDENTIAL, Some(true)),
            (DEVICE_B, CREDENTIAL_B, Some(false)),
        ],
        &[],
        false,
        factory,
    )
    .await;
    let fake = h.fake.clone();
    eventually("alice's hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    eventually("bob's hello", || hello_on(&fake, DEVICE_B, 0)).await;
    // Alice's pair carries her TV, with a live session; bob's carries his.
    fake.send_to(
        DEVICE_ID,
        json!({"type":"attach","device_id":"0000aaaa","os":"samsung_tv","name":"Alice's TV","address":"10.0.0.5"}),
    );
    fake.send_to(DEVICE_ID, json!({"type":"session_started","target":"0000aaaa","session_id":"a1f","silicon_id":"si:chef","since":"2026-09-27T10:00:00Z","side":"sideA"}));
    fake.send_to(DEVICE_ID, json!({"type":"ping","nonce":1}));
    fake.send_to(
        DEVICE_B,
        json!({"type":"attach","device_id":"0000bbbb","os":"lg_tv","name":"Bob's TV","address":"10.0.0.9"}),
    );
    fake.send_to(DEVICE_B, json!({"type":"ping","nonce":1}));
    let in_use = |device: &str| {
        h.handle
            .status
            .get()
            .attached
            .iter()
            .find(|a| a.device_id == device)
            .and_then(|a| a.in_use.as_ref().map(|u| u.session_id.clone()))
    };
    eventually("alice's TV in use", || in_use("0000aaaa")).await;
    // Each TV's `attached` goes on its own Carbon's connection.
    eventually("attached on each pair", || {
        let a = frames_of(&fake, DEVICE_ID, "attached");
        let b = frames_of(&fake, DEVICE_B, "attached");
        (a.iter().any(|f| f["device_id"] == "0000aaaa") && b.iter().any(|f| f["device_id"] == "0000bbbb")).then_some(())
    })
    .await;
    assert!(
        !frames_of(&fake, DEVICE_B, "attached")
            .iter()
            .any(|f| f["device_id"] == "0000aaaa")
    );
    // Once the salt is known, each reports a key, never the raw id.
    let keyed = eventually("a hardware key", || {
        frames_of(&fake, DEVICE_ID, "attached")
            .into_iter()
            .find(|f| f["hardware_key"].is_string())
    })
    .await;
    assert_eq!(keyed["hardware_key"].as_str().unwrap().len(), 64);
    assert!(!keyed.to_string().contains("duid-"), "{keyed}");

    // Bob's connection drops and comes back; its greeting announces no session on bob's TV.
    fake.send_to(DEVICE_B, json!("__drop"));
    eventually("bob reconnected", || hello_on(&fake, DEVICE_B, 1)).await;
    eventually("bob's socket", || fake.connected(DEVICE_B).then_some(())).await;
    fake.send_to(
        DEVICE_B,
        json!({"type":"attach","device_id":"0000bbbb","os":"lg_tv","name":"Bob's TV","address":"10.0.0.9"}),
    );
    fake.send_to(DEVICE_B, json!({"type":"ping","nonce":2}));
    eventually("bob's greeting handled", || {
        ends.lock()
            .unwrap()
            .iter()
            .any(|(d, s)| d == "0000bbbb" && s.is_empty())
            .then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Alice's TV session is untouched: bob's greeting only speaks for bob's devices.
    assert!(
        !ends.lock().unwrap().iter().any(|(d, _)| d == "0000aaaa"),
        "{:?}",
        ends.lock().unwrap()
    );
    assert_eq!(in_use("0000aaaa").as_deref(), Some("a1f"));
    h.stop().await;
}

#[tokio::test]
async fn pairing_with_another_carbon_adds_a_second_pair() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    eventually("online", || {
        (h.handle.status.get().phase == Phase::Online).then_some(())
    })
    .await;
    h.handle.actions.send(UiAction::PairAnother).unwrap();
    // The code comes from the device's own credential, and shows in its own card.
    eventually("POST /device/enrollments", || {
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::PairEnrollment { auth } if auth == &format!("Extend-Device {CREDENTIAL}")))
            .then_some(())
    })
    .await;
    eventually("the code for another Carbon", || {
        h.handle
            .status
            .get()
            .adding_pair
            .and_then(|a| a.pairing)
            .filter(|p| p.code == "4F9C2A")
    })
    .await;
    // The computer stays paired to alice meanwhile.
    assert_eq!(h.handle.status.get().phase, Phase::Online);
    assert!(h.handle.status.get().pairing.is_none());
    eventually("enrollment socket", || fake.connected("enroll").then_some(())).await;
    // Bob enters it: a second pair, with its own credential and connection.
    fake.send_to(
        "enroll",
        json!({"type":"paired","device_id":DEVICE_B,"device_credential":CREDENTIAL_B,"environment":null}),
    );
    let bob = eventually("bob's hello", || hello_on(&fake, DEVICE_B, 0)).await;
    let s = eventually("two pairs", || {
        let s = h.handle.status.get();
        (s.pairs.len() == 2 && s.adding_pair.is_none() && s.pairs.iter().all(|p| p.phase == PairPhase::Online))
            .then_some(s)
    })
    .await;
    assert!(s.pair(DEVICE_B).unwrap().terminal_withheld);
    assert!(
        !caps(&bob).contains(&"terminal".to_owned()) || {
            // The first hello may go before the new pair counts; the next one withholds it.
            eventually_sync(|| hello_on(&fake, DEVICE_B, 1).filter(|h| !caps(h).contains(&"terminal".to_owned())))
        }
    );
    let stored = h.store.load_all().unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].device_id.as_str(), DEVICE_B);
    assert_eq!(stored[1].first_pair, Some(false));
    assert_eq!(stored[0].first_pair, Some(true));
    // Alice's hello keeps the terminal.
    let last_alice = fake
        .frames_on(DEVICE_ID)
        .into_iter()
        .rfind(|f| f["type"] == "hello")
        .unwrap();
    assert!(caps(&last_alice).contains(&"terminal".to_owned()));

    // Cancelling another code takes it down.
    h.handle.actions.send(UiAction::PairAnother).unwrap();
    eventually("a second code", || {
        h.handle.status.get().adding_pair.and_then(|a| a.pairing)
    })
    .await;
    h.handle.actions.send(UiAction::CancelPairAnother).unwrap();
    eventually("code gone", || {
        h.handle.status.get().adding_pair.is_none().then_some(())
    })
    .await;
    h.stop().await;
}

/// Polls `f` for a few seconds from sync code inside an assertion.
fn eventually_sync(mut f: impl FnMut() -> Option<Value>) -> bool {
    for _ in 0..100 {
        if f().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

#[tokio::test]
async fn revoking_one_pair_keeps_the_other() {
    let h = start_shared().await;
    let fake = h.fake.clone();
    eventually("alice's hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    eventually("bob's hello", || hello_on(&fake, DEVICE_B, 0)).await;
    eventually("both online", || {
        let s = h.handle.status.get();
        (s.pairs.len() == 2 && s.pairs.iter().all(|p| p.phase == PairPhase::Online)).then_some(())
    })
    .await;
    // Alice revokes her pair on the computer.
    h.handle
        .actions
        .send(UiAction::RevokePair {
            device_id: DEVICE_ID.parse().unwrap(),
        })
        .unwrap();
    eventually("DELETE /api/v1/device with alice's credential", || {
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::Revoke { auth } if auth == &format!("Extend-Device {CREDENTIAL}")))
            .then_some(())
    })
    .await;
    let s = eventually("one pair left", || {
        let s = h.handle.status.get();
        (s.pairs.len() == 1).then_some(s)
    })
    .await;
    assert_eq!(s.pairs[0].device_id, DEVICE_B);
    assert_eq!(s.phase, Phase::Online);
    let stored = h.store.load_all().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].device_id.as_str(), DEVICE_B);
    // Bob's pair stays connected, and with nobody else's pair here his Silicons get the
    // terminal back.
    let again = eventually("bob's hello with the terminal", || {
        fake.frames_on(DEVICE_B)
            .into_iter()
            .rfind(|f| f["type"] == "hello")
            .filter(|h| caps(h).contains(&"terminal".to_owned()))
    })
    .await;
    assert!(!again["missing"].to_string().contains("Several Carbons"), "{again}");
    assert_eq!(
        fake.count(|s| matches!(s, Seen::CreateEnrollment(_))),
        0,
        "no new pairing code"
    );
    assert!(!fake.connected(DEVICE_ID));
    h.stop().await;
}

#[tokio::test]
async fn a_rotated_credential_is_stored_then_confirmed_and_the_old_one_kept_until_then() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    fake.accept(CREDENTIAL_ROTATED, DEVICE_ID);
    fake.send(json!({"type":"credential","device_credential":CREDENTIAL_ROTATED}));
    eventually("credential_saved", || {
        frames_of(&fake, DEVICE_ID, "credential_saved").first().cloned()
    })
    .await;
    let c = h.store.load_all().unwrap().remove(0);
    assert_eq!(c.device_credential, CREDENTIAL_ROTATED);
    assert_eq!(c.previous_credential.as_deref(), Some(CREDENTIAL));
    // The next connection uses the new one, and the old one is forgotten.
    fake.send(json!("__drop"));
    eventually("connected with the new credential", || {
        fake.seen()
            .iter()
            .any(|s| matches!(s, Seen::DeviceSocket { auth } if auth == &format!("Extend-Device {CREDENTIAL_ROTATED}")))
            .then_some(())
    })
    .await;
    eventually("the old credential forgotten", || {
        h.store
            .load_all()
            .unwrap()
            .first()
            .filter(|c| c.previous_credential.is_none())
            .cloned()
    })
    .await;

    // A rotation the service never took (its credential_saved was lost, or the service was
    // rolled back): the app falls back to the credential it replaced.
    eventually("hello again", || hello_on(&fake, DEVICE_ID, 1)).await;
    eventually("socket", || fake.connected(DEVICE_ID).then_some(())).await;
    let never_taken = "edc_EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE";
    fake.send(json!({"type":"credential","device_credential":never_taken}));
    eventually("second credential_saved", || {
        frames_of(&fake, DEVICE_ID, "credential_saved").get(1).cloned()
    })
    .await;
    fake.send(json!("__drop"));
    eventually("back on the previous credential", || {
        let c = h.store.load_all().unwrap().remove(0);
        (c.device_credential == CREDENTIAL_ROTATED && c.previous_credential.is_none()).then_some(())
    })
    .await;
    eventually("hello a third time", || hello_on(&fake, DEVICE_ID, 2)).await;
    // No credential ever reaches the status file.
    let status = std::fs::read_to_string(h.state.join("status.json")).unwrap();
    assert!(!status.contains("edc_"), "{status}");
    h.stop().await;
}

#[tokio::test]
async fn awake_goes_to_extend_on_lock_and_unlock() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    let first = eventually("awake after hello", || {
        frames_of(&fake, DEVICE_ID, "awake").first().cloned()
    })
    .await;
    assert_eq!(first["awake"], true);
    h.computer.locked.store(true, std::sync::atomic::Ordering::SeqCst);
    let locked = eventually("locked", || {
        frames_of(&fake, DEVICE_ID, "awake")
            .into_iter()
            .find(|f| f["awake"] == false)
    })
    .await;
    assert_eq!(locked["sleep_state"], "locked");
    assert_eq!(locked["run"], first["run"]);
    assert!(locked["seq"].as_u64() > first["seq"].as_u64());
    eventually("status says locked", || {
        (h.handle.status.get().awake == Some(false)).then_some(())
    })
    .await;
    h.computer.locked.store(false, std::sync::atomic::Ordering::SeqCst);
    let unlocked = eventually("unlocked", || {
        frames_of(&fake, DEVICE_ID, "awake")
            .into_iter()
            .find(|f| f["awake"] == true && f["input_seen"] == true)
    })
    .await;
    assert!(unlocked["seq"].as_u64() > locked["seq"].as_u64());
    assert!(unlocked.get("sleep_state").is_none());
    h.stop().await;
}

fn wake_request(id: &str, who: &str, side: &str) -> Value {
    json!({"type":"wake_request","target":null,"wake_id":id,"silicon_id":who,"reason":"Check the order screen",
        "side":side,"alert":true,"created_at":expires_in(0),"expires_at":expires_in(1800)})
}

const W1: &str = "0192f3a4-0000-7000-8000-000000000001";
const W2: &str = "0192f3a4-0000-7000-8000-000000000002";

#[tokio::test]
async fn wake_requests_show_once_hide_from_another_side_and_go_when_answered() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    h.computer.locked.store(true, std::sync::atomic::Ordering::SeqCst);
    eventually("locked", || {
        frames_of(&fake, DEVICE_ID, "awake")
            .into_iter()
            .find(|f| f["awake"] == false)
    })
    .await;

    // A Silicon asks alice to wake it: the notification names it and its reason.
    fake.send(wake_request(W1, "si:chef", "sideA"));
    let shown = eventually("wake_request_shown", || {
        frames_of(&fake, DEVICE_ID, "wake_request_shown").first().cloned()
    })
    .await;
    assert_eq!(shown["wake_id"], W1);
    assert_eq!(shown["shown"], true);
    let n = h.notifier.current().expect("a notification");
    assert!(n.title.starts_with("si:chef asks to use this "), "{n:?}");
    assert!(n.body.contains("Check the order screen"), "{n:?}");
    assert!(n.alert);
    assert_eq!(h.handle.status.get().wake_requests.len(), 1);
    // Asked again (a refresh): it isn't answered twice.
    fake.send(wake_request(W1, "si:chef", "sideA"));
    fake.send(json!({"type":"ping","nonce":5}));
    eventually("pong", || frames_of(&fake, DEVICE_ID, "pong").first().cloned()).await;
    assert_eq!(frames_of(&fake, DEVICE_ID, "wake_request_shown").len(), 1);
    // Nothing of it is written to disk.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let status = std::fs::read_to_string(h.state.join("status.json")).unwrap();
    assert!(
        !status.contains("order screen") && !status.contains("si:chef"),
        "{status}"
    );

    // A Silicon of another side starts a session (a terminal session on the locked computer):
    // before its first command runs, the request no longer names anyone, on screen or in the
    // notification.
    fake.send(json!({"type":"session_started","target":null,"session_id":"5e1","silicon_id":"si:scout","since":expires_in(0),"side":"sideB"}));
    fake.send(json!({"type":"command","id":"0192f000-0000-7000-8000-0000000000c1","session_id":"5e1","command":"snapshot","args":[],"timeout_ms":30000,"upload_ids":[]}));
    eventually("the command ran", || {
        frames_of(&fake, DEVICE_ID, "result").first().cloned()
    })
    .await;
    let at_command = h.computer.at_command.lock().unwrap().clone();
    let seen = at_command[0].clone().expect("a notification while the command ran");
    assert!(
        !seen.title.contains("si:chef") && !seen.body.contains("order screen"),
        "{seen:?}"
    );
    assert!(seen.body.contains(extend_agent::notify::REDACTED), "{seen:?}");
    assert!(h.handle.status.get().wake_requests[0].silicon_id.is_none());
    // The session ends: the Carbon sees who asked again.
    fake.send(json!({"type":"session_ended","target":null,"session_id":"5e1","reason":"ended_by_silicon"}));
    eventually("named again", || {
        h.notifier.current().filter(|n| n.title.starts_with("si:chef"))
    })
    .await;
    // The service ends it: the notification goes.
    fake.send(json!({"type":"wake_request_ended","target":null,"wake_id":W1,"reason":"withdrawn"}));
    eventually("notification gone", || h.notifier.current().is_none().then_some(())).await;
    assert!(h.handle.status.get().wake_requests.is_empty());

    // Another request, then the Carbon unlocks the computer: answered, the notification goes.
    fake.send(wake_request(W2, "si:sous", "sideA"));
    eventually("second notification", || h.notifier.current()).await;
    h.computer.locked.store(false, std::sync::atomic::Ordering::SeqCst);
    eventually("answered by unlocking", || h.notifier.current().is_none().then_some(())).await;
    assert!(h.handle.status.get().wake_requests.is_empty());
    h.stop().await;
}

#[tokio::test]
async fn the_display_is_kept_on_only_while_a_session_runs_on_an_awake_computer() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    let changes = || h.display.changes.lock().unwrap().clone();
    fake.send(
        json!({"type":"session_started","target":null,"session_id":"d01","silicon_id":"si:chef","since":expires_in(0)}),
    );
    eventually("held", || (changes() == [true]).then_some(())).await;
    fake.send(json!({"type":"takeover","target":null,"session_id":"d01","reason":"Approve the prompt","expires_at":expires_in(600)}));
    eventually("released for the takeover", || {
        (changes() == [true, false]).then_some(())
    })
    .await;
    fake.send(json!({"type":"takeover_ended","target":null,"session_id":"d01"}));
    eventually("held again", || (changes() == [true, false, true]).then_some(())).await;
    h.computer.locked.store(true, std::sync::atomic::Ordering::SeqCst);
    eventually("released when locked", || (changes().len() == 4).then_some(())).await;
    h.computer.locked.store(false, std::sync::atomic::Ordering::SeqCst);
    eventually("held after unlocking", || (changes().len() == 5).then_some(())).await;
    fake.send(json!({"type":"session_ended","target":null,"session_id":"d01","reason":"idle_timeout"}));
    eventually("released at the end", || {
        (changes() == [true, false, true, false, true, false]).then_some(())
    })
    .await;
    // A carried device's session doesn't hold this computer's display.
    fake.send(json!({"type":"attach","device_id":"0000aaaa","os":"samsung_tv","name":"TV","address":"10.0.0.5"}));
    fake.send(json!({"type":"session_started","target":"0000aaaa","session_id":"d02","silicon_id":"si:chef","since":expires_in(0)}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(changes().len(), 6);
    h.stop().await;
}

#[tokio::test]
async fn setup_retry_runs_the_step_again_at_once() {
    let (factory, _, retries) = counting_factory();
    let h = start_hosting(true, &[], false, factory).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    fake.send(json!({"type":"setup_retry","target":null,"step":"accessibility"}));
    eventually("this computer's step retried", || {
        (h.computer.retried.lock().unwrap().as_slice() == [Some("accessibility".to_owned())]).then_some(())
    })
    .await;
    fake.send(json!({"type":"attach","device_id":"0000aaaa","os":"samsung_tv","name":"TV","address":"10.0.0.5"}));
    eventually("attached", || frames_of(&fake, DEVICE_ID, "attached").first().cloned()).await;
    let before = frames_of(&fake, DEVICE_ID, "attached").len();
    fake.send(json!({"type":"setup_retry","target":"0000aaaa","step":null}));
    eventually("the TV's steps retried", || {
        (retries.lock().unwrap().as_slice() == [("0000aaaa".to_owned(), None)]).then_some(())
    })
    .await;
    // And its setup reported again straight after, unchanged or not.
    eventually("attached again", || {
        (frames_of(&fake, DEVICE_ID, "attached").len() > before).then_some(())
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn a_sessions_scratch_files_go_with_it() {
    let h = start(true).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    fake.send(
        json!({"type":"session_started","target":null,"session_id":"e01","silicon_id":"si:chef","since":expires_in(0)}),
    );
    fake.send(json!({"type":"command","id":"0192f000-0000-7000-8000-0000000000e1","session_id":"e01","command":"clipboard","args":[],"timeout_ms":30000,"upload_ids":[]}));
    let r = eventually("result", || frames_of(&fake, DEVICE_ID, "result").first().cloned()).await;
    let workdir = PathBuf::from(r["output"]["workdir"].as_str().unwrap());
    assert!(workdir.starts_with(h.state.join("work").join("e01")), "{workdir:?}");
    fake.send(json!({"type":"session_ended","target":null,"session_id":"e01","reason":"ended_by_silicon"}));
    eventually("the session's scratch space is gone", || {
        (!h.state.join("work").join("e01").exists()).then_some(())
    })
    .await;
    h.stop().await;
}

#[tokio::test]
async fn a_1_0_credential_is_moved_to_the_1_1_layout_and_used() {
    let (fake, addr) = start_fake().await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config::for_tests(dir.path(), &format!("http://{addr}"));
    std::fs::write(
        config.state_dir.join("credential.json"),
        serde_json::to_vec(
            &json!({"device_id":DEVICE_ID,"device_credential":CREDENTIAL,"service_url":config.service_url.to_string()}),
        )
        .unwrap(),
    )
    .unwrap();
    let state = config.state_dir.clone();
    let store = credential::store_for(&config);
    let (agent, handle) = Agent::new(AgentDeps {
        config,
        local: Arc::new(FakeComputer::default()),
        hosted_factory: Arc::new(|_d| Err("none".into())),
        credentials: store,
        probe_interval: Duration::from_secs(3600),
        screen_watch: None,
        notifier: Arc::new(extend_agent::notify::Silent("none".into())),
        display: Arc::new(extend_agent::display::NoKeeper),
    });
    let task = tokio::spawn(agent.run());
    eventually("hello with the 1.0 credential", || hello_on(&fake, DEVICE_ID, 0)).await;
    let raw: Value = serde_json::from_slice(&std::fs::read(state.join("credential.json")).unwrap()).unwrap();
    assert!(raw.is_array(), "{raw}");
    // DeviceSelf said this is the first pair; it is noted.
    eventually("first pair noted", || {
        let raw: Value = serde_json::from_slice(&std::fs::read(state.join("credential.json")).unwrap()).unwrap();
        (raw[0]["first_pair"] == true).then_some(())
    })
    .await;
    // A computer that can't read its screen is always awake.
    let awake = eventually("awake", || frames_of(&fake, DEVICE_ID, "awake").first().cloned()).await;
    assert_eq!(awake["awake"], true);
    handle.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
}

/// A TV that counts how often it is checked.
struct CountedTv(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait]
impl Driver for CountedTv {
    async fn probe(&self) -> Probe {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Tv.probe().await
    }
    async fn run(&self, inv: Invocation<'_>) -> Output {
        Tv.run(inv).await
    }
}

#[tokio::test]
async fn a_carried_device_asked_to_wake_is_checked_every_few_seconds_until_the_request_ends() {
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = probes.clone();
    let factory: DriverFactory = Arc::new(move |_d| Ok(Box::new(CountedTv(counter.clone())) as Box<dyn Driver>));
    let h = start_hosting(true, &[], false, factory).await;
    let fake = h.fake.clone();
    eventually("hello", || hello_on(&fake, DEVICE_ID, 0)).await;
    fake.send(json!({"type":"attach","device_id":"0000aaaa","os":"samsung_tv","name":"TV","address":"10.0.0.5"}));
    eventually("attached", || frames_of(&fake, DEVICE_ID, "attached").first().cloned()).await;
    let count = || probes.load(std::sync::atomic::Ordering::SeqCst);
    // The periodic check is an hour apart here; only the wake request makes it look again.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = count();
    fake.send(
        json!({"type":"wake_request","target":"0000aaaa","wake_id":W1,"alert":false,
        "created_at":expires_in(0),"expires_at":expires_in(1800)}),
    );
    let wanted = |s: &extend_agent::status::AgentStatus| {
        s.attached.iter().any(|a| a.device_id == "0000aaaa" && a.wake_requested)
    };
    eventually("marked as asked to wake", || {
        wanted(&h.handle.status.get()).then_some(())
    })
    .await;
    for _ in 0..80 {
        if count() >= before + 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(count() >= before + 2, "checked {} times", count() - before);
    // A carried device's request shows nothing on this computer, and isn't answered for it.
    assert!(h.notifier.current().is_none());
    assert!(frames_of(&fake, DEVICE_ID, "wake_request_shown").is_empty());
    fake.send(json!({"type":"wake_request_ended","target":"0000aaaa","wake_id":W1,"reason":"woken"}));
    eventually("no longer asked", || (!wanted(&h.handle.status.get())).then_some(())).await;
    h.stop().await;
}
