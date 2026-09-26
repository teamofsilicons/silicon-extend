//! LG (webOS) TVs, through SSAP ("Simple Service Access Protocol") over the TV's local WebSocket.
//!
//! - Main socket: `ws://<ip>:3000` (older webOS), else `wss://<ip>:3001` (webOS 2022+, self-signed).
//! - Pairing: a `register` message with the standard manifest and `pairingType: PROMPT`. The TV asks
//!   the Carbon to accept; on accept it answers `registered` with a `client-key` we keep in
//!   `state_dir/lg.json` and present on later connections, so the TV doesn't ask again.
//! - Buttons: `ssap://com.webos.service.networkinput/getPointerInputSocket` gives a second socket
//!   that takes `type:button\nname:UP\n\n`.
//! - Apps: `ssap://com.webos.applicationManager/listLaunchPoints`, `ssap://system.launcher/launch`
//!   (links through `com.webos.app.browser` with `target`), `ssap://system.launcher/close`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bridge_driver::{Driver, Invocation, Output, Probe};
use bridge_protocol::DeviceOs;
use bridge_protocol::model::{MissingCapability, Setup, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::HostedDevice;
use crate::common::{
    self, Button, CLIENT_NAME, OpenTarget, find_app, guarded, invalid, load_json, not_ready,
    offline, save_json, step, step_error, step_help, unsupported, unsupported_command, url_host,
};
use crate::script;
use crate::ws::{self, Ws};

const STATE_FILE: &str = "lg.json";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const APPROVAL_WAIT: Duration = Duration::from_secs(60);
const BROWSER: &str = "com.webos.app.browser";

// ───────────── Protocol ─────────────

/// The permissions Bridge asks for when registering (the standard unsigned manifest that current
/// webOS accepts with `pairingType: PROMPT`).
const PERMISSIONS: &[&str] = &[
    "APP_TO_APP",
    "CLOSE",
    "CONTROL_AUDIO",
    "CONTROL_DISPLAY",
    "CONTROL_INPUT_JOYSTICK",
    "CONTROL_INPUT_MEDIA_PLAYBACK",
    "CONTROL_INPUT_MEDIA_RECORDING",
    "CONTROL_INPUT_TEXT",
    "CONTROL_INPUT_TV",
    "CONTROL_MOUSE_AND_KEYBOARD",
    "CONTROL_POWER",
    "CONTROL_TV_SCREEN",
    "LAUNCH",
    "LAUNCH_WEBAPP",
    "READ_APP_STATUS",
    "READ_COUNTRY_INFO",
    "READ_CURRENT_CHANNEL",
    "READ_INPUT_DEVICE_LIST",
    "READ_INSTALLED_APPS",
    "READ_LGE_SDX",
    "READ_LGE_TV_INPUT_EVENTS",
    "READ_NETWORK_STATE",
    "READ_NOTIFICATIONS",
    "READ_POWER_STATE",
    "READ_RUNNING_APPS",
    "READ_SETTINGS",
    "READ_TV_CHANNEL_LIST",
    "READ_TV_CURRENT_TIME",
    "READ_UPDATE_INFO",
    "SEARCH",
    "TEST_OPEN",
    "TEST_PROTECTED",
    "TEST_SECURE",
    "UPDATE_FROM_REMOTE_APP",
    "WRITE_NOTIFICATION_ALERT",
    "WRITE_NOTIFICATION_TOAST",
    "WRITE_SETTINGS",
];

pub(crate) fn register_message(client_key: Option<&str>) -> String {
    let mut payload = json!({
        "forcePairing": false,
        "pairingType": "PROMPT",
        "manifest": {
            "appVersion": "1.1",
            "manifestVersion": 1,
            "permissions": PERMISSIONS,
            "localizedAppNames": {"": CLIENT_NAME},
        },
    });
    if let Some(k) = client_key.filter(|k| !k.is_empty()) {
        payload["client-key"] = json!(k);
    }
    json!({"type": "register", "id": "register_0", "payload": payload}).to_string()
}

pub(crate) fn request_message(id: &str, uri: &str, payload: Value) -> String {
    json!({"id": id, "type": "request", "uri": format!("ssap://{uri}"), "payload": payload})
        .to_string()
}

pub(crate) fn button_name(button: Button) -> Option<&'static str> {
    Some(match button {
        Button::Up => "UP",
        Button::Down => "DOWN",
        Button::Left => "LEFT",
        Button::Right => "RIGHT",
        Button::Select => "ENTER",
        Button::Back => "BACK",
        Button::Home => "HOME",
        Button::Menu => "MENU",
        Button::PlayPause => "PLAY",
        Button::VolumeUp => "VOLUMEUP",
        Button::VolumeDown => "VOLUMEDOWN",
        Button::Mute => "MUTE",
        // Power goes through `ssap://system/turnOff`; the pointer socket's POWER is unreliable.
        Button::Power => return None,
    })
}

pub(crate) fn pointer_button(name: &str) -> String {
    format!("type:button\nname:{name}\n\n")
}

/// Payload for launching a link in the TV's browser.
pub(crate) fn browser_launch(url: &str) -> Value {
    json!({"id": BROWSER, "target": url})
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reply {
    /// The TV put up its accept prompt.
    Prompt,
    Registered {
        client_key: Option<String>,
    },
    Response {
        id: String,
        payload: Value,
    },
    Error {
        id: String,
        error: String,
    },
    Other,
}

pub(crate) fn parse_reply(text: &str) -> Reply {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Reply::Other;
    };
    let id = match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    match v.get("type").and_then(Value::as_str) {
        Some("registered") => Reply::Registered {
            client_key: payload
                .get("client-key")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("response")
            if payload.get("pairingType").and_then(Value::as_str) == Some("PROMPT") =>
        {
            Reply::Prompt
        }
        Some("response") => Reply::Response { id, payload },
        Some("error") => Reply::Error {
            id,
            error: v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_owned(),
        },
        _ => Reply::Other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct App {
    pub id: String,
    pub name: String,
}

pub(crate) fn parse_launch_points(payload: &Value) -> Vec<App> {
    payload
        .get("launchPoints")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|a| {
                    Some(App {
                        id: a.get("id")?.as_str()?.to_owned(),
                        name: a
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ───────────── Driver ─────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    client_key: Option<String>,
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

#[derive(Debug, Clone)]
pub(crate) struct Ports {
    pub plain: Option<u16>,
    pub tls: Option<u16>,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            plain: Some(3000),
            tls: Some(3001),
        }
    }
}

#[derive(Debug)]
enum LgError {
    Offline(String),
    AwaitingApproval,
    Denied(String),
    Failed(String),
}

impl LgError {
    fn into_output(self) -> Output {
        match self {
            Self::Offline(m) => offline(format!("Can't reach the TV: {m}")),
            Self::AwaitingApproval => not_ready(
                "The TV is asking the Carbon to accept \"Silicon Bridge\"; it can take commands once they accept",
            ),
            Self::Denied(m) => not_ready(format!(
                "The TV refused the connection ({m}). Accept \"Silicon Bridge\" when the TV asks"
            )),
            Self::Failed(m) => common::failed(m),
        }
    }
}

struct Session {
    main: Ws,
    pointer: Option<Ws>,
}

#[derive(Debug, Clone, Default)]
struct Info {
    model: Option<String>,
    os_version: Option<String>,
}

struct Inner {
    device: HostedDevice,
    ports: Ports,
    saved: Mutex<Saved>,
    pairing: Mutex<Pairing>,
    session: tokio::sync::Mutex<Option<Session>>,
    info: Mutex<Info>,
    next_id: AtomicU64,
    next_is_pause: AtomicBool,
}

pub struct LgDriver {
    inner: Arc<Inner>,
}

impl LgDriver {
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
                session: tokio::sync::Mutex::new(None),
                info: Mutex::new(Info::default()),
                next_id: AtomicU64::new(1),
                next_is_pause: AtomicBool::new(true),
            }),
        }
    }
}

impl Inner {
    fn client_key(&self) -> Option<String> {
        self.saved.lock().unwrap().client_key.clone()
    }

    fn update_saved(&self, f: impl FnOnce(&mut Saved)) {
        let mut s = self.saved.lock().unwrap();
        f(&mut s);
        if let Err(e) = save_json(&self.device.state_dir, STATE_FILE, &*s) {
            tracing::warn!(error = %e, "couldn't save the LG client key");
        }
    }

    async fn host(&self) -> Option<String> {
        if let Some(a) = self.device.address.clone().filter(|a| !a.is_empty()) {
            return Some(common::split_host_port(&a).0);
        }
        if let Some(a) = self.saved.lock().unwrap().address.clone() {
            return Some(a);
        }
        let found = crate::discover::discover(DeviceOs::LgTv, Duration::from_secs(3)).await;
        let host =
            common::split_host_port(&crate::discover::pick(&found, &self.device.name)?.address).0;
        self.update_saved(|s| s.address = Some(host.clone()));
        Some(host)
    }

    /// Opens the main socket (plain first, then TLS) and registers, waiting up to `wait` for the
    /// Carbon to accept the prompt.
    async fn open_session(&self, wait: Duration) -> Result<Session, LgError> {
        let host = self
            .host()
            .await
            .ok_or_else(|| LgError::Offline("no address for this TV".into()))?;
        let mut errors = Vec::new();
        let mut main = None;
        for (port, scheme) in [(self.ports.plain, "ws"), (self.ports.tls, "wss")] {
            let Some(port) = port else { continue };
            match ws::connect(
                &format!("{scheme}://{}:{port}", url_host(&host)),
                CONNECT_TIMEOUT,
            )
            .await
            {
                Ok(w) => {
                    main = Some(w);
                    break;
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        let mut main = main.ok_or_else(|| LgError::Offline(errors.join("; ")))?;
        ws::send_text(&mut main, register_message(self.client_key().as_deref()))
            .await
            .map_err(|e| LgError::Offline(e.to_string()))?;
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LgError::AwaitingApproval);
            }
            match ws::next_text(&mut main, left).await {
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(LgError::AwaitingApproval);
                }
                Err(e) => return Err(LgError::Failed(e.to_string())),
                Ok(None) => return Err(LgError::Failed("the TV closed the socket".into())),
                Ok(Some(t)) => match parse_reply(&t) {
                    Reply::Prompt => {
                        let mut p = self.pairing.lock().unwrap();
                        if !matches!(*p, Pairing::Waiting { .. }) {
                            *p = Pairing::Waiting {
                                since: Instant::now(),
                            };
                        }
                    }
                    Reply::Registered { client_key } => {
                        if let Some(k) = client_key
                            && self.client_key().as_deref() != Some(k.as_str())
                        {
                            self.update_saved(|s| s.client_key = Some(k));
                        }
                        *self.pairing.lock().unwrap() = Pairing::Idle;
                        return Ok(Session {
                            main,
                            pointer: None,
                        });
                    }
                    Reply::Error { id, error } if id == "register_0" => {
                        return Err(LgError::Denied(error));
                    }
                    _ => continue,
                },
            }
        }
    }

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
            let result = me.open_session(APPROVAL_WAIT).await;
            let next = match result {
                Ok(s) => {
                    if let Ok(mut slot) = me.session.try_lock() {
                        *slot = Some(s);
                    }
                    Pairing::Idle
                }
                Err(LgError::Denied(m)) => Pairing::Denied(m),
                Err(LgError::AwaitingApproval) => Pairing::Idle,
                Err(LgError::Offline(m)) | Err(LgError::Failed(m)) => Pairing::Failed(m),
            };
            *me.pairing.lock().unwrap() = next;
        });
    }

    fn waiting_for_approval(&self) -> bool {
        matches!(*self.pairing.lock().unwrap(), Pairing::Waiting { since } if since.elapsed() < APPROVAL_WAIT + CONNECT_TIMEOUT)
    }

    async fn ensure<'a>(&self, slot: &'a mut Option<Session>) -> Result<&'a mut Session, LgError> {
        if slot.is_none() {
            if self.client_key().is_none() && self.waiting_for_approval() {
                return Err(LgError::AwaitingApproval);
            }
            *slot = Some(self.open_session(Duration::from_secs(8)).await?);
        }
        Ok(slot.as_mut().expect("session just opened"))
    }

    async fn exchange(
        &self,
        session: &mut Session,
        uri: &str,
        payload: &Value,
    ) -> Result<Value, LgError> {
        let id = format!("req_{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        ws::send_text(
            &mut session.main,
            request_message(&id, uri, payload.clone()),
        )
        .await
        .map_err(|e| LgError::Offline(e.to_string()))?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match ws::next_text(&mut session.main, left).await {
                Ok(Some(t)) => match parse_reply(&t) {
                    Reply::Response { id: rid, payload } if rid == id => {
                        if payload.get("returnValue") == Some(&Value::Bool(false)) {
                            let why = payload
                                .get("errorText")
                                .and_then(Value::as_str)
                                .unwrap_or("request refused");
                            return Err(LgError::Failed(format!("{uri}: {why}")));
                        }
                        return Ok(payload);
                    }
                    Reply::Error { id: rid, error } if rid == id => {
                        return Err(LgError::Failed(format!("{uri}: {error}")));
                    }
                    _ => continue,
                },
                Ok(None) => return Err(LgError::Offline("the TV closed the socket".into())),
                Err(e) => return Err(LgError::Offline(e.to_string())),
            }
        }
    }

    /// One SSAP request, reconnecting once if the socket had died.
    async fn request(&self, uri: &str, payload: Value) -> Result<Value, LgError> {
        let mut slot = self.session.lock().await;
        for attempt in 0..2 {
            let session = self.ensure(&mut slot).await?;
            match self.exchange(session, uri, &payload).await {
                Err(LgError::Offline(_)) if attempt == 0 => *slot = None,
                other => return other,
            }
        }
        Err(LgError::Offline(
            "the TV keeps dropping the connection".into(),
        ))
    }

    async fn button(&self, name: &str) -> Result<(), LgError> {
        let mut slot = self.session.lock().await;
        for attempt in 0..2 {
            let session = self.ensure(&mut slot).await?;
            if session.pointer.is_none() {
                let payload = self
                    .exchange(
                        session,
                        "com.webos.service.networkinput/getPointerInputSocket",
                        &json!({}),
                    )
                    .await?;
                let path = payload
                    .get("socketPath")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LgError::Failed("the TV gave no pointer socket".into()))?;
                session.pointer = Some(
                    ws::connect(path, CONNECT_TIMEOUT)
                        .await
                        .map_err(|e| LgError::Offline(e.to_string()))?,
                );
            }
            let pointer = session
                .pointer
                .as_mut()
                .expect("pointer socket just opened");
            match ws::send_text(pointer, pointer_button(name)).await {
                Ok(()) => return Ok(()),
                Err(_) if attempt == 0 => *slot = None,
                Err(e) => return Err(LgError::Offline(e.to_string())),
            }
        }
        Err(LgError::Offline(
            "the TV keeps dropping the connection".into(),
        ))
    }

    async fn apps(&self) -> Result<Vec<App>, LgError> {
        Ok(parse_launch_points(
            &self
                .request("com.webos.applicationManager/listLaunchPoints", json!({}))
                .await?,
        ))
    }

    async fn press(&self, button: Button, hold: Option<Duration>) -> Output {
        if hold.is_some() {
            return unsupported(
                "LG TVs can't hold a remote button over the network; use `tv-remote press`",
            );
        }
        let result = match button {
            Button::Power => self
                .request("system/turnOff", json!({}))
                .await
                .map(|_| "system/turnOff"),
            Button::PlayPause => {
                let name = if self.next_is_pause.fetch_xor(true, Ordering::SeqCst) {
                    "PAUSE"
                } else {
                    "PLAY"
                };
                self.button(name).await.map(|_| name)
            }
            b => {
                let name = button_name(b).expect("every other button has a name");
                self.button(name).await.map(|_| name)
            }
        };
        match result {
            Ok(sent) => Output::ok(
                json!({"button": button.as_str(), "sent": sent}),
                format!("Pressed {}", button.as_str()),
            ),
            Err(e) => e.into_output(),
        }
    }

    async fn open(&self, target: OpenTarget) -> Output {
        match target {
            OpenTarget::Url(url) => {
                if !common::is_web_url(&url) {
                    return invalid(
                        "LG TVs open web links (http or https); to open an app, name it",
                    );
                }
                let launched = match self
                    .request("system.launcher/launch", browser_launch(&url))
                    .await
                {
                    Ok(p) => Ok(p),
                    Err(LgError::Failed(_)) => {
                        self.request("system.launcher/open", json!({"target": url}))
                            .await
                    }
                    Err(e) => Err(e),
                };
                match launched {
                    Ok(_) => Output::ok(
                        json!({"url": url, "app": BROWSER}),
                        format!("Opened {url} in the TV's browser"),
                    ),
                    Err(e) => e.into_output(),
                }
            }
            OpenTarget::App(name) => {
                let apps = match self.apps().await {
                    Ok(a) => a,
                    Err(e) => return e.into_output(),
                };
                let Some(app) = find_app(&apps, &name, |a| &a.id, |a| &a.name).cloned() else {
                    let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).take(40).collect();
                    return invalid(format!(
                        "No app called \"{name}\" on this TV. Installed: {}",
                        names.join(", ")
                    ));
                };
                match self
                    .request("system.launcher/launch", json!({"id": app.id}))
                    .await
                {
                    Ok(_) => Output::ok(
                        json!({"app": app.id, "name": app.name}),
                        format!("Opened {}", app.name),
                    ),
                    Err(e) => e.into_output(),
                }
            }
            OpenTarget::AppWithUrl(name, url) => {
                let apps = self.apps().await.unwrap_or_default();
                let id = find_app(&apps, &name, |a| &a.id, |a| &a.name)
                    .map(|a| a.id.clone())
                    .unwrap_or(name.clone());
                let payload = json!({"id": id, "contentId": url, "params": {"contentTarget": url}});
                match self.request("system.launcher/launch", payload).await {
                    Ok(_) => Output::ok(
                        json!({"app": id, "url": url}),
                        format!("Opened {url} in {name}"),
                    ),
                    Err(e) => e.into_output(),
                }
            }
        }
    }

    async fn foreground(&self) -> Result<Option<String>, LgError> {
        let p = self
            .request(
                "com.webos.applicationManager/getForegroundAppInfo",
                json!({}),
            )
            .await?;
        Ok(p.get("appId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned))
    }

    async fn close(&self, app: Option<String>) -> Output {
        let id = match app {
            Some(name) => {
                let apps = self.apps().await.unwrap_or_default();
                find_app(&apps, &name, |a| &a.id, |a| &a.name)
                    .map(|a| a.id.clone())
                    .unwrap_or(name)
            }
            None => match self.foreground().await {
                Ok(Some(id)) => id,
                Ok(None) => return Output::ok(json!({"closed": false}), "No app is open"),
                Err(e) => return e.into_output(),
            },
        };
        match self
            .request("system.launcher/close", json!({"id": id}))
            .await
        {
            Ok(_) => Output::ok(json!({"app": id, "closed": true}), format!("Closed {id}")),
            Err(e) => e.into_output(),
        }
    }

    async fn refresh_info(&self) -> Result<(), LgError> {
        if self.info.lock().unwrap().model.is_some() {
            // Cheap liveness check once the details are known.
            return self.foreground().await.map(|_| ());
        }
        let sys = self.request("system/getSystemInfo", json!({})).await?;
        let sw = self
            .request(
                "com.webos.service.update/getCurrentSWInformation",
                json!({}),
            )
            .await
            .unwrap_or(Value::Null);
        let s = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|x| !x.is_empty())
        };
        let os_version = match (
            s(&sw, "product_name"),
            s(&sw, "major_ver"),
            s(&sw, "minor_ver"),
        ) {
            (Some(p), Some(maj), Some(min)) => Some(format!("{p} ({maj}.{min})")),
            (Some(p), _, _) => Some(p),
            _ => Some("webOS".into()),
        };
        *self.info.lock().unwrap() = Info {
            model: s(&sys, "modelName"),
            os_version,
        };
        Ok(())
    }

    async fn reachable(&self) -> Result<(), String> {
        let host = self.host().await.ok_or("no address for this TV")?;
        let mut last = String::from("no ports to try");
        for port in [self.ports.plain, self.ports.tls].into_iter().flatten() {
            match crate::tls::tcp(&host, port, Duration::from_secs(2)).await {
                Ok(_) => return Ok(()),
                Err(e) => last = format!("{host}:{port}: {e}"),
            }
        }
        Err(last)
    }
}

#[async_trait]
impl Driver for LgDriver {
    async fn probe(&self) -> Probe {
        let me = &self.inner;
        let full = DeviceOs::LgTv.full_capabilities();
        let reach_title = "The TV is on and on the same network as this computer";
        let approve_title = "Approve the connection on the TV";
        let (online, reach, approve) = if me.client_key().is_some() {
            match me.refresh_info().await {
                Ok(()) => (
                    true,
                    step("network", reach_title, StepStatus::Done),
                    step("approve", approve_title, StepStatus::Done),
                ),
                Err(LgError::Denied(e)) => {
                    // The TV forgot us (reset, or the Carbon removed the pairing): ask again.
                    me.update_saved(|s| s.client_key = None);
                    (
                        true,
                        step("network", reach_title, StepStatus::Done),
                        step_error("approve", approve_title, StepStatus::NeedsCarbon, None, e),
                    )
                }
                Err(e) => {
                    let m = match e {
                        LgError::Offline(m) | LgError::Failed(m) => m,
                        _ => "no answer".into(),
                    };
                    (
                        false,
                        step_error(
                            "network",
                            reach_title,
                            StepStatus::NeedsCarbon,
                            Some(
                                "Turn the TV on and connect it to the same network as this computer.",
                            ),
                            m,
                        ),
                        step("approve", approve_title, StepStatus::Done),
                    )
                }
            }
        } else {
            match me.reachable().await {
                Err(m) => (
                    false,
                    step_error(
                        "network",
                        reach_title,
                        StepStatus::NeedsCarbon,
                        Some("Turn the TV on and connect it to the same network as this computer."),
                        m,
                    ),
                    step("approve", approve_title, StepStatus::Todo),
                ),
                Ok(()) => {
                    let state = me.pairing.lock().unwrap().clone();
                    let approve = match state {
                        Pairing::Denied(m) => {
                            *me.pairing.lock().unwrap() = Pairing::Idle;
                            step_error(
                                "approve",
                                approve_title,
                                StepStatus::Failed,
                                Some(
                                    "Accept \"Silicon Bridge\" when the TV asks; Bridge asks again shortly.",
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
                                "The TV asks whether to accept \"Silicon Bridge\". Choose Accept (or Yes) with the TV's remote.",
                            )
                        }
                    };
                    (
                        true,
                        step("network", reach_title, StepStatus::Done),
                        approve,
                    )
                }
            }
        };
        let ready = online && me.client_key().is_some();
        let info = me.info.lock().unwrap().clone();
        let reason = if !online {
            "The TV is off or can't be reached"
        } else {
            "Waiting for the connection to be accepted on the TV"
        };
        Probe {
            os: DeviceOs::LgTv,
            os_version: info.os_version,
            model: info.model,
            capabilities: if ready { full.to_vec() } else { vec![] },
            missing: if ready {
                vec![]
            } else {
                full.iter()
                    .map(|c| MissingCapability {
                        capability: *c,
                        reason: reason.into(),
                    })
                    .collect()
            },
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
                    Ok(p) => me.press(p.button, p.hold).await,
                    Err(e) => invalid(e),
                },
                "back" | "home" if inv.args.iter().all(|a| a == "--json") => {
                    me.press(
                        if inv.command == "back" {
                            Button::Back
                        } else {
                            Button::Home
                        },
                        None,
                    )
                    .await
                }
                "back" | "home" => invalid(format!("`{}` takes no arguments on a TV", inv.command)),
                "open" => match common::parse_open(inv.args) {
                    Ok(t) => me.open(t).await,
                    Err(e) => invalid(e),
                },
                "close" => match common::parse_close(inv.args) {
                    Ok(app) => me.close(app).await,
                    Err(e) => invalid(e),
                },
                "apps" => match me.apps().await {
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
                "appstate" => match me.foreground().await {
                    Ok(Some(id)) => Output::ok(json!({"app": id}), id),
                    Ok(None) => Output::ok(json!({"app": null}), "No app in front"),
                    Err(e) => e.into_output(),
                },
                "replay" | "test" | "batch" => script::run(self, &inv).await,
                "app-switcher" => unsupported("LG TVs have no app switcher over the network"),
                other => unsupported_command(other, "an LG TV"),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samsung::tests::{args, inv};
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    #[test]
    fn messages() {
        let r: Value = serde_json::from_str(&register_message(Some("abc"))).unwrap();
        assert_eq!(r["type"], "register");
        assert_eq!(r["id"], "register_0");
        assert_eq!(r["payload"]["pairingType"], "PROMPT");
        assert_eq!(r["payload"]["client-key"], "abc");
        assert_eq!(r["payload"]["manifest"]["manifestVersion"], 1);
        assert!(
            r["payload"]["manifest"]["permissions"]
                .as_array()
                .unwrap()
                .contains(&json!("LAUNCH"))
        );
        let r: Value = serde_json::from_str(&register_message(None)).unwrap();
        assert!(r["payload"].get("client-key").is_none());

        let q: Value = serde_json::from_str(&request_message(
            "req_1",
            "system.launcher/launch",
            json!({"id": "netflix"}),
        ))
        .unwrap();
        assert_eq!(
            q,
            json!({"id":"req_1","type":"request","uri":"ssap://system.launcher/launch","payload":{"id":"netflix"}})
        );
        assert_eq!(pointer_button("UP"), "type:button\nname:UP\n\n");
        assert_eq!(
            browser_launch("https://a.b"),
            json!({"id":"com.webos.app.browser","target":"https://a.b"})
        );
    }

    #[test]
    fn buttons() {
        let expect = [
            (Button::Up, "UP"),
            (Button::Down, "DOWN"),
            (Button::Left, "LEFT"),
            (Button::Right, "RIGHT"),
            (Button::Select, "ENTER"),
            (Button::Back, "BACK"),
            (Button::Home, "HOME"),
            (Button::Menu, "MENU"),
            (Button::VolumeUp, "VOLUMEUP"),
            (Button::VolumeDown, "VOLUMEDOWN"),
            (Button::Mute, "MUTE"),
        ];
        for (b, n) in expect {
            assert_eq!(button_name(b), Some(n));
        }
        assert_eq!(button_name(Button::Power), None);
    }

    #[test]
    fn replies() {
        assert_eq!(
            parse_reply(
                r#"{"type":"response","id":"register_0","payload":{"pairingType":"PROMPT","returnValue":true}}"#
            ),
            Reply::Prompt
        );
        assert_eq!(
            parse_reply(r#"{"type":"registered","id":"register_0","payload":{"client-key":"k1"}}"#),
            Reply::Registered {
                client_key: Some("k1".into())
            }
        );
        assert_eq!(
            parse_reply(
                r#"{"type":"error","id":"register_0","error":"403 User denied access","payload":{}}"#
            ),
            Reply::Error {
                id: "register_0".into(),
                error: "403 User denied access".into()
            }
        );
        let apps = parse_launch_points(
            &json!({"launchPoints":[{"id":"netflix","title":"Netflix","icon":"x"}],"returnValue":true}),
        );
        assert_eq!(
            apps,
            vec![App {
                id: "netflix".into(),
                name: "Netflix".into()
            }]
        );
    }

    // ───────────── Mock TV ─────────────

    const KEY: &str = "3b5d8e0f2c1a";

    struct MockLg {
        port: u16,
        requests: Arc<Mutex<Vec<Value>>>,
        pointer: Arc<Mutex<Vec<String>>>,
        registers: Arc<Mutex<Vec<Value>>>,
    }

    async fn mock_lg(deny: bool) -> MockLg {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let tv = MockLg {
            port,
            requests: Arc::default(),
            pointer: Arc::default(),
            registers: Arc::default(),
        };
        let (reqs, ptr, regs) = (
            tv.requests.clone(),
            tv.pointer.clone(),
            tv.registers.clone(),
        );
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = l.accept().await else { return };
                let (reqs, ptr, regs) = (reqs.clone(), ptr.clone(), regs.clone());
                tokio::spawn(async move {
                    let path = Arc::new(Mutex::new(String::new()));
                    let p2 = path.clone();
                    #[allow(clippy::result_large_err)]
                    let cb = move |r: &Request, resp: Response| {
                        *p2.lock().unwrap() = r.uri().path().to_owned();
                        Ok(resp)
                    };
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(s, cb).await else {
                        return;
                    };
                    if path.lock().unwrap().ends_with("netinput.pointer.sock") {
                        while let Some(Ok(Message::Text(t))) = ws.next().await {
                            ptr.lock().unwrap().push(t.to_string());
                        }
                        return;
                    }
                    while let Some(Ok(m)) = ws.next().await {
                        let Message::Text(t) = m else { continue };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v["type"] == "register" {
                            regs.lock().unwrap().push(v.clone());
                            if v["payload"]["client-key"] == KEY {
                                let _ = ws.send(Message::text(json!({"type":"registered","id":"register_0","payload":{"client-key":KEY}}).to_string())).await;
                                continue;
                            }
                            let _ = ws
                                .send(Message::text(json!({"type":"response","id":"register_0","payload":{"pairingType":"PROMPT","returnValue":true}}).to_string()))
                                .await;
                            tokio::time::sleep(Duration::from_millis(150)).await;
                            let answer = if deny {
                                json!({"type":"error","id":"register_0","error":"403 User denied access","payload":{}})
                            } else {
                                json!({"type":"registered","id":"register_0","payload":{"client-key":KEY}})
                            };
                            let _ = ws.send(Message::text(answer.to_string())).await;
                            continue;
                        }
                        reqs.lock().unwrap().push(v.clone());
                        let id = v["id"].clone();
                        let uri = v["uri"]
                            .as_str()
                            .unwrap_or("")
                            .trim_start_matches("ssap://")
                            .to_owned();
                        let ok = |p: Value| json!({"type":"response","id":id,"payload":p});
                        let reply = match uri.as_str() {
                            "system/getSystemInfo" => {
                                ok(json!({"modelName":"OLED55C1PUB","returnValue":true}))
                            }
                            "com.webos.service.update/getCurrentSWInformation" => ok(
                                json!({"product_name":"webOSTV 6.0","major_ver":"03","minor_ver":"21.20","returnValue":true}),
                            ),
                            "com.webos.service.networkinput/getPointerInputSocket" => ok(json!({
                                "socketPath": format!("ws://127.0.0.1:{port}/resources/9f1c/netinput.pointer.sock"), "returnValue": true
                            })),
                            "com.webos.applicationManager/listLaunchPoints" => {
                                ok(json!({"launchPoints":[
                                {"id":"youtube.leanback.v4","title":"YouTube"},{"id":"netflix","title":"Netflix"},
                                {"id":"com.webos.app.browser","title":"Web Browser"}],"returnValue":true}))
                            }
                            "system.launcher/launch" | "system.launcher/close" => {
                                let known =
                                    ["youtube.leanback.v4", "netflix", "com.webos.app.browser"];
                                if known.contains(&v["payload"]["id"].as_str().unwrap_or("")) {
                                    ok(json!({"id": v["payload"]["id"], "returnValue": true}))
                                } else {
                                    ok(
                                        json!({"returnValue": false, "errorCode": -101, "errorText": "app not found"}),
                                    )
                                }
                            }
                            "com.webos.applicationManager/getForegroundAppInfo" => {
                                ok(json!({"appId":"netflix","returnValue":true}))
                            }
                            "system/turnOff" => ok(json!({"returnValue":true})),
                            _ => {
                                json!({"type":"error","id":id,"error":"404 no such service or method","payload":{}})
                            }
                        };
                        let _ = ws.send(Message::text(reply.to_string())).await;
                    }
                });
            }
        });
        tv
    }

    fn driver(tv: &MockLg, dir: &std::path::Path) -> LgDriver {
        let device = HostedDevice {
            device_id: "dev_lg".into(),
            os: DeviceOs::LgTv,
            name: "Bedroom".into(),
            address: Some("127.0.0.1".into()),
            state_dir: dir.to_path_buf(),
            agent_device: vec![],
        };
        LgDriver::with_ports(
            device,
            Ports {
                plain: Some(tv.port),
                tls: None,
            },
        )
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
    async fn registers_through_the_prompt_and_runs_commands() {
        let tv = mock_lg(false).await;
        let dir = tempfile::tempdir().unwrap();
        let d = driver(&tv, dir.path());

        let p = d.probe().await;
        assert!(p.online);
        assert!(p.capabilities.is_empty());
        assert_eq!(p.setup.steps[1].status, StepStatus::NeedsCarbon);
        wait_until(|| {
            std::fs::read_to_string(dir.path().join(STATE_FILE)).is_ok_and(|s| s.contains(KEY))
        })
        .await;
        let p = d.probe().await;
        assert_eq!(
            p.setup.state,
            bridge_protocol::model::SetupState::Complete,
            "{p:?}"
        );
        assert_eq!(p.model.as_deref(), Some("OLED55C1PUB"));
        assert_eq!(p.os_version.as_deref(), Some("webOSTV 6.0 (03.21.20)"));
        assert_eq!(p.capabilities, DeviceOs::LgTv.full_capabilities().to_vec());
        // A new driver (the host app restarted) presents the saved key and gets no prompt.
        let d = driver(&tv, dir.path());
        let p = d.probe().await;
        assert!(
            p.online && p.setup.state == bridge_protocol::model::SetupState::Complete,
            "{p:?}"
        );
        assert_eq!(
            tv.registers.lock().unwrap().last().unwrap()["payload"]["client-key"],
            KEY
        );

        let w = dir.path();
        for (cmd, a) in [
            ("tv-remote", vec!["press", "up"]),
            ("tv-remote", vec!["press", "select"]),
            ("home", vec![]),
            ("back", vec![]),
        ] {
            let out = d.run(inv(cmd, &args(&a), w, &[])).await;
            assert!(out.ok, "{cmd} {a:?}: {out:?}");
        }
        let out = d
            .run(inv("tv-remote", &args(&["longpress", "up"]), w, &[]))
            .await;
        assert_eq!(out.error.unwrap().code, "unsupported_on_device");
        wait_until(|| tv.pointer.lock().unwrap().len() == 4).await;
        assert_eq!(
            *tv.pointer.lock().unwrap(),
            vec![
                "type:button\nname:UP\n\n",
                "type:button\nname:ENTER\n\n",
                "type:button\nname:HOME\n\n",
                "type:button\nname:BACK\n\n"
            ]
        );

        let out = d.run(inv("apps", &[], w, &[])).await;
        assert_eq!(out.output["apps"][1]["id"], "netflix");
        let out = d.run(inv("open", &args(&["youtube"]), w, &[])).await;
        assert!(out.ok, "{out:?}");
        let out = d
            .run(inv("open", &args(&["https://example.com"]), w, &[]))
            .await;
        assert!(out.ok, "{out:?}");
        let out = d.run(inv("open", &args(&["Hulu"]), w, &[])).await;
        assert_eq!(out.error.unwrap().code, "invalid_args");
        let out = d.run(inv("appstate", &[], w, &[])).await;
        assert_eq!(out.output["app"], "netflix");
        let out = d.run(inv("close", &[], w, &[])).await;
        assert!(out.ok, "{out:?}");
        let out = d
            .run(inv("tv-remote", &args(&["press", "power"]), w, &[]))
            .await;
        assert!(out.ok, "{out:?}");

        let reqs = tv.requests.lock().unwrap().clone();
        let find = |uri: &str| {
            reqs.iter()
                .filter(|r| r["uri"] == format!("ssap://{uri}"))
                .cloned()
                .collect::<Vec<_>>()
        };
        let launches = find("system.launcher/launch");
        assert_eq!(launches[0]["payload"], json!({"id":"youtube.leanback.v4"}));
        assert_eq!(
            launches[1]["payload"],
            json!({"id":"com.webos.app.browser","target":"https://example.com"})
        );
        assert_eq!(
            find("system.launcher/close")[0]["payload"],
            json!({"id":"netflix"})
        );
        assert_eq!(find("system/turnOff").len(), 1);
    }

    #[tokio::test]
    async fn denial_is_reported() {
        let tv = mock_lg(true).await;
        let dir = tempfile::tempdir().unwrap();
        let d = driver(&tv, dir.path());
        let _ = d.probe().await;
        wait_until(|| matches!(*d.inner.pairing.lock().unwrap(), Pairing::Denied(_))).await;
        let p = d.probe().await;
        assert_eq!(p.setup.steps[1].status, StepStatus::Failed);
        assert!(
            p.setup.steps[1]
                .error
                .as_deref()
                .unwrap()
                .contains("denied")
        );
    }
}
