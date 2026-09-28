//! The driver for the computer the agent runs on: the platform driver plus the terminal.

use std::sync::Arc;

use async_trait::async_trait;
use extend_driver::{Driver, Invocation, Output, Probe};
use extend_protocol::Capability;

use crate::config::Config;
use crate::drivers::terminal;

pub struct LocalDriver {
    platform: Arc<dyn Driver>,
}

impl LocalDriver {
    pub fn new(platform: Arc<dyn Driver>) -> Self {
        Self { platform }
    }

    /// The device engine on Mac and Linux, Extend's own driver on Windows, for a one-off run
    /// (`extend-agent probe`, `extend-agent exec`).
    pub fn for_this_computer(config: &Config) -> Self {
        Self::new(platform_driver(config, false))
    }

    /// The same for the long-running agent, which may also force a stuck session's release at
    /// session boundaries and retries failed session cleanup in the background (one-off runs
    /// share its device-engine daemon and leave both to it).
    pub fn for_the_agent(config: &Config) -> Self {
        Self::new(platform_driver(config, true))
    }
}

#[cfg(target_os = "macos")]
fn platform_driver(config: &Config, agent: bool) -> Arc<dyn Driver> {
    use crate::drivers::{agent_device::AgentDeviceDriver, probe_macos};
    let driver = AgentDeviceDriver::new(
        config.agent_device.clone(),
        config.agent_device_problem.clone(),
        "macos",
        config.agent_device_state_dir(),
        config.session_data_dir(),
        Arc::new(|input| probe_macos::build_probe(&probe_macos::gather(), input)),
    );
    Arc::new(if agent { driver.for_the_agent() } else { driver })
}

#[cfg(target_os = "linux")]
fn platform_driver(config: &Config, agent: bool) -> Arc<dyn Driver> {
    use crate::drivers::{agent_device::AgentDeviceDriver, probe_linux};
    let driver = AgentDeviceDriver::new(
        config.agent_device.clone(),
        config.agent_device_problem.clone(),
        "linux",
        config.agent_device_state_dir(),
        config.session_data_dir(),
        Arc::new(|input| probe_linux::build_probe(&probe_linux::gather(), input)),
    );
    Arc::new(if agent { driver.for_the_agent() } else { driver })
}

#[cfg(windows)]
fn platform_driver(config: &Config, _agent: bool) -> Arc<dyn Driver> {
    Arc::new(crate::drivers::windows::WindowsDriver::new(
        config.state_dir.join("windows"),
    ))
}

/// Adds the terminal, which every computer has.
pub fn with_terminal(mut p: Probe) -> Probe {
    if !p.capabilities.contains(&Capability::Terminal) {
        p.capabilities.push(Capability::Terminal);
        p.capabilities.sort();
    }
    p.missing.retain(|m| m.capability != Capability::Terminal);
    p
}

#[async_trait]
impl Driver for LocalDriver {
    async fn probe(&self) -> Probe {
        with_terminal(self.platform.probe().await)
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        if inv.command == "terminal" {
            return terminal::run(&inv).await;
        }
        self.platform.run(inv).await
    }

    async fn session_started(&self, session_id: &str) {
        self.platform.session_started(session_id).await;
    }

    async fn session_ended(&self, session_id: &str) {
        // Whatever the session's terminal left running ends with it (an empty id is "no session
        // here": nothing of a particular session to end).
        if !session_id.is_empty() {
            let id = session_id.to_owned();
            let _ = tokio::task::spawn_blocking(move || terminal::end_session(&id)).await;
        }
        self.platform.session_ended(session_id).await;
    }

    async fn setup_code(&self, code: &str) -> Result<(), String> {
        self.platform.setup_code(code).await
    }

    async fn retry_setup(&self, step: Option<&str>) {
        self.platform.retry_setup(step).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extend_protocol::model::{MissingCapability, Setup};

    #[test]
    fn terminal_is_always_there() {
        let p = Probe {
            os: extend_protocol::DeviceOs::Linux,
            os_version: None,
            model: None,
            capabilities: vec![Capability::Replay, Capability::AppsLaunch],
            missing: vec![MissingCapability {
                capability: Capability::Terminal,
                reason: "x".into(),
            }],
            setup: Setup::complete(),
            engine_version: None,
            online: true,
            awake: None,
            sleep_state: None,
            hardware_id: None,
        };
        let p = with_terminal(p);
        assert_eq!(
            p.capabilities,
            vec![Capability::AppsLaunch, Capability::Replay, Capability::Terminal]
        );
        assert!(p.missing.is_empty());
    }
}
