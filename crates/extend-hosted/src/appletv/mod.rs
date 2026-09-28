//! Apple TV, from the Mac on the same network.
//!
//! Remote buttons and apps go over the Companion protocol ([`companion`]), which needs a one-time
//! HAP pairing: the Apple TV shows a 4-digit code, the Carbon types it on the website, and it
//! reaches us through [`Driver::setup_code`]. The credentials live in `state_dir/appletv.json`.
//! Pictures and videos go over AirPlay ([`airplay`]).
//!
//! Everything here is native Rust; nothing needs pyatv or Python at run time.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use extend_driver::{Driver, Invocation, Output, Probe};
use extend_protocol::DeviceOs;
use extend_protocol::model::{MissingCapability, Setup, SetupStep, StepStatus};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::HostedDevice;
use crate::common::{
    self, Button, OpenTarget, find_app, guarded, invalid, load_json, not_ready, offline, save_json, sleep_or_cancel,
    step, step_failure, step_help, unsupported, unsupported_command,
};

/// The network step's error when the Apple TV can't be reached.
const UNREACHABLE: &str =
    "The Apple TV can't be reached. Turn it on with its remote and connect it to the same network as this Mac.";
use crate::script;

mod airplay;
mod companion;
mod hap;
mod opack;
mod srp;

use companion::{Companion, CompanionError, Identity, hid};
use hap::{Credentials, PairSetup, PairingError};

const STATE_FILE: &str = "appletv.json";
const DEFAULT_COMPANION_PORT: u16 = 49153;
const DEFAULT_AIRPLAY_PORT: u16 = 7000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A code left on screen this long is replaced by a fresh one.
const CODE_LIFETIME: Duration = Duration::from_secs(600);
const CLIENT_MODEL: &str = "iPhone14,3";

pub(crate) fn driver(device: HostedDevice) -> Result<Box<dyn Driver>, String> {
    if !cfg!(target_os = "macos") {
        return Err("An Apple TV is carried by a Mac on the same network; this computer isn't a Mac".into());
    }
    Ok(Box::new(AppleTvDriver::new(device, Ports::default())))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    credentials: Option<Credentials>,
    /// Our pairing identifier, the same for every pairing from this Mac.
    #[serde(default)]
    pairing_id: String,
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    rp_id: String,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    companion_port: Option<u16>,
    #[serde(default)]
    airplay_port: Option<u16>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    os_version: Option<String>,
    /// The Apple TV's own id from its network announcement (AirPlay `deviceid`, else the
    /// Companion link's `rpMRtID`): the same whichever Mac or Carbon looks, so one Apple TV
    /// carried for two Carbons is known as one device.
    #[serde(default)]
    hardware_id: Option<String>,
}

/// What FetchAttentionState answers, as awake and why not: 1 is asleep (the TV is off), 2 the
/// screensaver, 3 awake, 4 idle. 0 (or no answer: old tvOS) can't be told.
fn awake_from_attention(state: Option<u64>) -> (Option<bool>, Option<extend_protocol::model::SleepState>) {
    match state {
        Some(1) => (Some(false), Some(extend_protocol::model::SleepState::Standby)),
        Some(2..=4) => (Some(true), None),
        _ => (None, None),
    }
}

/// Said when the power button is pressed on a sleeping Apple TV: Extend never wakes a device.
const ASLEEP: &str = "The Apple TV is asleep; only its Carbon can wake it (extend device wake).";

/// Fixed ports instead of mDNS (tests, or an address given as `ip:port`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Ports {
    pub companion: Option<u16>,
    pub airplay: Option<u16>,
}

struct Pending {
    conn: Companion,
    setup: PairSetup,
    since: Instant,
}

struct Inner {
    device: HostedDevice,
    ports: Ports,
    saved: Mutex<Saved>,
    conn: tokio::sync::Mutex<Option<Companion>>,
    pending: tokio::sync::Mutex<Option<Pending>>,
    showing: tokio::sync::Mutex<Option<airplay::Showing>>,
    looked_up: Mutex<Option<Instant>>,
}

pub struct AppleTvDriver {
    inner: Arc<Inner>,
}

impl AppleTvDriver {
    pub(crate) fn new(device: HostedDevice, ports: Ports) -> Self {
        let mut saved: Saved = load_json(&device.state_dir, STATE_FILE).unwrap_or_default();
        let mut changed = false;
        if saved.pairing_id.is_empty() {
            saved.pairing_id = uuid::Uuid::new_v4().to_string().to_uppercase();
            changed = true;
        }
        if saved.device_id.is_empty() {
            let b: [u8; 5] = rand::random();
            saved.device_id = format!("02:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", b[0], b[1], b[2], b[3], b[4]);
            saved.rp_id = hex::encode(rand::random::<[u8; 6]>());
            changed = true;
        }
        if changed {
            let _ = save_json(&device.state_dir, STATE_FILE, &saved);
        }
        Self {
            inner: Arc::new(Inner {
                device,
                ports,
                saved: Mutex::new(saved),
                conn: tokio::sync::Mutex::new(None),
                pending: tokio::sync::Mutex::new(None),
                showing: tokio::sync::Mutex::new(None),
                looked_up: Mutex::new(None),
            }),
        }
    }
}

fn companion_error(e: CompanionError) -> Output {
    match e {
        CompanionError::Io(e) => offline(format!("Can't reach the Apple TV: {e}")),
        CompanionError::Pairing(PairingError::NotPaired) => {
            not_ready("The Apple TV no longer recognises this Mac; add it again and enter the code it shows")
        }
        CompanionError::Refused(m) => common::failed(format!("The Apple TV refused: {m}")),
        other => common::failed(other.to_string()),
    }
}

impl Inner {
    fn saved(&self) -> Saved {
        self.saved.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut Saved)) {
        let mut s = self.saved.lock().unwrap();
        f(&mut s);
        if let Err(e) = save_json(&self.device.state_dir, STATE_FILE, &*s) {
            tracing::warn!(error = %e, "couldn't save Apple TV state");
        }
    }

    fn identity(&self) -> Identity {
        let s = self.saved();
        Identity {
            name: common::CLIENT_NAME.into(),
            model: CLIENT_MODEL.into(),
            device_id: s.device_id,
            rp_id: s.rp_id,
        }
    }

    /// Host, Companion port and AirPlay port. mDNS fills in ports (and model, tvOS version) at most
    /// every ten minutes; defaults stand in when it finds nothing.
    async fn endpoints(&self) -> Result<(String, u16, u16), String> {
        let saved = self.saved();
        let (mut host, mut companion) = match self.device.address.clone().filter(|a| !a.trim().is_empty()) {
            Some(a) => {
                let (h, p) = common::split_host_port(&a);
                (Some(h), p)
            }
            None => (saved.address.clone(), None),
        };
        companion = companion.or(self.ports.companion);
        let fixed = self.ports.companion.is_some() || self.ports.airplay.is_some();
        let stale = self
            .looked_up
            .lock()
            .unwrap()
            .is_none_or(|t| t.elapsed() > Duration::from_secs(600));
        if !fixed
            && (host.is_none()
                || (stale
                    && (saved.companion_port.is_none() || saved.airplay_port.is_none() || saved.os_version.is_none())))
        {
            *self.looked_up.lock().unwrap() = Some(Instant::now());
            let (links, airplays) = tokio::join!(
                crate::discover::browse_mdns("_companion-link._tcp.local.", Duration::from_secs(2)),
                crate::discover::browse_mdns("_airplay._tcp.local.", Duration::from_secs(2)),
            );
            let matches = |s: &crate::discover::MdnsService| match &host {
                Some(h) => s.addresses.iter().any(|a| a.to_string() == *h),
                None => s.instance.eq_ignore_ascii_case(&self.device.name),
            };
            let link = links.iter().find(|s| matches(s)).or_else(|| {
                let tvs: Vec<_> = links
                    .iter()
                    .filter(|s| s.txt.get("rpMd").is_some_and(|m| m.starts_with("AppleTV")))
                    .collect();
                if host.is_none() && tvs.len() == 1 {
                    Some(tvs[0])
                } else {
                    None
                }
            });
            if let Some(l) = link {
                if host.is_none() {
                    host = l.best_address().map(|a| a.to_string());
                }
                let model = l.txt.get("rpMd").cloned();
                let link_id = l.txt.get("rpMRtID").cloned();
                let addr = host.clone();
                self.update(|s| {
                    s.companion_port = Some(l.port);
                    s.address = addr;
                    if model.is_some() {
                        s.model = model;
                    }
                    if s.hardware_id.is_none() {
                        s.hardware_id = link_id;
                    }
                });
            }
            let ap = airplays.iter().find(|s| {
                host.as_ref()
                    .is_some_and(|h| s.addresses.iter().any(|a| a.to_string() == *h))
            });
            if let Some(a) = ap {
                let os = a.txt.get("osvers").map(|v| format!("tvOS {v}"));
                let device_id = a.txt.get("deviceid").map(|d| d.to_ascii_uppercase());
                self.update(|s| {
                    s.airplay_port = Some(a.port);
                    if os.is_some() {
                        s.os_version = os;
                    }
                    if device_id.is_some() {
                        s.hardware_id = device_id;
                    }
                });
            }
        }
        let saved = self.saved();
        let host = host.ok_or("No Apple TV found on this network. Check it's on and on the same Wi-Fi as this Mac")?;
        let companion = companion.or(saved.companion_port).unwrap_or(DEFAULT_COMPANION_PORT);
        let airplay = self
            .ports
            .airplay
            .or(saved.airplay_port)
            .unwrap_or(DEFAULT_AIRPLAY_PORT);
        Ok((host, companion, airplay))
    }

    /// Makes sure `slot` holds a verified Companion session.
    async fn ensure(&self, slot: &mut Option<Companion>) -> Result<(), CompanionError> {
        if slot.is_some() {
            return Ok(());
        }
        let creds = self
            .saved()
            .credentials
            .ok_or(CompanionError::Pairing(PairingError::NotPaired))?;
        let (host, port, _) = self
            .endpoints()
            .await
            .map_err(|e| CompanionError::Io(std::io::Error::other(e)))?;
        let mut c = Companion::connect(&host, port, CONNECT_TIMEOUT).await?;
        if let Err(e) = c.verify(&creds).await {
            if matches!(e, CompanionError::Pairing(PairingError::NotPaired)) {
                // The Apple TV forgot us (reset, or removed in its settings): pair again.
                self.update(|s| s.credentials = None);
            }
            return Err(e);
        }
        c.start_session(&self.identity(), &creds.client_id).await?;
        *slot = Some(c);
        Ok(())
    }

    /// Runs `op` on the Companion session, reconnecting once if the old connection had died.
    async fn with<T>(
        &self,
        op: impl for<'c> Fn(&'c mut Companion) -> BoxFuture<'c, Result<T, CompanionError>>,
    ) -> Result<T, CompanionError> {
        let mut slot = self.conn.lock().await;
        for attempt in 0..2 {
            self.ensure(&mut slot).await?;
            match op(slot.as_mut().expect("ensured")).await {
                Err(CompanionError::Io(_)) if attempt == 0 => *slot = None,
                Err(CompanionError::Io(e)) => {
                    *slot = None;
                    return Err(CompanionError::Io(e));
                }
                other => return other,
            }
        }
        unreachable!("the loop returns on its second pass")
    }

    /// Opens a pairing connection so the Apple TV shows a code.
    async fn start_pairing(&self) -> Result<(), String> {
        let (host, port, _) = self.endpoints().await?;
        let mut conn = Companion::connect(&host, port, CONNECT_TIMEOUT)
            .await
            .map_err(|e| format!("Can't reach the Apple TV: {e}"))?;
        let mut setup = PairSetup::new(&self.saved().pairing_id);
        match conn.pair_start(&mut setup).await {
            Ok(()) => {
                *self.pending.lock().await = Some(Pending {
                    conn,
                    setup,
                    since: Instant::now(),
                });
                Ok(())
            }
            Err(e) => Err(companion::describe(&e)),
        }
    }

    async fn press(&self, button: Button, hold: Option<Duration>, inv: &Invocation<'_>) -> Output {
        let code = match button {
            Button::Up => hid::UP,
            Button::Down => hid::DOWN,
            Button::Left => hid::LEFT,
            Button::Right => hid::RIGHT,
            Button::Select => hid::SELECT,
            Button::Back | Button::Menu => hid::MENU,
            Button::Home => hid::HOME,
            Button::PlayPause => hid::PLAY_PAUSE,
            Button::VolumeUp => hid::VOLUME_UP,
            Button::VolumeDown => hid::VOLUME_DOWN,
            Button::Mute => {
                return unsupported("The Apple TV's remote protocol has no mute button");
            }
            Button::Power => {
                // Power only ever puts an awake Apple TV to sleep. A sleeping one is left alone:
                // waking a device is its Carbon's to do.
                let r = self
                    .with(|c| {
                        Box::pin(async move {
                            let asleep = c.attention_state().await.map(|s| s == 1).unwrap_or(false);
                            if !asleep {
                                c.hid(hid::SLEEP, false).await?;
                            }
                            Ok(asleep)
                        })
                    })
                    .await;
                return match r {
                    Ok(true) => not_ready(ASLEEP),
                    Ok(false) => Output::ok(
                        json!({"button": "power", "action": "sleep"}),
                        "Sent sleep to the Apple TV",
                    ),
                    Err(e) => companion_error(e),
                };
            }
        };
        let r = match hold {
            None => {
                self.with(|c| {
                    Box::pin(async move {
                        c.hid(code, true).await?;
                        c.hid(code, false).await
                    })
                })
                .await
            }
            Some(d) => match self.with(|c| Box::pin(async move { c.hid(code, true).await })).await {
                Ok(()) => {
                    let _ = sleep_or_cancel(inv, d).await;
                    // Always release, even when cancelled mid-hold.
                    self.with(|c| Box::pin(async move { c.hid(code, false).await })).await
                }
                Err(e) => Err(e),
            },
        };
        match r {
            Ok(()) => Output::ok(
                json!({"button": button.as_str(), "held_ms": hold.map(|d| d.as_millis() as u64)}),
                match hold {
                    None => format!("Pressed {}", button.as_str()),
                    Some(d) => format!("Held {} for {} ms", button.as_str(), d.as_millis()),
                },
            ),
            Err(e) => companion_error(e),
        }
    }

    async fn apps(&self) -> Result<Vec<(String, String)>, CompanionError> {
        self.with(|c| Box::pin(async move { c.apps().await })).await
    }

    async fn open(&self, target: OpenTarget) -> Output {
        let (launch, label) = match target {
            OpenTarget::Url(u) | OpenTarget::AppWithUrl(_, u) if common::is_web_url(&u) => {
                return unsupported(format!(
                    "The Apple TV has no web browser, so it can't open {u}. For a video link use `display show --video {u}`"
                ));
            }
            OpenTarget::Url(u) | OpenTarget::AppWithUrl(_, u) => (u.clone(), u),
            OpenTarget::App(name) => {
                let apps = match self.apps().await {
                    Ok(a) => a,
                    Err(e) => return companion_error(e),
                };
                match find_app(&apps, &name, |a| &a.0, |a| &a.1) {
                    Some((id, n)) => (id.clone(), n.clone()),
                    None if name.contains('.') && !name.contains(' ') => (name.clone(), name.clone()),
                    None => {
                        let names: Vec<&str> = apps.iter().map(|a| a.1.as_str()).take(40).collect();
                        return invalid(format!(
                            "No app called \"{name}\" on this Apple TV. Installed: {}",
                            names.join(", ")
                        ));
                    }
                }
            }
        };
        let l = launch.clone();
        match self
            .with(move |c| {
                let l = l.clone();
                Box::pin(async move { c.launch(&l).await })
            })
            .await
        {
            Ok(()) => Output::ok(json!({"app": launch, "name": label}), format!("Opened {label}")),
            Err(e) => companion_error(e),
        }
    }

    async fn display(&self, args: &[String], inv: &Invocation<'_>) -> Output {
        let mut it = args.iter().map(String::as_str).filter(|a| *a != "--json");
        match it.next() {
            Some("clear") => {
                return match self.showing.lock().await.take() {
                    Some(s) => {
                        s.stop().await;
                        Output::ok(json!({"cleared": true}), "Cleared the Apple TV screen")
                    }
                    None => Output::ok(json!({"cleared": false}), "Nothing was showing"),
                };
            }
            Some("show") => {}
            _ => {
                return invalid("usage: display show --image <file|url> | --video <file|url>  or  display clear");
            }
        }
        let (kind, value) = match (it.next(), it.next(), it.next()) {
            (Some(k @ ("--image" | "--video" | "--url" | "--text")), Some(v), None) => (k, v.to_owned()),
            _ => return invalid("usage: display show --image <file|url> | --video <file|url>"),
        };
        if matches!(kind, "--url" | "--text") {
            return unsupported("An Apple TV shows pictures and videos only (`--image` or `--video`)");
        }
        let (host, _, port) = match self.endpoints().await {
            Ok(e) => e,
            Err(e) => return offline(e),
        };
        let creds = self.saved().credentials;
        if let Some(old) = self.showing.lock().await.take() {
            old.stop().await;
        }
        let file = if common::is_web_url(&value) {
            None
        } else {
            common::resolve_attachment(&value, inv.attachments)
        };
        if !common::is_web_url(&value) && file.is_none() {
            return invalid(format!("{value} wasn't sent with the command"));
        }
        let result = if kind == "--video" {
            let media = match file {
                Some(p) => airplay::Media::File(p),
                None => airplay::Media::Url(value.clone()),
            };
            airplay::play_video(&host, port, media, creds.as_ref(), common::CLIENT_NAME).await
        } else {
            let (bytes, content_type) = match file {
                Some(p) => match std::fs::read(&p) {
                    Ok(b) => (b, common::content_type_for(&p).to_owned()),
                    Err(e) => return invalid(format!("can't read {value}: {e}")),
                },
                None => match crate::http::request("GET", &value, &[], b"", Duration::from_secs(30), false).await {
                    Ok(r) if r.is_success() => {
                        let ct = r.header("content-type").unwrap_or("image/jpeg").to_owned();
                        (r.body, ct)
                    }
                    Ok(r) => {
                        return common::failed(format!("Downloading {value} failed with HTTP {}", r.status()));
                    }
                    Err(e) => return common::failed(format!("Downloading {value} failed: {e}")),
                },
            };
            airplay::show_photo(&host, port, bytes, &content_type, creds.as_ref()).await
        };
        match result {
            Ok(showing) => {
                let what = showing.what.clone();
                *self.showing.lock().await = Some(showing);
                Output::ok(
                    json!({"showing": kind.trim_start_matches("--"), "source": value, "airplay": what}),
                    format!("Showing {value} on the Apple TV"),
                )
            }
            Err(e) => common::failed(e),
        }
    }
}

#[async_trait]
impl Driver for AppleTvDriver {
    async fn probe(&self) -> Probe {
        let me = &self.inner;
        let full = DeviceOs::Tvos.full_capabilities();
        let network_title = "The Apple TV is on and on the same network as this Mac";
        let code_title = "Enter the code the Apple TV shows";
        let mut online = false;
        let mut attention: Option<u64> = None;
        let (network, code): (SetupStep, SetupStep) = match me.endpoints().await {
            Err(e) => (
                step_failure("network", network_title, StepStatus::NeedsCarbon, UNREACHABLE, e),
                step("code", code_title, StepStatus::Todo),
            ),
            Ok(_) if me.saved().credentials.is_some() => {
                match me.with(|c| Box::pin(async move { c.attention_state().await })).await {
                    // Old tvOS versions don't answer FetchAttentionState; a refusal still proves the session works.
                    Ok(state) => {
                        online = true;
                        attention = Some(state);
                        (
                            step("network", network_title, StepStatus::Done),
                            step("code", code_title, StepStatus::Done),
                        )
                    }
                    Err(CompanionError::Refused(_)) => {
                        online = true;
                        (
                            step("network", network_title, StepStatus::Done),
                            step("code", code_title, StepStatus::Done),
                        )
                    }
                    Err(CompanionError::Pairing(PairingError::NotPaired)) => {
                        // Credentials were just dropped; pairing starts on the next probe.
                        (
                            step("network", network_title, StepStatus::Done),
                            code_step(code_title, None),
                        )
                    }
                    Err(e) => (
                        step_failure(
                            "network",
                            network_title,
                            StepStatus::NeedsCarbon,
                            "The Apple TV didn't answer this Mac. Check it is on the same Wi-Fi or network as this Mac.",
                            e,
                        ),
                        step("code", code_title, StepStatus::Done),
                    ),
                }
            }
            Ok(_) => {
                let expired = me
                    .pending
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|p| p.since.elapsed() > CODE_LIFETIME);
                let showing = me.pending.lock().await.is_some() && !expired;
                let started = if showing { Ok(()) } else { me.start_pairing().await };
                match started {
                    Ok(()) => {
                        online = true;
                        (
                            step("network", network_title, StepStatus::Done),
                            code_step(code_title, None),
                        )
                    }
                    Err(e) if e.contains("Can't reach") => (
                        step_failure("network", network_title, StepStatus::NeedsCarbon, UNREACHABLE, e),
                        step("code", code_title, StepStatus::Todo),
                    ),
                    Err(e) => {
                        online = true;
                        (
                            step("network", network_title, StepStatus::Done),
                            code_step(code_title, Some(e)),
                        )
                    }
                }
            }
        };
        let paired = me.saved().credentials.is_some();
        let ready = online && paired;
        let saved = me.saved();
        let (awake, sleep_state) = awake_from_attention(attention);
        let reason = if !online {
            "The Apple TV can't be reached"
        } else {
            "Waiting for the code the Apple TV shows"
        };
        Probe {
            os: DeviceOs::Tvos,
            os_version: saved.os_version,
            model: saved.model,
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
            setup: Setup::from_steps(vec![network, code]),
            engine_version: None,
            online,
            awake,
            sleep_state,
            hardware_id: saved.hardware_id.clone(),
        }
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        let me = &self.inner;
        if me.saved().credentials.is_none() && !matches!(inv.command, "replay" | "test" | "batch") {
            return not_ready("This Apple TV isn't paired yet: enter the code it shows on the website");
        }
        guarded(&inv, async {
            match inv.command {
                "tv-remote" => match common::parse_tv_remote(inv.args) {
                    Ok(p) => me.press(p.button, p.hold, &inv).await,
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
                        &inv,
                    )
                    .await
                }
                "back" | "home" => invalid(format!("`{}` takes no arguments on a TV", inv.command)),
                "app-switcher" => {
                    let r = me
                        .with(|c| {
                            Box::pin(async move {
                                for _ in 0..2 {
                                    c.hid(hid::HOME, true).await?;
                                    c.hid(hid::HOME, false).await?;
                                }
                                Ok(())
                            })
                        })
                        .await;
                    match r {
                        Ok(()) => Output::ok(json!({"button": "home", "count": 2}), "Opened the app switcher"),
                        Err(e) => companion_error(e),
                    }
                }
                "open" => match common::parse_open(inv.args) {
                    Ok(t) => me.open(t).await,
                    Err(e) => invalid(e),
                },
                "close" => match common::parse_close(inv.args) {
                    // Companion can't quit an app; Home puts it in the background.
                    Ok(_) => match me
                        .with(|c| {
                            Box::pin(async move {
                                c.hid(hid::HOME, true).await?;
                                c.hid(hid::HOME, false).await
                            })
                        })
                        .await
                    {
                        Ok(()) => Output::ok(
                            json!({"closed": false, "home": true}),
                            "The Apple TV can't quit apps remotely; went to the Home screen instead",
                        ),
                        Err(e) => companion_error(e),
                    },
                    Err(e) => invalid(e),
                },
                "apps" => match me.apps().await {
                    Ok(apps) => {
                        let text = apps
                            .iter()
                            .map(|(id, n)| format!("{n}  {id}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let list: Vec<_> = apps.iter().map(|(id, n)| json!({"id": id, "name": n})).collect();
                        Output::ok(json!({"apps": list}), text)
                    }
                    Err(e) => companion_error(e),
                },
                "appstate" => unsupported("The Apple TV doesn't say which app is in front over its remote protocol"),
                "display" => me.display(inv.args, &inv).await,
                "replay" | "test" | "batch" => script::run(self, &inv).await,
                other => unsupported_command(other, "an Apple TV"),
            }
        })
        .await
    }

    async fn session_ended(&self, _session_id: &str) {
        if let Some(s) = self.inner.showing.lock().await.take() {
            s.stop().await;
        }
    }

    /// Retry: look for the Apple TV on the network again, and put up a fresh code if it is
    /// waiting for one (the probe that follows does both at once).
    async fn retry_setup(&self, _step: Option<&str>) {
        let me = &self.inner;
        *me.looked_up.lock().unwrap() = None;
        *me.conn.lock().await = None;
        if me.saved().credentials.is_none() {
            *me.pending.lock().await = None;
        }
    }

    async fn setup_code(&self, code: &str) -> Result<(), String> {
        let me = &self.inner;
        let digits: String = code.chars().filter(char::is_ascii_digit).collect();
        if digits.len() != 4 {
            return Err("The Apple TV's code is 4 digits".into());
        }
        let pending = me.pending.lock().await.take();
        let Some(mut p) = pending else {
            me.start_pairing().await?;
            return Err("There was no code on the Apple TV's screen. It shows one now; enter that code".into());
        };
        match p.conn.pair_finish(&mut p.setup, &digits, common::CLIENT_NAME).await {
            Ok((creds, info)) => {
                let model = info
                    .as_ref()
                    .and_then(|i| i.get("model"))
                    .and_then(|m| m.as_str())
                    .map(str::to_owned);
                me.update(|s| {
                    s.credentials = Some(creds);
                    if model.is_some() {
                        s.model = model;
                    }
                });
                *me.conn.lock().await = None;
                Ok(())
            }
            Err(e) => {
                let why = companion::describe(&e);
                // The Apple TV ends a pairing attempt after an error; ask for a fresh code.
                match me.start_pairing().await {
                    Ok(()) => Err(format!("{why} The Apple TV now shows a new code; enter that one")),
                    Err(again) => {
                        tracing::info!("couldn't ask the Apple TV for a new code: {again}");
                        Err(format!(
                            "{why} The Apple TV didn't put up a new code; keep it awake on its home screen and tap Retry."
                        ))
                    }
                }
            }
        }
    }
}

fn code_step(title: &str, error: Option<String>) -> SetupStep {
    let mut s = match error {
        Some(e) => step_failure(
            "code",
            title,
            StepStatus::NeedsCarbon,
            "The Apple TV didn't put up a code. Keep it awake on its home screen; Extend asks again shortly, or tap Retry.",
            e,
        ),
        None => step_help(
            "code",
            title,
            StepStatus::NeedsCarbon,
            "The Apple TV shows a 4-digit code. Type it here.",
        ),
    };
    s.input = Some("code".into());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samsung::tests::{args, inv};

    fn driver(dir: &std::path::Path, companion: u16, airplay: u16) -> AppleTvDriver {
        let device = HostedDevice {
            device_id: "dev_atv".into(),
            os: DeviceOs::Tvos,
            name: "Living Room".into(),
            address: Some("127.0.0.1".into()),
            state_dir: dir.to_path_buf(),
            agent_device: vec![],
        };
        AppleTvDriver::new(
            device,
            Ports {
                companion: Some(companion),
                airplay: Some(airplay),
            },
        )
    }

    #[tokio::test]
    async fn pairs_with_the_code_and_runs_commands() {
        let tv = companion::mock::start("4821").await;
        let rx = airplay::mock::start().await;
        let dir = tempfile::tempdir().unwrap();
        let d = driver(dir.path(), tv.port, rx.port);

        // Unpaired: the code step asks for input and the Apple TV shows a code.
        let p = d.probe().await;
        assert!(p.online);
        assert!(p.capabilities.is_empty());
        let code = &p.setup.steps[1];
        assert_eq!(
            (code.key.as_str(), code.status, code.input.as_deref()),
            ("code", StepStatus::NeedsCarbon, Some("code"))
        );
        assert_eq!(*tv.pins_shown.lock().unwrap(), 1);
        // Probing again doesn't put up another code.
        let _ = d.probe().await;
        assert_eq!(*tv.pins_shown.lock().unwrap(), 1);
        assert_eq!(
            d.run(inv("home", &[], dir.path(), &[])).await.error.unwrap().code,
            "device_not_ready"
        );

        // A wrong code: refused, and a fresh code goes up.
        let err = d.setup_code("1111").await.unwrap_err();
        assert!(err.contains("didn't match") && err.contains("new code"), "{err}");
        assert_eq!(*tv.pins_shown.lock().unwrap(), 2);
        assert!(d.setup_code("12").await.unwrap_err().contains("4 digits"));
        d.setup_code("4821").await.unwrap();
        let saved = std::fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(saved.contains("\"ltpk\""), "{saved}");

        let p = d.probe().await;
        assert_eq!(p.setup.state, extend_protocol::model::SetupState::Complete, "{p:?}");
        assert_eq!(p.capabilities, DeviceOs::Tvos.full_capabilities().to_vec());
        assert_eq!(p.model.as_deref(), Some("AppleTV14,1"));
        assert_eq!((p.awake, p.sleep_state), (Some(true), None));

        let w = dir.path();
        let ok = |o: Output| assert!(o.ok, "{o:?}");
        ok(d.run(inv("tv-remote", &args(&["press", "select"]), w, &[])).await);
        ok(d.run(inv(
            "tv-remote",
            &args(&["longpress", "right", "--duration-ms", "40"]),
            w,
            &[],
        ))
        .await);
        ok(d.run(inv("back", &[], w, &[])).await);
        ok(d.run(inv("tv-remote", &args(&["press", "power"]), w, &[])).await);
        assert_eq!(
            d.run(inv("tv-remote", &args(&["press", "mute"]), w, &[]))
                .await
                .error
                .unwrap()
                .code,
            "unsupported_on_device"
        );
        let out = d.run(inv("apps", &[], w, &[])).await;
        assert_eq!(out.output["apps"][0]["name"], "Netflix");
        ok(d.run(inv("open", &args(&["netflix"]), w, &[])).await);
        ok(d.run(inv("open", &args(&["youtube://watch?v=abc"]), w, &[])).await);
        assert_eq!(
            d.run(inv("open", &args(&["https://example.com"]), w, &[]))
                .await
                .error
                .unwrap()
                .code,
            "unsupported_on_device"
        );
        assert_eq!(
            d.run(inv("open", &args(&["Disney+"]), w, &[]))
                .await
                .error
                .unwrap()
                .code,
            "invalid_args"
        );
        ok(d.run(inv("close", &[], w, &[])).await);

        // Asleep: it says so, and power is refused (nothing ever wakes it) while every other
        // command still goes to it as usual.
        *tv.attention.lock().unwrap() = 1;
        let p = d.probe().await;
        assert!(p.online);
        assert_eq!(
            (p.awake, p.sleep_state),
            (Some(false), Some(extend_protocol::model::SleepState::Standby))
        );
        let power = d.run(inv("tv-remote", &args(&["press", "power"]), w, &[])).await;
        assert!(!power.ok);
        assert_eq!(power.error.as_ref().unwrap().message, ASLEEP);
        *tv.attention.lock().unwrap() = 3;

        let clip = w.join("clip.mp4");
        std::fs::write(&clip, vec![1u8; 2048]).unwrap();
        let att = vec![clip.clone()];
        let path = clip.to_string_lossy().to_string();
        ok(d.run(inv("display", &args(&["show", "--video", &path]), w, &att)).await);
        assert!(rx.played.lock().unwrap()[0].ends_with("/clip.mp4"));
        let out = d.run(inv("display", &args(&["show", "--text", "hi"]), w, &[])).await;
        assert_eq!(out.error.unwrap().code, "unsupported_on_device");
        ok(d.run(inv("display", &args(&["clear"]), w, &[])).await);

        let log = tv.log.lock().unwrap().clone();
        let hids: Vec<(u64, u64)> = log
            .iter()
            .filter(|(n, _)| n == "_hidC")
            .map(|(_, c)| (c["_hidC"].as_u64().unwrap(), c["_hBtS"].as_u64().unwrap()))
            .collect();
        // select click, right hold, back (menu) click, power (sleep, release only), home click (close).
        assert_eq!(
            hids,
            vec![(6, 1), (6, 2), (4, 1), (4, 2), (5, 1), (5, 2), (12, 2), (7, 1), (7, 2)]
        );
        assert!(log.contains(&("_launchApp".into(), json!({"_bundleID": "com.netflix.Netflix"}))));
        assert!(log.contains(&("_launchApp".into(), json!({"_urlS": "youtube://watch?v=abc"}))));

        // The Apple TV forgets us: the next probe drops the credentials and asks for a new code.
        tv.accessory.lock().unwrap().paired.clear();
        *d.inner.conn.lock().await = None;
        let _ = d.probe().await;
        let p = d.probe().await;
        assert_eq!(p.setup.steps[1].status, StepStatus::NeedsCarbon, "{p:?}");
        assert!(d.inner.saved().credentials.is_none());
        assert_eq!(*tv.pins_shown.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn unreachable_apple_tv() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver(dir.path(), 1, 2);
        let p = d.probe().await;
        assert!(!p.online);
        assert_eq!(p.setup.steps[0].status, StepStatus::NeedsCarbon);
    }
}
