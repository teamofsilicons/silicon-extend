//! What the agent is doing right now. The tray, the window, headless output and
//! `extend-agent status` all read this one value.
//!
//! `{state}/status.json` (what `extend-agent status` reads) is written from [`AgentStatus::for_file`]:
//! it never holds wake requests, their reasons, or which Silicon holds a carried device. On a
//! computer several Carbons paired, a Silicon's terminal can read that file, and those belong to
//! other sides; the tray and the window keep them in memory only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use extend_protocol::model::{InUseIndicator, MissingCapability, Setup, SetupState, SleepState};
use extend_protocol::{Capability, DeviceOs};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Starting,
    /// Unpaired: showing a pairing code.
    Enrolling,
    /// Paired and connected to Extend.
    Online,
    /// Paired, trying to reach Extend.
    Reconnecting,
    /// Another copy of the app (or a copy of a credential) took over every pair's connection.
    Superseded,
    /// Extend needs a newer app.
    UpgradeRequired,
    /// `extend-agent status` found no running agent.
    NotRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInfo {
    pub code: String,
    pub expires_at: String,
}

/// How one pair's connection is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PairPhase {
    #[default]
    Connecting,
    Online,
    Reconnecting,
    /// Another connection took over this pair: the app waits for Reconnect.
    Superseded,
    UpgradeRequired,
}

/// One Carbon's pair of this computer. Each has its own device id, name and credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PairInfo {
    pub device_id: String,
    /// The name this Carbon gave the computer.
    #[serde(default)]
    pub name: Option<String>,
    /// The Carbon it's paired to, e.g. `c:alice`.
    #[serde(default)]
    pub owner: Option<String>,
    /// The Team selected when the pair was made.
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub phase: PairPhase,
    /// Made by the app's first enrollment (the Carbon who installed Silicon Extend here).
    #[serde(default)]
    pub first_pair: Option<bool>,
    /// Silicons given access through this pair don't get the terminal: several Carbons paired
    /// this computer, and this pair isn't the first one.
    #[serde(default)]
    pub terminal_withheld: bool,
}

/// "Pair with another Carbon" while its code is up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AddingPair {
    #[serde(default)]
    pub pairing: Option<PairingInfo>,
    /// Why there is no code (the service refused, or can't be reached).
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InUseInfo {
    pub silicon_id: String,
    pub session_id: String,
    pub since: String,
    /// The pair the session runs through (whose Carbon gave the Silicon access).
    #[serde(default)]
    pub pair: Option<String>,
    /// That pair's Carbon, for "through c:alice".
    #[serde(default)]
    pub carbon: Option<String>,
    /// The session's side tag, for redacting other sides' wake requests. Never written to disk.
    #[serde(skip)]
    pub side: Option<String>,
}

/// A Silicon's request to wake this computer, as the window shows it. In memory only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WakeInfo {
    pub wake_id: String,
    /// The pair it was made through, and that pair's Carbon.
    pub pair: String,
    pub carbon: Option<String>,
    /// Absent when it's redacted: another side's request while a session runs, or one the service
    /// sent without them.
    pub silicon_id: Option<String>,
    pub reason: Option<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverInfo {
    pub session_id: String,
    pub reason: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    pub environment_id: String,
    pub name: String,
    pub state: String,
}

/// A device this computer carries (iPhone, iPad, Apple TV, Samsung or LG TV).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachedInfo {
    #[serde(default)]
    pub in_use_indicator: InUseIndicator,
    pub device_id: String,
    pub name: String,
    pub os: DeviceOs,
    pub online: bool,
    #[serde(default)]
    pub in_use: Option<InUseInfo>,
    #[serde(default)]
    pub takeover: Option<TakeoverInfo>,
    #[serde(default)]
    pub setup: Option<Setup>,
    /// Why this computer can't carry it, when it can't.
    #[serde(default)]
    pub error: Option<String>,
    /// Whether it is awake (None: it can't be told).
    #[serde(default)]
    pub awake: Option<bool>,
    #[serde(default)]
    pub sleep_state: Option<SleepState>,
    /// The pair of this computer it is carried for (whose Carbon added it).
    #[serde(default)]
    pub host: Option<String>,
    /// A Silicon asked its Carbon to wake it. In memory only.
    #[serde(skip)]
    pub wake_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentStatus {
    #[serde(default)]
    pub in_use_indicator: InUseIndicator,
    #[serde(default)]
    pub indicator_sync_pending: bool,
    pub pid: u32,
    pub app_version: String,
    pub service_url: String,
    pub phase: Phase,
    #[serde(default)]
    pub pairing: Option<PairingInfo>,
    /// Every Carbon's pair of this computer, oldest first.
    #[serde(default)]
    pub pairs: Vec<PairInfo>,
    /// "Pair with another Carbon", while it runs.
    #[serde(default)]
    pub adding_pair: Option<AddingPair>,
    #[serde(default)]
    pub environment: Option<EnvironmentInfo>,
    /// Whether this computer is awake, and why not.
    #[serde(default)]
    pub awake: Option<bool>,
    #[serde(default)]
    pub sleep_state: Option<SleepState>,
    /// Open requests to wake this computer. In memory only.
    #[serde(skip)]
    pub wake_requests: Vec<WakeInfo>,
    #[serde(default)]
    pub in_use: Option<InUseInfo>,
    #[serde(default)]
    pub takeover: Option<TakeoverInfo>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub missing: Vec<MissingCapability>,
    #[serde(default)]
    pub setup: Option<Setup>,
    #[serde(default)]
    pub attached: Vec<AttachedInfo>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub credential_store: String,
    #[serde(default)]
    pub updated_at: String,
}

impl AgentStatus {
    pub fn setup_needs_carbon(&self) -> bool {
        self.setup.as_ref().is_some_and(|s| s.state != SetupState::Complete)
    }

    /// What `status.json` holds: everything but wake requests (skipped by serde) and which Silicon
    /// holds a carried device.
    pub fn for_file(&self) -> AgentStatus {
        let mut s = self.clone();
        for a in &mut s.attached {
            a.in_use = None;
            a.takeover = None;
        }
        s
    }

    /// The Carbons this computer is paired to, oldest pair first ("c:alice and c:bob").
    pub fn owners(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in &self.pairs {
            if let Some(o) = &p.owner
                && !out.contains(o)
            {
                out.push(o.clone());
            }
        }
        out
    }

    pub fn pair(&self, device_id: &str) -> Option<&PairInfo> {
        self.pairs.iter().find(|p| p.device_id == device_id)
    }

    /// One line for the menu and headless output.
    pub fn headline(&self) -> String {
        let word = crate::sysinfo::computer_word();
        match self.phase {
            Phase::Starting => "Starting…".into(),
            Phase::NotRunning => "Silicon Extend isn't running".into(),
            Phase::Enrolling => match &self.pairing {
                Some(p) => format!("Pairing code: {}", p.code),
                None => "Getting a pairing code…".into(),
            },
            Phase::Superseded => format!("Another connection took over this {word}'s pairs"),
            Phase::UpgradeRequired => "Update Silicon Extend to keep using it".into(),
            Phase::Online | Phase::Reconnecting => {
                if let Some(u) = &self.in_use {
                    format!("{} is using this {word}", u.silicon_id)
                } else if self.phase == Phase::Reconnecting {
                    "Reconnecting to Extend…".into()
                } else if !self.owners().is_empty() {
                    format!("Paired to {}", and_list(&self.owners()))
                } else {
                    "Paired".into()
                }
            }
        }
    }
}

/// Shared, watchable status.
#[derive(Clone)]
pub struct StatusHandle {
    tx: Arc<watch::Sender<AgentStatus>>,
}

impl StatusHandle {
    pub fn new(initial: AgentStatus) -> Self {
        Self {
            tx: Arc::new(watch::channel(initial).0),
        }
    }
    pub fn get(&self) -> AgentStatus {
        self.tx.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<AgentStatus> {
        self.tx.subscribe()
    }
    /// Applies `f`; watchers wake only when something changed.
    pub fn update(&self, f: impl FnOnce(&mut AgentStatus)) {
        self.tx.send_if_modified(|s| {
            let before = s.clone();
            f(s);
            if *s != before {
                s.updated_at = now_rfc3339();
                true
            } else {
                false
            }
        });
    }
}

/// "a", "a and b", "a, b and c".
pub fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Keeps `{state}/status.json` in step with the status, for `extend-agent status`.
pub async fn persist(handle: StatusHandle, path: PathBuf) {
    let mut rx = handle.subscribe();
    loop {
        let snapshot = rx.borrow_and_update().for_file();
        if let Ok(bytes) = serde_json::to_vec_pretty(&snapshot)
            && let Err(e) = crate::config::write_private_file(&path, &bytes)
        {
            tracing::warn!("couldn't write {}: {e:#}", path.display());
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Reads the status file, marking it `not_running` when its process is gone.
pub fn read_status_file(path: &Path) -> AgentStatus {
    let mut status = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<AgentStatus>(&b).ok())
        .unwrap_or_default();
    if status.pid == 0 || !process_alive(status.pid) {
        status.phase = Phase::NotRunning;
    }
    status
}

#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: kill with signal 0 only checks whether the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// The readable text of `extend-agent status`.
pub fn render_text(s: &AgentStatus) -> String {
    let mut out = Vec::new();
    out.push(format!("Silicon Extend {}: {}", s.app_version, s.headline()));
    if s.phase == Phase::NotRunning {
        return out.join("\n");
    }
    if let Some(env) = &s.environment {
        out.push(format!("Test environment: {} ({})", env.name, env.state));
    }
    if let Some(p) = &s.pairing {
        out.push(format!(
            "Enter {} at extend.teamofsilicons.com to pair. It changes at {}.",
            p.code, p.expires_at
        ));
    }
    for p in &s.pairs {
        let name = p.name.as_deref().unwrap_or("(unnamed)");
        let state = match p.phase {
            PairPhase::Online => String::new(),
            PairPhase::Connecting | PairPhase::Reconnecting => ", reconnecting".into(),
            PairPhase::Superseded => ", taken over by another connection".into(),
            PairPhase::UpgradeRequired => ", needs an update".into(),
        };
        out.push(format!(
            "Paired to {}{}: {name} ({}){state}",
            p.owner.as_deref().unwrap_or("a Carbon"),
            p.team.as_deref().map(|t| format!(" in {t}")).unwrap_or_default(),
            p.device_id
        ));
        if p.terminal_withheld {
            out.push(format!(
                "  Silicons {} gives access to use the screen, keyboard and apps, not the terminal.",
                p.owner.as_deref().unwrap_or("this Carbon")
            ));
        }
    }
    if let Some(a) = s.awake {
        out.push(match (a, s.sleep_state) {
            (true, _) => "Awake: yes".into(),
            (false, Some(st)) => format!("Awake: no ({})", st.label()),
            (false, None) => "Awake: no".into(),
        });
    }
    if let Some(u) = &s.in_use {
        let through = u.carbon.as_deref().map(|c| format!(" through {c}")).unwrap_or_default();
        out.push(format!(
            "In use by {}{through} in session {} since {}",
            u.silicon_id, u.session_id, u.since
        ));
    }
    if let Some(t) = &s.takeover {
        out.push(format!("Waiting for you: {} (until {})", t.reason, t.expires_at));
    }
    if !s.capabilities.is_empty() {
        let caps: Vec<&str> = s.capabilities.iter().map(|c| c.as_str()).collect();
        out.push(format!("Can: {}", caps.join(", ")));
    }
    for m in &s.missing {
        out.push(format!("Missing {}: {}", m.capability.as_str(), m.reason));
    }
    if let Some(setup) = &s.setup {
        for step in &setup.steps {
            let help = step.help.as_deref().map(|h| format!(" — {h}")).unwrap_or_default();
            out.push(format!(
                "Setup [{}] {}{help}",
                serde_json::to_value(step.status)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                step.title
            ));
        }
    }
    for a in &s.attached {
        let state = if let Some(e) = &a.error {
            format!("can't be used: {e}")
        } else if let Some(u) = &a.in_use {
            format!("{} is using it", u.silicon_id)
        } else if !a.online {
            "offline".into()
        } else {
            match (a.awake, a.sleep_state) {
                (Some(false), Some(st)) => format!("online, not awake ({})", st.label()),
                (Some(false), None) => "online, not awake".into(),
                _ => "online".into(),
            }
        };
        out.push(format!(
            "Carrying {} ({}, {}): {state}",
            a.name,
            a.os.as_str(),
            a.device_id
        ));
    }
    if let Some(e) = &s.last_error {
        out.push(format!("Last problem: {e}"));
    }
    out.push(format!("Service: {}", s.service_url));
    if !s.credential_store.is_empty() {
        out.push(format!("Credential: {}", s.credential_store));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headlines() {
        let mut s = AgentStatus {
            phase: Phase::Enrolling,
            ..Default::default()
        };
        assert_eq!(s.headline(), "Getting a pairing code…");
        s.pairing = Some(PairingInfo {
            code: "4F9C2A".into(),
            expires_at: "x".into(),
        });
        assert_eq!(s.headline(), "Pairing code: 4F9C2A");
        s.phase = Phase::Online;
        s.pairs = vec![PairInfo {
            device_id: "7c1e09ab".into(),
            owner: Some("c:alice".into()),
            ..Default::default()
        }];
        assert_eq!(s.headline(), "Paired to c:alice");
        s.pairs.push(PairInfo {
            device_id: "0d44e1f2".into(),
            owner: Some("c:bob".into()),
            terminal_withheld: true,
            ..Default::default()
        });
        assert_eq!(s.headline(), "Paired to c:alice and c:bob");
        assert!(
            render_text(&s)
                .contains("Silicons c:bob gives access to use the screen, keyboard and apps, not the terminal.")
        );
        s.in_use = Some(InUseInfo {
            silicon_id: "si:chef".into(),
            session_id: "a3f".into(),
            since: "t".into(),
            pair: Some("7c1e09ab".into()),
            carbon: Some("c:alice".into()),
            side: Some("9f2c".into()),
        });
        assert!(s.headline().starts_with("si:chef is using this "));
        assert!(render_text(&s).contains("In use by si:chef through c:alice in session a3f"));
        assert_eq!(and_list(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    #[test]
    fn the_status_file_holds_no_wake_requests_or_carried_holders() {
        let holder = InUseInfo {
            silicon_id: "si:scout".into(),
            session_id: "b40".into(),
            since: "t".into(),
            pair: None,
            carbon: None,
            side: Some("side-tag".into()),
        };
        let s = AgentStatus {
            phase: Phase::Online,
            wake_requests: vec![WakeInfo {
                wake_id: "w".into(),
                pair: "7c1e09ab".into(),
                carbon: Some("c:alice".into()),
                silicon_id: Some("si:chef".into()),
                reason: Some("Check the order screen".into()),
                expires_at: "t".into(),
            }],
            attached: vec![AttachedInfo {
                in_use_indicator: InUseIndicator::Shown,
                device_id: "3f2a1b0c".into(),
                name: "Alice's iPhone".into(),
                os: DeviceOs::Ios,
                online: true,
                in_use: Some(InUseInfo {
                    silicon_id: "si:sous".into(),
                    ..holder.clone()
                }),
                takeover: None,
                setup: None,
                error: None,
                awake: None,
                sleep_state: None,
                host: Some("7c1e09ab".into()),
                wake_requested: true,
            }],
            in_use: Some(holder),
            ..Default::default()
        };
        let json = serde_json::to_string(&s.for_file()).unwrap();
        for secret in [
            "Check the order screen",
            "si:chef",
            "si:sous",
            "wake_request",
            "side-tag",
        ] {
            assert!(!json.contains(secret), "status.json says {secret:?}: {json}");
        }
        // The computer's own holder stays: `extend-agent status` shows who is using it.
        assert!(json.contains("\"in_use\":{\"silicon_id\":\"si:scout\""), "{json}");
        // The window still has everything.
        assert_eq!(s.wake_requests.len(), 1);
    }

    #[test]
    fn update_only_notifies_on_change() {
        let h = StatusHandle::new(AgentStatus::default());
        let mut rx = h.subscribe();
        rx.borrow_and_update();
        h.update(|_| {});
        assert!(!rx.has_changed().unwrap());
        h.update(|s| s.phase = Phase::Online);
        assert!(rx.has_changed().unwrap());
        assert!(!h.get().updated_at.is_empty());
    }

    #[test]
    fn missing_status_file_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_status_file(&dir.path().join("status.json")).phase,
            Phase::NotRunning
        );
        let s = AgentStatus {
            pid: std::process::id(),
            phase: Phase::Online,
            ..Default::default()
        };
        let p = dir.path().join("status.json");
        std::fs::write(&p, serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(read_status_file(&p).phase, Phase::Online);
    }
}
