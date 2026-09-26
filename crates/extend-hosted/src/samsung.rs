//! Samsung (Tizen) TVs, through the TV's local remote-control API.
//!
//! - Remote socket: `wss://<ip>:8002/api/v2/channels/samsung.remote.control?name=<base64>&token=<t>`
//!   (2016 models without TLS: `ws://<ip>:8001/…`, no token). The first connection makes the TV ask
//!   the Carbon to allow "Silicon Extend"; on Allow it sends `ms.channel.connect` with a token we
//!   keep in `state_dir/samsung.json`, and later connections present it so the TV doesn't ask again.
//! - Keys: `ms.remote.control` with `SendRemoteKey` `Click`, or `Press` … `Release` to hold.
//! - Apps: `ed.installedApp.get` and `ed.apps.launch` over the socket; REST
//!   `POST|DELETE|GET http://<ip>:8001/api/v2/applications/<appId>` to launch, close, query.
//! - Device info: `GET http://<ip>:8001/api/v2/` (answers without pairing).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use extend_driver::{Driver, Invocation, Output, Probe};
use extend_protocol::DeviceOs;
use extend_protocol::model::{MissingCapability, Setup, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::HostedDevice;
use crate::common::{
    self, Button, CLIENT_NAME, OpenTarget, find_app, guarded, invalid, load_json, not_ready,
    offline, save_json, sleep_or_cancel, step, step_error, step_help, unsupported,
    unsupported_command, url_host,
};
use crate::ws::{self, Ws};
use crate::{http, script};

const STATE_FILE: &str = "samsung.json";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const REST_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a pairing attempt waits for the Carbon to answer the prompt on the TV.
const APPROVAL_WAIT: Duration = Duration::from_secs(60);
const DEFAULT_BROWSER: &str = "org.tizen.browser";

// ───────────── Protocol ─────────────

/// Base64 of the client name, as the `name` query parameter wants it.
pub(crate) fn encode_name(name: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(name.as_bytes())
}

/// The remote-control socket URL.
pub(crate) fn remote_url(
    host: &str,
    port: u16,
    tls: bool,
    name: &str,
    token: Option<&str>,
) -> String {
    let scheme = if tls { "wss" } else { "ws" };
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("name", &encode_name(name));
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        q.append_pair("token", t);
    }
    format!(
        "{scheme}://{}:{port}/api/v2/channels/samsung.remote.control?{}",
        url_host(host),
        q.finish()
    )
}

pub(crate) fn key_for(button: Button) -> &'static str {
    match button {
        Button::Up => "KEY_UP",
        Button::Down => "KEY_DOWN",
        Button::Left => "KEY_LEFT",
        Button::Right => "KEY_RIGHT",
        Button::Select => "KEY_ENTER",
        Button::Back => "KEY_RETURN",
        Button::Home => "KEY_HOME",
        Button::Menu => "KEY_MENU",
        // Tizen has no single play/pause key; the driver alternates KEY_PAUSE and KEY_PLAY.
        Button::PlayPause => "KEY_PLAY",
        Button::VolumeUp => "KEY_VOLUP",
        Button::VolumeDown => "KEY_VOLDOWN",
        Button::Mute => "KEY_MUTE",
        Button::Power => "KEY_POWER",
    }
}

/// `Click`, `Press` or `Release` of one key.
pub(crate) fn key_message(cmd: &str, key: &str) -> String {
    json!({
        "method": "ms.remote.control",
        "params": {"Cmd": cmd, "DataOfCmd": key, "Option": "false", "TypeOfRemote": "SendRemoteKey"}
    })
    .to_string()
}

pub(crate) fn installed_apps_message() -> String {
    json!({"method": "ms.channel.emit", "params": {"event": "ed.installedApp.get", "to": "host"}})
        .to_string()
}

/// `action_type` is `DEEP_LINK` (apps with `app_type` 2, and links into apps) or `NATIVE_LAUNCH`.
pub(crate) fn launch_message(app_id: &str, action_type: &str, meta_tag: &str) -> String {
    json!({
        "method": "ms.channel.emit",
        "params": {
            "event": "ed.apps.launch",
            "to": "host",
            "data": {"action_type": action_type, "appId": app_id, "metaTag": meta_tag}
        }
    })
    .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct App {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub app_type: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    Connect { token: Option<String> },
    Unauthorized,
    TimedOut,
    InstalledApps(Vec<App>),
    Error(String),
    Other(String),
}

pub(crate) fn parse_event(text: &str) -> Event {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Event::Other("unparsable".into());
    };
    let event = v
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match event.as_str() {
        "ms.channel.connect" => Event::Connect {
            token: v.pointer("/data/token").and_then(|t| match t {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            }),
        },
        "ms.channel.unauthorized" => Event::Unauthorized,
        "ms.channel.timeOut" => Event::TimedOut,
        "ms.error" => Event::Error(
            v.pointer("/data/message")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_owned(),
        ),
        "ed.installedApp.get" => {
            let list = v
                .pointer("/data/data")
                .or_else(|| v.get("data"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Event::InstalledApps(
                list.iter()
                    .filter_map(|a| {
                        Some(App {
                            id: a.get("appId")?.as_str()?.to_owned(),
                            name: a
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            app_type: a.get("app_type").and_then(Value::as_i64).unwrap_or(0),
                        })
                    })
                    .collect(),
            )
        }
        other => Event::Other(other.to_owned()),
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct DeviceInfo {
    pub name: Option<String>,
    pub model: Option<String>,
    pub os_version: Option<String>,
    /// `false` when the TV reports `standby` (screen off, network kept alive).
    pub powered_on: bool,
    pub token_auth: bool,
}

pub(crate) fn parse_device_info(v: &Value) -> DeviceInfo {
    let d = v.get("device").cloned().unwrap_or(Value::Null);
    let s = |k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|x| !x.is_empty())
    };
    let os = s("OS").unwrap_or_else(|| "Tizen".into());
    let firmware = s("firmwareVersion").filter(|f| f != "Unknown");
    let api = v.get("version").and_then(Value::as_str);
    let os_version = match (firmware, api) {
        (Some(f), _) => Some(format!("{os} {f}")),
        (None, Some(a)) => Some(format!("{os} (remote API {a})")),
        (None, None) => Some(os),
    };
    DeviceInfo {
        name: s("name").or_else(|| v.get("name").and_then(Value::as_str).map(str::to_owned)),
        model: s("modelName").or_else(|| s("model")),
        os_version,
        powered_on: s("PowerState").is_none_or(|p| p.eq_ignore_ascii_case("on")),
        token_auth: s("TokenAuthSupport").is_some_and(|t| t == "true"),
    }
}

// ───────────── Driver ─────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    token: Option<String>,
    /// The Carbon allowed us (older TVs allow without issuing a token).
    #[serde(default)]
    approved: bool,
    /// Address found by discovery when the service didn't give one.
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Clone)]
enum Pairing {
    Idle,
    Waiting { since: Instant },
    Denied(String),
    Failed(String),
}

/// Where the TV's sockets are. Production uses 8001 (REST), 8002 (TLS socket) and 8001 (plain
/// socket for 2016 models); tests point these at mock servers.
#[derive(Debug, Clone)]
pub(crate) struct Ports {
    pub rest: u16,
    pub remote: u16,
    pub remote_tls: bool,
    pub legacy: Option<u16>,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            rest: 8001,
            remote: 8002,
            remote_tls: true,
            legacy: Some(8001),
        }
    }
}

#[derive(Debug)]
enum SocketError {
    Offline(String),
    AwaitingApproval,
    Denied,
    Protocol(String),
}

impl SocketError {
    fn into_output(self) -> Output {
        match self {
            Self::Offline(m) => offline(format!("Can't reach the TV: {m}")),
            Self::AwaitingApproval => not_ready(
                "The TV is asking the Carbon to allow \"Silicon Extend\"; it can take commands once they choose Allow",
            ),
            Self::Denied => not_ready(
                "The TV refused the connection. Allow \"Silicon Extend\" on the TV (Settings › General › External Device Manager › Device Connection Manager)",
            ),
            Self::Protocol(m) => common::failed(format!("The TV's remote socket failed: {m}")),
        }
    }
}

struct Inner {
    device: HostedDevice,
    ports: Ports,
    saved: Mutex<Saved>,
    pairing: Mutex<Pairing>,
    socket: tokio::sync::Mutex<Option<Ws>>,
    info: Mutex<Option<DeviceInfo>>,
    /// Tizen has separate play and pause keys; this remembers which to send next.
    next_is_pause: AtomicBool,
    last_app: Mutex<Option<String>>,
}

pub struct SamsungDriver {
    inner: Arc<Inner>,
}

impl SamsungDriver {
    pub fn new(device: HostedDevice) -> Self {
        Self::with_ports(device, Ports::default())
    }

    pub(crate) fn with_ports(device: HostedDevice, ports: Ports) -> Self {
        let saved: Saved = load_json(&device.state_dir, STATE_FILE).unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                device,
                ports,
                saved: Mutex::new(saved),
                pairing: Mutex::new(Pairing::Idle),
                socket: tokio::sync::Mutex::new(None),
                info: Mutex::new(None),
                next_is_pause: AtomicBool::new(true),
                last_app: Mutex::new(None),
            }),
        }
    }
}

impl Inner {
    fn saved(&self) -> Saved {
        self.saved.lock().unwrap().clone()
    }

    fn update_saved(&self, f: impl FnOnce(&mut Saved)) {
        let mut s = self.saved.lock().unwrap();
        f(&mut s);
        if let Err(e) = save_json(&self.device.state_dir, STATE_FILE, &*s) {
            tracing::warn!(error = %e, "couldn't save the Samsung TV token");
        }
    }

    fn approved(&self) -> bool {
        let s = self.saved.lock().unwrap();
        s.approved || s.token.is_some()
    }

    /// The TV's host: from the service, else remembered from discovery, else discovered now by name.
    async fn host(&self) -> Option<String> {
        if let Some(a) = self.device.address.clone().filter(|a| !a.is_empty()) {
            return Some(common::split_host_port(&a).0);
        }
        if let Some(a) = self.saved().address {
            return Some(a);
        }
        let found = crate::discover::discover(DeviceOs::SamsungTv, Duration::from_secs(3)).await;
        let pick = crate::discover::pick(&found, &self.device.name)?;
        let host = common::split_host_port(&pick.address).0;
        self.update_saved(|s| s.address = Some(host.clone()));
        Some(host)
    }

    async fn rest(&self, method: &str, path: &str) -> std::io::Result<http::Message> {
        let host = self
            .host()
            .await
            .ok_or_else(|| std::io::Error::other("no address for this TV"))?;
        let url = format!(
            "http://{}:{}/api/v2/{path}",
            url_host(&host),
            self.ports.rest
        );
        http::request(method, &url, &[], b"", REST_TIMEOUT, true).await
    }

    async fn device_info(&self) -> Result<DeviceInfo, String> {
        let resp = self.rest("GET", "").await.map_err(|e| e.to_string())?;
        let v = resp
            .json()
            .ok_or_else(|| format!("the TV answered {} without device info", resp.status()))?;
        Ok(parse_device_info(&v))
    }

    /// Opens the remote socket and waits up to `wait` for the TV to accept it.
    async fn open_socket(&self, wait: Duration) -> Result<Ws, SocketError> {
        let host = self
            .host()
            .await
            .ok_or_else(|| SocketError::Offline("no address for this TV".into()))?;
        let token = self.saved().token;
        let url = remote_url(
            &host,
            self.ports.remote,
            self.ports.remote_tls,
            CLIENT_NAME,
            token.as_deref(),
        );
        let ws = match ws::connect(&url, CONNECT_TIMEOUT).await {
            Ok(ws) => ws,
            Err(first) => match self.ports.legacy {
                Some(port) => {
                    let legacy = remote_url(&host, port, false, CLIENT_NAME, None);
                    ws::connect(&legacy, CONNECT_TIMEOUT)
                        .await
                        .map_err(|second| SocketError::Offline(format!("{first}; {second}")))?
                }
                None => return Err(SocketError::Offline(first.to_string())),
            },
        };
        self.await_connect(ws, wait).await
    }

    async fn await_connect(&self, mut ws: Ws, wait: Duration) -> Result<Ws, SocketError> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(SocketError::AwaitingApproval);
            }
            match ws::next_text(&mut ws, left).await {
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(SocketError::AwaitingApproval);
                }
                Err(e) => return Err(SocketError::Protocol(e.to_string())),
                Ok(None) => return Err(SocketError::Protocol("the TV closed the socket".into())),
                Ok(Some(text)) => match parse_event(&text) {
                    Event::Connect { token } => {
                        self.update_saved(|s| {
                            s.approved = true;
                            if token.is_some() {
                                s.token = token;
                            }
                        });
                        *self.pairing.lock().unwrap() = Pairing::Idle;
                        return Ok(ws);
                    }
                    Event::Unauthorized => return Err(SocketError::Denied),
                    Event::TimedOut => return Err(SocketError::AwaitingApproval),
                    Event::Error(m) => return Err(SocketError::Protocol(m)),
                    _ => continue,
                },
            }
        }
    }

    /// Starts a background connection that makes the TV show its prompt, unless one is running.
    fn start_pairing(self: &Arc<Self>) {
        {
            let mut p = self.pairing.lock().unwrap();
            if matches!(*p, Pairing::Waiting { .. }) {
                return;
            }
            *p = Pairing::Waiting {
                since: Instant::now(),
            };
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let result = me.open_socket(APPROVAL_WAIT).await;
            let mut p = me.pairing.lock().unwrap();
            *p = match result {
                Ok(ws) => {
                    // Keep the approved socket for the first command.
                    if let Ok(mut slot) = me.socket.try_lock() {
                        *slot = Some(ws);
                    }
                    Pairing::Idle
                }
                Err(SocketError::Denied) => Pairing::Denied("The TV denied the connection".into()),
                Err(SocketError::AwaitingApproval) => Pairing::Idle, // prompt timed out; next probe asks again
                Err(SocketError::Offline(m)) | Err(SocketError::Protocol(m)) => Pairing::Failed(m),
            };
        });
    }

    fn waiting_for_approval(&self) -> bool {
        matches!(*self.pairing.lock().unwrap(), Pairing::Waiting { since } if since.elapsed() < APPROVAL_WAIT + CONNECT_TIMEOUT)
    }

    /// Sends messages over the socket, reconnecting once if the old socket died. Each message is
    /// followed by its pause (for holds).
    async fn send(
        &self,
        messages: &[(String, Duration)],
        inv: &Invocation<'_>,
    ) -> Result<(), SocketError> {
        if !self.approved() && self.waiting_for_approval() {
            return Err(SocketError::AwaitingApproval);
        }
        let mut slot = self.socket.lock().await;
        for attempt in 0..2 {
            if slot.is_none() {
                *slot = Some(self.open_socket(Duration::from_secs(8)).await?);
            }
            let ws = slot.as_mut().expect("socket just opened");
            let mut ok = true;
            for (i, (msg, pause)) in messages.iter().enumerate() {
                if let Err(e) = ws::send_text(ws, msg.clone()).await {
                    // A dead socket fails on the first send; retry the whole list on a new one.
                    if i == 0 && attempt == 0 {
                        ok = false;
                        break;
                    }
                    *slot = None;
                    return Err(SocketError::Protocol(e.to_string()));
                }
                if !pause.is_zero() {
                    // A hold must always be released, even when cancelled.
                    let _ = sleep_or_cancel(inv, *pause).await;
                }
            }
            if ok {
                return Ok(());
            }
            *slot = None;
        }
        Err(SocketError::Protocol("couldn't send to the TV".into()))
    }

    async fn installed_apps(&self, inv: &Invocation<'_>) -> Result<Vec<App>, SocketError> {
        self.send(&[(installed_apps_message(), Duration::ZERO)], inv)
            .await?;
        let mut slot = self.socket.lock().await;
        let ws = slot
            .as_mut()
            .ok_or_else(|| SocketError::Protocol("socket closed".into()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match ws::next_text(ws, left).await {
                Ok(Some(t)) => match parse_event(&t) {
                    Event::InstalledApps(apps) => return Ok(apps),
                    Event::Error(m) => return Err(SocketError::Protocol(m)),
                    _ => continue,
                },
                Ok(None) => {
                    *slot = None;
                    return Err(SocketError::Protocol("the TV closed the socket".into()));
                }
                Err(e) => {
                    return Err(SocketError::Protocol(format!(
                        "the TV didn't list its apps ({e}); not every model can"
                    )));
                }
            }
        }
    }

    async fn press(&self, button: Button, hold: Option<Duration>, inv: &Invocation<'_>) -> Output {
        let key = if button == Button::PlayPause {
            if self.next_is_pause.fetch_xor(true, Ordering::SeqCst) {
                "KEY_PAUSE"
            } else {
                "KEY_PLAY"
            }
        } else {
            key_for(button)
        };
        let messages = match hold {
            None => vec![(key_message("Click", key), Duration::ZERO)],
            Some(d) => vec![
                (key_message("Press", key), d),
                (key_message("Release", key), Duration::ZERO),
            ],
        };
        match self.send(&messages, inv).await {
            Ok(()) => Output::ok(
                json!({"button": button.as_str(), "key": key, "held_ms": hold.map(|d| d.as_millis() as u64)}),
                match hold {
                    None => format!("Pressed {}", button.as_str()),
                    Some(d) => format!("Held {} for {} ms", button.as_str(), d.as_millis()),
                },
            ),
            Err(e) => e.into_output(),
        }
    }

    async fn open(&self, target: OpenTarget, inv: &Invocation<'_>) -> Output {
        match target {
            OpenTarget::Url(url) => {
                if !common::is_web_url(&url) {
                    return invalid(
                        "Samsung TVs open web links (http or https); to open an app, name it",
                    );
                }
                let apps = self.installed_apps(inv).await.unwrap_or_default();
                let browser = apps
                    .iter()
                    .find(|a| a.id == DEFAULT_BROWSER || a.name.eq_ignore_ascii_case("Internet"))
                    .map(|a| a.id.clone())
                    .unwrap_or_else(|| DEFAULT_BROWSER.into());
                match self
                    .send(
                        &[(
                            launch_message(&browser, "NATIVE_LAUNCH", &url),
                            Duration::ZERO,
                        )],
                        inv,
                    )
                    .await
                {
                    Ok(()) => {
                        *self.last_app.lock().unwrap() = Some(browser.clone());
                        Output::ok(
                            json!({"url": url, "app": browser}),
                            format!("Opened {url} in the TV's browser"),
                        )
                    }
                    Err(e) => e.into_output(),
                }
            }
            OpenTarget::App(name) => {
                let apps = self.installed_apps(inv).await.ok();
                let app = apps
                    .as_deref()
                    .and_then(|a| find_app(a, &name, |x| &x.id, |x| &x.name))
                    .cloned();
                let (id, label) = match (&app, &apps) {
                    (Some(a), _) => (a.id.clone(), a.name.clone()),
                    (None, Some(list)) if !looks_like_app_id(&name) => {
                        let names: Vec<&str> =
                            list.iter().map(|a| a.name.as_str()).take(40).collect();
                        return invalid(format!(
                            "No app called \"{name}\" on this TV. Installed: {}",
                            names.join(", ")
                        ));
                    }
                    _ => (name.clone(), name.clone()),
                };
                // REST first (works without the socket on most models), then the socket.
                let rest_ok = matches!(self.rest("POST", &format!("applications/{id}")).await, Ok(r) if r.is_success());
                if !rest_ok {
                    let action = if app.as_ref().is_some_and(|a| a.app_type == 2) {
                        "DEEP_LINK"
                    } else {
                        "NATIVE_LAUNCH"
                    };
                    if let Err(e) = self
                        .send(&[(launch_message(&id, action, ""), Duration::ZERO)], inv)
                        .await
                    {
                        return e.into_output();
                    }
                }
                *self.last_app.lock().unwrap() = Some(id.clone());
                Output::ok(
                    json!({"app": id, "name": label, "via": if rest_ok { "rest" } else { "socket" }}),
                    format!("Opened {label}"),
                )
            }
            OpenTarget::AppWithUrl(name, url) => {
                let apps = self.installed_apps(inv).await.unwrap_or_default();
                let id = find_app(&apps, &name, |x| &x.id, |x| &x.name)
                    .map(|a| a.id.clone())
                    .unwrap_or(name.clone());
                match self
                    .send(
                        &[(launch_message(&id, "DEEP_LINK", &url), Duration::ZERO)],
                        inv,
                    )
                    .await
                {
                    Ok(()) => {
                        *self.last_app.lock().unwrap() = Some(id.clone());
                        Output::ok(
                            json!({"app": id, "url": url}),
                            format!("Opened {url} in {name}"),
                        )
                    }
                    Err(e) => e.into_output(),
                }
            }
        }
    }

    async fn close(&self, app: Option<String>, inv: &Invocation<'_>) -> Output {
        let id = match app {
            Some(name) => {
                let apps = self.installed_apps(inv).await.unwrap_or_default();
                find_app(&apps, &name, |x| &x.id, |x| &x.name)
                    .map(|a| a.id.clone())
                    .unwrap_or(name)
            }
            None => match self.last_app.lock().unwrap().clone() {
                Some(id) => id,
                None => return invalid("Name the app to close: `close <app>`"),
            },
        };
        match self.rest("DELETE", &format!("applications/{id}")).await {
            Ok(r) if r.is_success() => {
                Output::ok(json!({"app": id, "closed": true}), format!("Closed {id}"))
            }
            Ok(r) => common::failed(format!(
                "The TV wouldn't close {id} (HTTP {}): {}",
                r.status(),
                r.text().trim()
            )),
            Err(e) => offline(format!("Can't reach the TV: {e}")),
        }
    }

    async fn appstate(&self, inv: &Invocation<'_>) -> Output {
        let apps = match self.installed_apps(inv).await {
            Ok(a) => a,
            Err(e) => return e.into_output(),
        };
        let checks = apps.iter().map(|a| async move {
            let r = self
                .rest("GET", &format!("applications/{}", a.id))
                .await
                .ok()?;
            let v = r.json()?;
            (v.get("visible").and_then(Value::as_bool) == Some(true)).then(|| a.clone())
        });
        let visible: Vec<App> = futures_util::future::join_all(checks)
            .await
            .into_iter()
            .flatten()
            .collect();
        match visible.first() {
            Some(a) => Output::ok(
                json!({"app": a.id, "name": a.name}),
                format!("{} ({})", a.name, a.id),
            ),
            None => Output::ok(json!({"app": null}), "No app in front (TV home or live TV)"),
        }
    }
}

fn looks_like_app_id(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_digit()) || (s.contains('.') && !s.contains(' '))
}

#[async_trait]
impl Driver for SamsungDriver {
    async fn probe(&self) -> Probe {
        let me = &self.inner;
        let full = DeviceOs::SamsungTv.full_capabilities();
        let info = me.device_info().await;
        let online = matches!(&info, Ok(i) if i.powered_on);
        if let Ok(i) = &info {
            *me.info.lock().unwrap() = Some(i.clone());
        }
        let cached = me.info.lock().unwrap().clone().unwrap_or_default();

        let reach_title = "The TV is on and on the same network as this computer";
        let reach = match &info {
            Ok(i) if i.powered_on => step("network", reach_title, StepStatus::Done),
            Ok(_) => step_error(
                "network",
                reach_title,
                StepStatus::NeedsCarbon,
                Some("Turn the TV on."),
                "The TV is in standby",
            ),
            Err(e) => step_error(
                "network",
                reach_title,
                StepStatus::NeedsCarbon,
                Some(
                    "Turn the TV on and connect it to the same Wi-Fi or network as this computer.",
                ),
                e.clone(),
            ),
        };

        let approve_title = "Approve the connection on the TV";
        let approve = if me.approved() {
            step("approve", approve_title, StepStatus::Done)
        } else if !online {
            step("approve", approve_title, StepStatus::Todo)
        } else {
            let state = me.pairing.lock().unwrap().clone();
            match state {
                Pairing::Denied(m) => {
                    // Retry quietly: a TV that remembers the denial answers at once without a prompt.
                    *me.pairing.lock().unwrap() = Pairing::Idle;
                    me.start_pairing();
                    step_error(
                        "approve",
                        approve_title,
                        StepStatus::Failed,
                        Some(
                            "On the TV open Settings › General › External Device Manager › Device Connection Manager › Device List and allow Silicon Extend.",
                        ),
                        m,
                    )
                }
                Pairing::Failed(m) => {
                    *me.pairing.lock().unwrap() = Pairing::Idle;
                    me.start_pairing();
                    step_error("approve", approve_title, StepStatus::NeedsCarbon, None, m)
                }
                Pairing::Idle | Pairing::Waiting { .. } => {
                    me.start_pairing();
                    step_help(
                        "approve",
                        approve_title,
                        StepStatus::NeedsCarbon,
                        "The TV shows a prompt asking to allow \"Silicon Extend\". Choose Allow with the TV's remote.",
                    )
                }
            }
        };

        let ready = online && me.approved();
        let missing = if ready {
            vec![]
        } else {
            let reason = if !online {
                "The TV is off or can't be reached"
            } else {
                "Waiting for the connection to be approved on the TV"
            };
            full.iter()
                .map(|c| MissingCapability {
                    capability: *c,
                    reason: reason.into(),
                })
                .collect()
        };
        Probe {
            os: DeviceOs::SamsungTv,
            os_version: cached.os_version,
            model: cached.model,
            capabilities: if ready { full.to_vec() } else { vec![] },
            missing,
            setup: Setup::from_steps(vec![reach, approve]),
            agent_device_version: None,
            online,
        }
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        let me = &self.inner;
        guarded(&inv, async {
            match inv.command {
                "tv-remote" => match common::parse_tv_remote(inv.args) {
                    Ok(p) => me.press(p.button, p.hold, &inv).await,
                    Err(e) => invalid(e),
                },
                "back" | "home" if inv.args.iter().all(|a| a == "--json") => {
                    let b = if inv.command == "back" {
                        Button::Back
                    } else {
                        Button::Home
                    };
                    me.press(b, None, &inv).await
                }
                "back" | "home" => invalid(format!("`{}` takes no arguments on a TV", inv.command)),
                "open" => match common::parse_open(inv.args) {
                    Ok(t) => me.open(t, &inv).await,
                    Err(e) => invalid(e),
                },
                "close" => match common::parse_close(inv.args) {
                    Ok(app) => me.close(app, &inv).await,
                    Err(e) => invalid(e),
                },
                "apps" => match me.installed_apps(&inv).await {
                    Ok(apps) => {
                        let text = apps
                            .iter()
                            .map(|a| format!("{}  {}", a.name, a.id))
                            .collect::<Vec<_>>()
                            .join("\n");
                        Output::ok(json!({"apps": apps}), text)
                    }
                    Err(e) => e.into_output(),
                },
                "appstate" => me.appstate(&inv).await,
                "replay" | "test" | "batch" => script::run(self, &inv).await,
                "app-switcher" => unsupported("Samsung TVs have no app switcher over the network"),
                other => unsupported_command(other, "a Samsung TV"),
            }
        })
        .await
    }

    async fn session_ended(&self, _session_id: &str) {
        *self.inner.last_app.lock().unwrap() = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::http::{Parser, encode_response, read_message};
    use extend_driver::cancel::CancelToken;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    #[test]
    fn name_and_urls() {
        assert_eq!(encode_name("Silicon Extend"), "U2lsaWNvbiBFeHRlbmQ=");
        assert_eq!(
            remote_url("192.168.1.5", 8002, true, "Silicon Extend", Some("12345")),
            "wss://192.168.1.5:8002/api/v2/channels/samsung.remote.control?name=U2lsaWNvbiBFeHRlbmQ%3D&token=12345"
        );
        assert_eq!(
            remote_url("192.168.1.5", 8001, false, "Silicon Extend", None),
            "ws://192.168.1.5:8001/api/v2/channels/samsung.remote.control?name=U2lsaWNvbiBFeHRlbmQ%3D"
        );
    }

    #[test]
    fn key_mapping_and_messages() {
        let expect = [
            (Button::Up, "KEY_UP"),
            (Button::Down, "KEY_DOWN"),
            (Button::Left, "KEY_LEFT"),
            (Button::Right, "KEY_RIGHT"),
            (Button::Select, "KEY_ENTER"),
            (Button::Back, "KEY_RETURN"),
            (Button::Home, "KEY_HOME"),
            (Button::Menu, "KEY_MENU"),
            (Button::VolumeUp, "KEY_VOLUP"),
            (Button::VolumeDown, "KEY_VOLDOWN"),
            (Button::Mute, "KEY_MUTE"),
            (Button::Power, "KEY_POWER"),
        ];
        for (b, k) in expect {
            assert_eq!(key_for(b), k);
        }
        let m: Value = serde_json::from_str(&key_message("Click", "KEY_HOME")).unwrap();
        assert_eq!(
            m,
            json!({"method":"ms.remote.control","params":{"Cmd":"Click","DataOfCmd":"KEY_HOME","Option":"false","TypeOfRemote":"SendRemoteKey"}})
        );
        let m: Value = serde_json::from_str(&launch_message(
            "org.tizen.browser",
            "NATIVE_LAUNCH",
            "https://x.y",
        ))
        .unwrap();
        assert_eq!(m["params"]["event"], "ed.apps.launch");
        assert_eq!(m["params"]["data"]["metaTag"], "https://x.y");
    }

    #[test]
    fn events() {
        assert_eq!(
            parse_event(
                r#"{"event":"ms.channel.connect","data":{"clients":[],"id":"a","token":"19287654"}}"#
            ),
            Event::Connect {
                token: Some("19287654".into())
            }
        );
        assert_eq!(
            parse_event(r#"{"event":"ms.channel.connect","data":{"id":"a"}}"#),
            Event::Connect { token: None }
        );
        assert_eq!(
            parse_event(r#"{"event":"ms.channel.unauthorized"}"#),
            Event::Unauthorized
        );
        let apps = parse_event(
            r#"{"data":{"data":[{"appId":"111299001912","app_type":2,"icon":"/x.png","is_lock":0,"name":"YouTube"}]},"event":"ed.installedApp.get","from":"host"}"#,
        );
        assert_eq!(
            apps,
            Event::InstalledApps(vec![App {
                id: "111299001912".into(),
                name: "YouTube".into(),
                app_type: 2
            }])
        );
    }

    #[test]
    fn device_info() {
        let v = json!({"device":{"OS":"Tizen","PowerState":"on","TokenAuthSupport":"true","firmwareVersion":"Unknown","modelName":"QN65Q80TAFXZA","name":"[TV] Living Room"},"version":"2.0.25"});
        let i = parse_device_info(&v);
        assert_eq!(i.model.as_deref(), Some("QN65Q80TAFXZA"));
        assert_eq!(i.os_version.as_deref(), Some("Tizen (remote API 2.0.25)"));
        assert!(i.powered_on && i.token_auth);
        let v = json!({"device":{"PowerState":"standby","modelName":"X"}});
        assert!(!parse_device_info(&v).powered_on);
    }

    // ───────────── Mock TV ─────────────

    #[derive(Clone, Copy, PartialEq)]
    pub(crate) enum Mode {
        Allow,
        Deny,
    }

    pub(crate) struct MockTv {
        pub rest_port: u16,
        pub ws_port: u16,
        pub socket_log: Arc<Mutex<Vec<Value>>>,
        pub rest_log: Arc<Mutex<Vec<String>>>,
        pub connect_urls: Arc<Mutex<Vec<String>>>,
    }

    const TOKEN: &str = "46781234";

    pub(crate) async fn mock_tv(mode: Mode) -> MockTv {
        let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let wsl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tv = MockTv {
            rest_port: rest.local_addr().unwrap().port(),
            ws_port: wsl.local_addr().unwrap().port(),
            socket_log: Arc::default(),
            rest_log: Arc::default(),
            connect_urls: Arc::default(),
        };
        let rest_log = tv.rest_log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = rest.accept().await else {
                    return;
                };
                let log = rest_log.clone();
                tokio::spawn(async move {
                    let mut p = Parser::default();
                    let Ok(req) = read_message(&mut s, &mut p).await else {
                        return;
                    };
                    let mut parts = req.start_line.split_whitespace();
                    let (method, path) = (
                        parts.next().unwrap_or("").to_owned(),
                        parts.next().unwrap_or("").to_owned(),
                    );
                    log.lock().unwrap().push(format!("{method} {path}"));
                    let (status, body) = match (method.as_str(), path.as_str()) {
                        ("GET", "/api/v2/") => (
                            200,
                            json!({"device":{"OS":"Tizen","PowerState":"on","TokenAuthSupport":"true","firmwareVersion":"Unknown","modelName":"QN65Q80TAFXZA","name":"[TV] Living Room"},"version":"2.0.25"}).to_string(),
                        ),
                        ("GET", "/api/v2/applications/111299001912") => (200, json!({"id":"111299001912","name":"YouTube","running":true,"visible":true}).to_string()),
                        ("GET", p) if p.starts_with("/api/v2/applications/") => (200, json!({"running":false,"visible":false}).to_string()),
                        ("POST" | "DELETE", "/api/v2/applications/111299001912" | "/api/v2/applications/3201907018807") => (200, "true".into()),
                        _ => (404, "{}".into()),
                    };
                    let _ = s
                        .write_all(&encode_response(
                            "HTTP/1.1",
                            status,
                            "OK",
                            &[("Content-Type", "application/json".into())],
                            body.as_bytes(),
                        ))
                        .await;
                });
            }
        });
        let (log, urls) = (tv.socket_log.clone(), tv.connect_urls.clone());
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = wsl.accept().await else {
                    return;
                };
                let (log, urls) = (log.clone(), urls.clone());
                tokio::spawn(async move {
                    let seen = Arc::new(Mutex::new(String::new()));
                    let seen2 = seen.clone();
                    #[allow(clippy::result_large_err)]
                    let cb = move |req: &Request, resp: Response| {
                        *seen2.lock().unwrap() = req.uri().to_string();
                        Ok(resp)
                    };
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(s, cb).await else {
                        return;
                    };
                    let uri = seen.lock().unwrap().clone();
                    urls.lock().unwrap().push(uri.clone());
                    // Startup noise real TVs send before answering.
                    let _ = ws
                        .send(Message::text(r#"{"event":"ed.edenTV.update","data":{}}"#))
                        .await;
                    if uri.contains(&format!("token={TOKEN}")) {
                        let _ = ws
                            .send(Message::text(
                                r#"{"event":"ms.channel.connect","data":{"id":"c1","clients":[]}}"#,
                            ))
                            .await;
                    } else if mode == Mode::Deny {
                        let _ = ws
                            .send(Message::text(r#"{"event":"ms.channel.unauthorized"}"#))
                            .await;
                        return;
                    } else {
                        // The prompt is on screen; the Carbon takes a moment to choose Allow.
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        let msg = json!({"event":"ms.channel.connect","data":{"id":"c1","clients":[],"token":TOKEN}});
                        let _ = ws.send(Message::text(msg.to_string())).await;
                    }
                    while let Some(Ok(m)) = ws.next().await {
                        let Message::Text(t) = m else { continue };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        log.lock().unwrap().push(v.clone());
                        if v["params"]["event"] == "ed.installedApp.get" {
                            let apps = json!({"event":"ed.installedApp.get","from":"host","data":{"data":[
                                {"appId":"111299001912","app_type":2,"name":"YouTube"},
                                {"appId":"3201907018807","app_type":2,"name":"Netflix"},
                                {"appId":"org.tizen.browser","app_type":4,"name":"Internet"}]}});
                            let _ = ws.send(Message::text(apps.to_string())).await;
                        }
                    }
                });
            }
        });
        tv
    }

    fn driver(tv: &MockTv, dir: &std::path::Path) -> SamsungDriver {
        let device = HostedDevice {
            device_id: "dev_s".into(),
            os: DeviceOs::SamsungTv,
            name: "Living Room".into(),
            address: Some("127.0.0.1".into()),
            state_dir: dir.to_path_buf(),
            agent_device: vec![],
        };
        SamsungDriver::with_ports(
            device,
            Ports {
                rest: tv.rest_port,
                remote: tv.ws_port,
                remote_tls: false,
                legacy: None,
            },
        )
    }

    pub(crate) fn inv<'a>(
        command: &'a str,
        args: &'a [String],
        dir: &'a std::path::Path,
        att: &'a [std::path::PathBuf],
    ) -> Invocation<'a> {
        Invocation {
            id: uuid::Uuid::new_v4(),
            session_id: "ses_1",
            command,
            args,
            attachments: att,
            workdir: dir,
            timeout: Duration::from_secs(20),
            cancel: CancelToken::new(),
        }
    }

    pub(crate) fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    async fn wait_until(mut f: impl FnMut() -> bool) {
        for _ in 0..100 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!("condition never became true");
    }

    #[tokio::test]
    async fn pairs_through_the_prompt_and_runs_commands() {
        let tv = mock_tv(Mode::Allow).await;
        let dir = tempfile::tempdir().unwrap();
        let d = driver(&tv, dir.path());

        // First probe: online, prompt shown, nothing usable yet.
        let p = d.probe().await;
        assert!(p.online);
        assert_eq!(p.model.as_deref(), Some("QN65Q80TAFXZA"));
        assert!(p.capabilities.is_empty());
        assert_eq!(p.setup.steps[1].status, StepStatus::NeedsCarbon);
        wait_until(|| !tv.connect_urls.lock().unwrap().is_empty()).await;
        let first = tv.connect_urls.lock().unwrap()[0].clone();
        assert!(
            first.contains("name=U2lsaWNvbiBFeHRlbmQ%3D") && !first.contains("token="),
            "{first}"
        );

        // The Carbon chooses Allow; the token lands on disk.
        wait_until(|| {
            std::fs::read_to_string(dir.path().join(STATE_FILE)).is_ok_and(|s| s.contains(TOKEN))
        })
        .await;
        let p = d.probe().await;
        assert_eq!(p.setup.state, extend_protocol::model::SetupState::Complete);
        assert_eq!(
            p.capabilities,
            DeviceOs::SamsungTv.full_capabilities().to_vec()
        );

        let w = dir.path();
        let out = d
            .run(inv("tv-remote", &args(&["press", "up"]), w, &[]))
            .await;
        assert!(out.ok, "{out:?}");
        let out = d
            .run(inv(
                "tv-remote",
                &args(&["longpress", "select", "--duration-ms", "60"]),
                w,
                &[],
            ))
            .await;
        assert!(out.ok, "{out:?}");
        assert!(d.run(inv("back", &[], w, &[])).await.ok);
        assert!(
            d.run(inv("tv-remote", &args(&["press", "play-pause"]), w, &[]))
                .await
                .ok
        );
        assert!(
            d.run(inv("tv-remote", &args(&["press", "play-pause"]), w, &[]))
                .await
                .ok
        );

        let out = d.run(inv("apps", &[], w, &[])).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.output["apps"][0]["name"], "YouTube");

        let out = d.run(inv("open", &args(&["youtube"]), w, &[])).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.output["via"], "rest");
        let out = d
            .run(inv("open", &args(&["https://example.com/a?b=1"]), w, &[]))
            .await;
        assert!(out.ok, "{out:?}");
        let out = d.run(inv("open", &args(&["Disney+"]), w, &[])).await;
        assert_eq!(out.error.unwrap().code, "invalid_args");
        let out = d.run(inv("appstate", &[], w, &[])).await;
        assert_eq!(out.output["app"], "111299001912");
        let out = d.run(inv("close", &args(&["Netflix"]), w, &[])).await;
        assert!(out.ok, "{out:?}");
        let out = d.run(inv("snapshot", &[], w, &[])).await;
        assert_eq!(out.error.unwrap().code, "unsupported_on_device");

        let keys: Vec<(String, String)> = tv
            .socket_log
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["method"] == "ms.remote.control")
            .map(|v| {
                (
                    v["params"]["Cmd"].as_str().unwrap().to_owned(),
                    v["params"]["DataOfCmd"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let k = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            keys,
            vec![
                k("Click", "KEY_UP"),
                k("Press", "KEY_ENTER"),
                k("Release", "KEY_ENTER"),
                k("Click", "KEY_RETURN"),
                k("Click", "KEY_PAUSE"),
                k("Click", "KEY_PLAY")
            ]
        );
        let launches: Vec<Value> = tv
            .socket_log
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["params"]["event"] == "ed.apps.launch")
            .cloned()
            .collect();
        assert_eq!(launches.len(), 1);
        assert_eq!(
            launches[0]["params"]["data"],
            json!({"action_type":"NATIVE_LAUNCH","appId":"org.tizen.browser","metaTag":"https://example.com/a?b=1"})
        );
        let rest = tv.rest_log.lock().unwrap().clone();
        assert!(
            rest.contains(&"POST /api/v2/applications/111299001912".to_string()),
            "{rest:?}"
        );
        assert!(
            rest.contains(&"DELETE /api/v2/applications/3201907018807".to_string()),
            "{rest:?}"
        );

        // Later connections present the token (no new prompt).
        assert!(
            tv.connect_urls
                .lock()
                .unwrap()
                .iter()
                .skip(1)
                .all(|u| u.contains(&format!("token={TOKEN}")))
        );
    }

    #[tokio::test]
    async fn runs_scripts_and_batches() {
        let tv = mock_tv(Mode::Allow).await;
        let dir = tempfile::tempdir().unwrap();
        common::save_json(
            dir.path(),
            STATE_FILE,
            &json!({"token": TOKEN, "approved": true}),
        )
        .unwrap();
        let d = driver(&tv, dir.path());
        let script = dir.path().join("zap.ad");
        std::fs::write(&script, "context platform=tv\nenv APP=\"YouTube\"\nopen \"${APP}\"\nwait 20\ntv-remote press down\nhome\n").unwrap();
        let att = vec![script];
        let out = d
            .run(inv("replay", &args(&["zap.ad"]), dir.path(), &att))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.output["steps"].as_array().unwrap().len(), 4);

        let steps = r#"[{"command":"tv-remote","input":{"button":"left"}},{"command":"snapshot","input":{}}]"#;
        let out = d
            .run(inv("batch", &args(&["--steps", steps]), dir.path(), &[]))
            .await;
        assert!(!out.ok);
        assert_eq!(out.error.as_ref().unwrap().code, "unsupported_on_device");
        assert_eq!(out.output["failed_step"], 2);
    }

    #[tokio::test]
    async fn denied_connection_is_reported() {
        let tv = mock_tv(Mode::Deny).await;
        let dir = tempfile::tempdir().unwrap();
        let d = driver(&tv, dir.path());
        let _ = d.probe().await;
        wait_until(|| matches!(*d.inner.pairing.lock().unwrap(), Pairing::Denied(_))).await;
        let p = d.probe().await;
        assert_eq!(p.setup.steps[1].status, StepStatus::Failed);
        assert!(p.capabilities.is_empty());
    }

    #[tokio::test]
    async fn offline_tv() {
        let dir = tempfile::tempdir().unwrap();
        let device = HostedDevice {
            device_id: "dev_s".into(),
            os: DeviceOs::SamsungTv,
            name: "Gone".into(),
            address: Some("127.0.0.1".into()),
            state_dir: dir.path().to_path_buf(),
            agent_device: vec![],
        };
        // Nothing listens on these ports.
        let d = SamsungDriver::with_ports(
            device,
            Ports {
                rest: 1,
                remote: 2,
                remote_tls: false,
                legacy: None,
            },
        );
        let p = d.probe().await;
        assert!(!p.online);
        assert_eq!(p.setup.steps[0].status, StepStatus::NeedsCarbon);
        let out = d
            .run(inv("tv-remote", &args(&["press", "up"]), dir.path(), &[]))
            .await;
        assert_eq!(out.error.unwrap().code, "device_offline");
    }
}
