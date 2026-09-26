//! What the agent is doing right now. The tray, the window, headless output and
//! `extend-agent status` all read this one value.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use extend_protocol::model::{MissingCapability, Setup, SetupState};
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
    /// Another copy of the app took over this computer's connection.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeviceInfo {
    pub device_id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// The Carbon it's paired to, e.g. `c:alice`.
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InUseInfo {
    pub silicon_id: String,
    pub session_id: String,
    pub since: String,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentStatus {
    pub pid: u32,
    pub app_version: String,
    pub service_url: String,
    pub phase: Phase,
    #[serde(default)]
    pub pairing: Option<PairingInfo>,
    #[serde(default)]
    pub device: Option<DeviceInfo>,
    #[serde(default)]
    pub environment: Option<EnvironmentInfo>,
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
            Phase::Superseded => format!("Another copy of Silicon Extend is connected for this {word}"),
            Phase::UpgradeRequired => "Update Silicon Extend to keep using it".into(),
            Phase::Online | Phase::Reconnecting => {
                if let Some(u) = &self.in_use {
                    format!("{} is using this {word}", u.silicon_id)
                } else if self.phase == Phase::Reconnecting {
                    "Reconnecting to Extend…".into()
                } else if let Some(owner) = self.device.as_ref().and_then(|d| d.owner.as_deref()) {
                    format!("Paired to {owner}")
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

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Keeps `{state}/status.json` in step with the status, for `extend-agent status`.
pub async fn persist(handle: StatusHandle, path: PathBuf) {
    let mut rx = handle.subscribe();
    loop {
        let snapshot = rx.borrow_and_update().clone();
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
    if let Some(d) = &s.device {
        let name = d.name.as_deref().unwrap_or("(unnamed)");
        out.push(format!("Device: {name} ({})", d.device_id));
        if let Some(o) = &d.owner {
            out.push(format!(
                "Paired to: {o}{}",
                d.team.as_deref().map(|t| format!(" in {t}")).unwrap_or_default()
            ));
        }
    }
    if let Some(u) = &s.in_use {
        out.push(format!(
            "In use by {} in session {} since {}",
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
        } else if a.online {
            "online".into()
        } else {
            "offline".into()
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
        s.device = Some(DeviceInfo {
            device_id: "7c1e09ab".into(),
            owner: Some("c:alice".into()),
            ..Default::default()
        });
        assert_eq!(s.headline(), "Paired to c:alice");
        s.in_use = Some(InUseInfo {
            silicon_id: "si:chef".into(),
            session_id: "a3f".into(),
            since: "t".into(),
        });
        assert!(s.headline().starts_with("si:chef is using this "));
        assert!(render_text(&s).contains("In use by si:chef in session a3f"));
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
