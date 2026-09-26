//! What a Windows computer reports it can do, given what the probe found.

use bridge_driver::Probe;
use bridge_protocol::DeviceOs;
use bridge_protocol::model::{MissingCapability, Setup, SetupStep, StepStatus};
use bridge_protocol::Capability as C;

pub const LOCKED_REASON: &str = "This computer is locked. Unlock it to let a Silicon use it.";

/// What this build does on Windows when the computer is unlocked. `terminal` is added by the agent.
pub const WORKING: &[C] = &[
    C::ScreenRead,
    C::ScreenCapture,
    C::InputPointer,
    C::InputText,
    C::AppsLaunch,
    C::AppsList,
    C::Clipboard,
    C::Links,
    C::Takeover,
];

/// Windows capabilities this build doesn't have yet, with why.
pub const NOT_BUILT: &[(C, &str)] = &[
    (C::ScreenRecord, "Screen recording isn't available in Silicon Bridge for Windows yet."),
    (C::Logs, "Device logs aren't available in Silicon Bridge for Windows yet."),
    (C::Alerts, "Handling system pop-ups isn't available in Silicon Bridge for Windows yet; the Silicon reads and clicks dialogs instead."),
    (C::Replay, "Replaying saved steps isn't available in Silicon Bridge for Windows yet."),
];

/// Capabilities a locked computer still offers (they don't touch the screen).
const WHILE_LOCKED: &[C] = &[C::AppsList, C::Takeover];

pub fn probe_from(locked: bool, os_version: Option<String>) -> Probe {
    let mut capabilities = Vec::new();
    let mut missing: Vec<MissingCapability> = Vec::new();
    for &cap in WORKING {
        if !locked || WHILE_LOCKED.contains(&cap) {
            capabilities.push(cap);
        } else {
            missing.push(MissingCapability { capability: cap, reason: LOCKED_REASON.into() });
        }
    }
    for (cap, reason) in NOT_BUILT {
        missing.push(MissingCapability { capability: *cap, reason: (*reason).into() });
    }
    let setup = if locked {
        Setup::from_steps(vec![SetupStep {
            key: "unlocked".into(),
            title: "Unlock this computer".into(),
            status: StepStatus::NeedsCarbon,
            help: Some("Sign in to Windows. A Silicon can't see or use a locked computer, and admin prompts always need you.".into()),
            error: None,
            input: None,
        }])
    } else {
        Setup::complete()
    };
    Probe {
        os: DeviceOs::Windows,
        os_version,
        model: None,
        capabilities,
        missing,
        setup,
        agent_device_version: None,
        online: true,
    }
}

/// The version number from `cmd /c ver` ("Microsoft Windows [Version 10.0.22631.4602]").
pub fn parse_ver(output: &str) -> Option<String> {
    let start = output.find("Version ")? + "Version ".len();
    let rest = &output[start..];
    let end = rest.find(']').unwrap_or(rest.len());
    let v = rest[..end].trim();
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_protocol::model::SetupState;

    #[test]
    fn unlocked_probe() {
        let p = probe_from(false, Some("10.0.22631".into()));
        assert_eq!(p.os, DeviceOs::Windows);
        assert!(p.capabilities.contains(&C::ScreenRead));
        assert!(p.capabilities.contains(&C::InputText));
        assert!(!p.capabilities.contains(&C::Terminal), "the agent adds terminal");
        assert!(p.missing.iter().any(|m| m.capability == C::ScreenRecord));
        assert_eq!(p.setup.state, SetupState::Complete);
        // Everything reported is within what a Windows computer can have.
        for c in &p.capabilities {
            assert!(DeviceOs::Windows.full_capabilities().contains(c), "{c:?}");
        }
        for m in &p.missing {
            assert!(DeviceOs::Windows.full_capabilities().contains(&m.capability), "{:?}", m.capability);
        }
    }

    #[test]
    fn locked_probe() {
        let p = probe_from(true, None);
        assert_eq!(p.capabilities, vec![C::AppsList, C::Takeover]);
        let screen = p.missing.iter().find(|m| m.capability == C::ScreenRead).unwrap();
        assert_eq!(screen.reason, LOCKED_REASON);
        assert_eq!(p.setup.state, SetupState::NeedsCarbon);
        assert!(p.online);
    }

    #[test]
    fn parses_ver() {
        assert_eq!(parse_ver("\r\nMicrosoft Windows [Version 10.0.22631.4602]\r\n"), Some("10.0.22631.4602".into()));
        assert_eq!(parse_ver("garbage"), None);
    }
}
