//! What a Windows computer reports it can do, given what the probe found.

use extend_driver::Probe;
use extend_protocol::Capability as C;
use extend_protocol::DeviceOs;
use extend_protocol::model::{MissingCapability, Setup, SetupStep, StepStatus};

use crate::drivers::screen_lock::ScreenBlock;

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
    (
        C::ScreenRecord,
        "Screen recording isn't available in Silicon Extend for Windows yet.",
    ),
    (
        C::Logs,
        "Device logs aren't available in Silicon Extend for Windows yet.",
    ),
    (
        C::Alerts,
        "Handling system pop-ups isn't available in Silicon Extend for Windows yet; the Silicon reads and clicks dialogs instead.",
    ),
    (
        C::Replay,
        "Replaying saved steps isn't available in Silicon Extend for Windows yet.",
    ),
];

/// Capabilities a locked computer still offers (they don't touch the screen).
const WHILE_LOCKED: &[C] = &[C::AppsList, C::Takeover];

/// What the computer can do given what blocks its screen (`None`: nothing does).
pub fn probe_from(block: Option<ScreenBlock>, os_version: Option<String>) -> Probe {
    let mut capabilities = Vec::new();
    let mut missing: Vec<MissingCapability> = Vec::new();
    for &cap in WORKING {
        match block {
            Some(b) if !WHILE_LOCKED.contains(&cap) => missing.push(MissingCapability {
                capability: cap,
                reason: b.reason(),
            }),
            _ => capabilities.push(cap),
        }
    }
    for (cap, reason) in NOT_BUILT {
        missing.push(MissingCapability {
            capability: *cap,
            reason: (*reason).into(),
        });
    }
    // A lock screen or another account needs the Carbon; an admin prompt is the Carbon at work.
    let setup = if matches!(block, Some(ScreenBlock::Locked | ScreenBlock::OtherSession)) {
        Setup::from_steps(vec![SetupStep {
            key: "unlocked".into(),
            title: "Unlock this computer".into(),
            status: StepStatus::NeedsCarbon,
            help: Some(
                "Sign in to Windows. A Silicon can't see or use a locked computer, and admin prompts always need you."
                    .into(),
            ),
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
        engine_version: None,
        online: true,
        awake: None,
        sleep_state: None,
        hardware_id: None,
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
    use extend_protocol::model::SetupState;

    #[test]
    fn unlocked_probe() {
        let p = probe_from(None, Some("10.0.22631".into()));
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
            assert!(
                DeviceOs::Windows.full_capabilities().contains(&m.capability),
                "{:?}",
                m.capability
            );
        }
    }

    #[test]
    fn locked_probe() {
        let p = probe_from(Some(ScreenBlock::Locked), None);
        assert_eq!(p.capabilities, vec![C::AppsList, C::Takeover]);
        let screen = p.missing.iter().find(|m| m.capability == C::ScreenRead).unwrap();
        assert_eq!(screen.reason, ScreenBlock::Locked.reason());
        assert!(
            screen.reason.contains("Only its Carbon can unlock it"),
            "{}",
            screen.reason
        );
        assert_eq!(p.setup.state, SetupState::NeedsCarbon);
        assert!(p.online);
        // An admin prompt blocks the screen the same way, but needs no setup step.
        let p = probe_from(Some(ScreenBlock::AdminPrompt), None);
        assert_eq!(p.capabilities, vec![C::AppsList, C::Takeover]);
        assert_eq!(p.setup.state, SetupState::Complete);
    }

    #[test]
    fn parses_ver() {
        assert_eq!(
            parse_ver("\r\nMicrosoft Windows [Version 10.0.22631.4602]\r\n"),
            Some("10.0.22631.4602".into())
        );
        assert_eq!(parse_ver("garbage"), None);
    }
}
