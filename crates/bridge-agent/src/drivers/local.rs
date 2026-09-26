//! The driver for the computer the agent runs on: the platform driver plus the terminal.

use std::sync::Arc;

use async_trait::async_trait;
use bridge_driver::{Driver, Invocation, Output, Probe};
use bridge_protocol::Capability;

use crate::config::Config;
use crate::drivers::terminal;

pub struct LocalDriver {
    platform: Arc<dyn Driver>,
}

impl LocalDriver {
    pub fn new(platform: Arc<dyn Driver>) -> Self {
        Self { platform }
    }

    /// agent-device on Mac and Linux, Bridge's own driver on Windows.
    pub fn for_this_computer(config: &Config) -> Self {
        Self::new(platform_driver(config))
    }
}

#[cfg(target_os = "macos")]
fn platform_driver(config: &Config) -> Arc<dyn Driver> {
    use crate::drivers::{agent_device::AgentDeviceDriver, probe_macos};
    Arc::new(AgentDeviceDriver::new(
        config.agent_device.clone(),
        config.agent_device_problem.clone(),
        "macos",
        config.agent_device_state_dir(),
        config.session_data_dir(),
        Arc::new(|input| probe_macos::build_probe(&probe_macos::gather(), input)),
    ))
}

#[cfg(target_os = "linux")]
fn platform_driver(config: &Config) -> Arc<dyn Driver> {
    use crate::drivers::{agent_device::AgentDeviceDriver, probe_linux};
    Arc::new(AgentDeviceDriver::new(
        config.agent_device.clone(),
        config.agent_device_problem.clone(),
        "linux",
        config.agent_device_state_dir(),
        config.session_data_dir(),
        Arc::new(|input| probe_linux::build_probe(&probe_linux::gather(), input)),
    ))
}

#[cfg(windows)]
fn platform_driver(config: &Config) -> Arc<dyn Driver> {
    Arc::new(crate::drivers::windows::WindowsDriver::new(config.state_dir.join("windows")))
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
        self.platform.session_ended(session_id).await;
    }

    async fn setup_code(&self, code: &str) -> Result<(), String> {
        self.platform.setup_code(code).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_protocol::model::{MissingCapability, Setup};

    #[test]
    fn terminal_is_always_there() {
        let p = Probe {
            os: bridge_protocol::DeviceOs::Linux,
            os_version: None,
            model: None,
            capabilities: vec![Capability::Replay, Capability::AppsLaunch],
            missing: vec![MissingCapability { capability: Capability::Terminal, reason: "x".into() }],
            setup: Setup::complete(),
            agent_device_version: None,
            online: true,
        };
        let p = with_terminal(p);
        assert_eq!(p.capabilities, vec![Capability::AppsLaunch, Capability::Replay, Capability::Terminal]);
        assert!(p.missing.is_empty());
    }
}
