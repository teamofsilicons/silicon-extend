//! What a Mac can do right now: Accessibility, Screen Recording, whether the device engine's UI
//! testing runner can drive apps without stopping for a password, and whether the Mac is locked
//! or asleep (then nothing that needs the screen works, and each such capability says why).
//!
//! Permissions belong to the app that launched the agent (Silicon Extend.app when installed; the
//! terminal during development), so the agent checks them in its own process with
//! `AXIsProcessTrusted` and `CGPreflightScreenCaptureAccess`, which never prompt.

use extend_driver::Probe;
use extend_protocol::model::{MissingCapability, Setup, SetupStep, StepStatus};
use extend_protocol::{Capability, DeviceOs};

use crate::drivers::agent_device::ProbeInput;
use crate::drivers::screen_lock::{self, ScreenBlock};

pub const ACCESSIBILITY_HELP: &str = "System Settings › Privacy & Security › Accessibility";
pub const SCREEN_RECORDING_HELP: &str = "System Settings › Privacy & Security › Screen & System Audio Recording";
pub const ACCESSIBILITY_REASON: &str =
    "Allow Accessibility for Silicon Extend in System Settings › Privacy & Security › Accessibility.";
pub const SCREEN_RECORDING_REASON: &str = "Allow Screen Recording for Silicon Extend in System Settings › Privacy & Security › Screen & System Audio Recording.";

/// macOS's UI Automation mode, which the device engine's UI testing runner turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationMode {
    /// On right now.
    Enabled,
    /// Off, but turning it on won't ask for a password.
    NoAuthentication,
    /// Off, and turning it on asks the Carbon for Touch ID or a password.
    NeedsAuthentication,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacFacts {
    pub accessibility: bool,
    pub screen_recording: bool,
    pub automation: AutomationMode,
    /// Full Xcode (not just the command line tools) is installed.
    pub xcode: bool,
    /// Why the screen can't be used right now (locked, asleep, another account on screen).
    pub screen: Option<ScreenBlock>,
}

/// Reads `automationmodetool`'s status output.
pub fn parse_automation_mode(out: &str) -> AutomationMode {
    let lower = out.to_ascii_lowercase();
    if lower.contains("automation mode is enabled") {
        AutomationMode::Enabled
    } else if lower.contains("does not require user authentication") {
        AutomationMode::NoAuthentication
    } else if lower.contains("requires user authentication") {
        AutomationMode::NeedsAuthentication
    } else {
        AutomationMode::Unknown
    }
}

/// Builds the probe from what was found. Pure, so every combination is testable.
pub fn build_probe(facts: &MacFacts, input: &ProbeInput<'_>) -> Probe {
    use Capability::*;
    let mut caps = vec![Takeover];
    let mut missing = Vec::new();
    let mut steps = Vec::new();

    if let Some(problem) = input.problem {
        for c in [
            ScreenRead,
            ScreenCapture,
            ScreenRecord,
            InputPointer,
            InputText,
            AppsLaunch,
            AppsList,
            Alerts,
            Clipboard,
            Logs,
            Replay,
            Links,
        ] {
            miss(c, problem, &mut missing);
        }
        // Not a setup step: the terminal still works, and the reason is in `missing`.
        return finish(caps, missing, steps);
    }

    // Always there with the device engine: apps, links, replay, clipboard, logs.
    caps.extend([AppsLaunch, Links, Replay]);
    if input.supports("apps") {
        caps.push(AppsList);
    } else {
        miss(
            AppsList,
            "The device engine doesn't support `apps` on this Mac.",
            &mut missing,
        );
    }
    for (cap, cmd) in [(Clipboard, "clipboard"), (Logs, "logs")] {
        if input.supports(cmd) {
            caps.push(cap);
        } else {
            miss(
                cap,
                &format!("The device engine doesn't support `{cmd}` on this Mac."),
                &mut missing,
            );
        }
    }

    steps.push(step(
        "accessibility",
        "Allow Accessibility for Silicon Extend",
        facts.accessibility,
        ACCESSIBILITY_HELP,
    ));
    if facts.accessibility {
        caps.extend([ScreenRead, InputPointer, InputText, Alerts]);
    } else {
        for c in [ScreenRead, InputPointer, InputText, Alerts] {
            miss(c, ACCESSIBILITY_REASON, &mut missing);
        }
    }

    steps.push(step(
        "screen_recording",
        "Allow Screen Recording for Silicon Extend",
        facts.screen_recording,
        SCREEN_RECORDING_HELP,
    ));
    if facts.screen_recording {
        caps.push(ScreenCapture);
        match input.supports("record") {
            false => miss(
                ScreenRecord,
                "The device engine doesn't support `record` on this Mac.",
                &mut missing,
            ),
            true => caps.push(ScreenRecord),
        }
    } else {
        for c in [ScreenCapture, ScreenRecord] {
            miss(c, SCREEN_RECORDING_REASON, &mut missing);
        }
    }

    // Not a setup step: nothing is left to set up, and the terminal works while it is locked.
    if let Some(block) = facts.screen {
        screen_lock::withhold(block, &mut caps, &mut missing);
    }
    finish(caps, missing, steps)
}

/// Records a missing capability once, with the first reason given.
pub fn miss(c: Capability, reason: &str, missing: &mut Vec<MissingCapability>) {
    if !missing.iter().any(|m| m.capability == c) {
        missing.push(MissingCapability {
            capability: c,
            reason: reason.to_owned(),
        });
    }
}

fn step(key: &str, title: &str, done: bool, help: &str) -> SetupStep {
    SetupStep {
        key: key.into(),
        title: title.into(),
        status: if done {
            StepStatus::Done
        } else {
            StepStatus::NeedsCarbon
        },
        help: (!done).then(|| help.to_owned()),
        error: None,
        input: None,
    }
}

fn finish(mut caps: Vec<Capability>, missing: Vec<MissingCapability>, steps: Vec<SetupStep>) -> Probe {
    caps.sort();
    caps.dedup();
    let missing = missing.into_iter().filter(|m| !caps.contains(&m.capability)).collect();
    Probe {
        os: DeviceOs::Macos,
        os_version: crate::sysinfo::os_version(),
        model: crate::sysinfo::model(),
        capabilities: caps,
        missing,
        setup: Setup::from_steps(steps),
        engine_version: None,
        online: true,
        awake: None,
        sleep_state: None,
        hardware_id: None,
    }
}

#[cfg(target_os = "macos")]
mod ffi {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }
}

/// Checks everything, without prompting.
#[cfg(target_os = "macos")]
pub fn gather() -> MacFacts {
    // SAFETY: both are plain queries with no arguments.
    let accessibility = unsafe { ffi::AXIsProcessTrusted() };
    let screen_recording = unsafe { ffi::CGPreflightScreenCaptureAccess() };
    let automation = std::process::Command::new("/usr/bin/automationmodetool")
        .output()
        .map(|o| {
            parse_automation_mode(&format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ))
        })
        .unwrap_or(AutomationMode::Unknown);
    MacFacts {
        accessibility,
        screen_recording,
        automation,
        xcode: full_xcode_installed(),
        screen: screen_lock::current(),
    }
}

#[cfg(target_os = "macos")]
fn full_xcode_installed() -> bool {
    let Ok(out) = std::process::Command::new("/usr/bin/xcode-select").arg("-p").output() else {
        return false;
    };
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    !dir.is_empty()
        && !dir.contains("CommandLineTools")
        && std::path::Path::new(&dir).join("usr/bin/xcodebuild").exists()
}

/// Asks macOS to add Silicon Extend to the Screen Recording list (shows the system prompt once).
#[cfg(target_os = "macos")]
pub fn request_screen_recording() -> bool {
    // SAFETY: plain call; macOS shows its own prompt.
    unsafe { ffi::CGRequestScreenCaptureAccess() }
}

/// Opens the System Settings page for a setup step.
#[cfg(target_os = "macos")]
pub fn open_settings(step: &str) {
    let url = match step {
        "accessibility" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        "screen_recording" => "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        "xcode" => "macappstore://apps.apple.com/app/xcode/id497799835",
        _ => "x-apple.systempreferences:com.apple.preference.security",
    };
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use extend_protocol::model::SetupState;

    fn all_good() -> MacFacts {
        MacFacts {
            accessibility: true,
            screen_recording: true,
            automation: AutomationMode::NoAuthentication,
            xcode: true,
            screen: None,
        }
    }

    #[test]
    fn a_locked_or_sleeping_mac_says_so_for_everything_that_needs_the_screen() {
        for block in [ScreenBlock::Locked, ScreenBlock::Asleep, ScreenBlock::OtherSession] {
            let p = build_probe(
                &MacFacts {
                    screen: Some(block),
                    ..all_good()
                },
                &input(),
            );
            // The terminal (added by the local driver) and what doesn't touch the screen stay.
            assert_eq!(
                p.capabilities,
                vec![Capability::AppsList, Capability::Logs, Capability::Takeover]
            );
            for c in [
                Capability::ScreenRead,
                Capability::ScreenCapture,
                Capability::ScreenRecord,
                Capability::InputPointer,
                Capability::InputText,
                Capability::AppsLaunch,
                Capability::Clipboard,
                Capability::Replay,
                Capability::Links,
                Capability::Alerts,
            ] {
                let m = p.missing.iter().find(|m| m.capability == c).expect("missing");
                assert_eq!(m.reason, block.reason(), "{c:?}");
            }
            // Sessions still start (the terminal works), so it isn't a setup step.
            assert_eq!(p.setup.state, SetupState::Complete);
        }
        if cfg!(target_os = "macos") {
            assert!(ScreenBlock::Locked.reason().starts_with("This Mac is locked"));
        }
        // A permission that is off keeps its own reason: unlocking won't fix it.
        let p = build_probe(
            &MacFacts {
                accessibility: false,
                screen: Some(ScreenBlock::Locked),
                ..all_good()
            },
            &input(),
        );
        let m = p
            .missing
            .iter()
            .find(|m| m.capability == Capability::ScreenRead)
            .unwrap();
        assert_eq!(m.reason, ACCESSIBILITY_REASON);
        let m = p
            .missing
            .iter()
            .find(|m| m.capability == Capability::ScreenCapture)
            .unwrap();
        assert_eq!(m.reason, ScreenBlock::Locked.reason());
    }

    fn input() -> ProbeInput<'static> {
        ProbeInput {
            problem: None,
            commands: None,
        }
    }

    #[test]
    fn fully_set_up_mac_has_everything_but_terminal() {
        let p = build_probe(&all_good(), &input());
        let mut want: Vec<Capability> = DeviceOs::Macos
            .full_capabilities()
            .iter()
            .copied()
            .filter(|c| *c != Capability::Terminal)
            .collect();
        want.sort();
        assert_eq!(p.capabilities, want);
        assert!(p.missing.is_empty(), "{:?}", p.missing);
        assert_eq!(p.setup.state, SetupState::Complete);
    }

    #[test]
    fn missing_accessibility_is_reported_precisely() {
        let p = build_probe(
            &MacFacts {
                accessibility: false,
                ..all_good()
            },
            &input(),
        );
        assert!(!p.capabilities.contains(&Capability::ScreenRead));
        assert!(!p.capabilities.contains(&Capability::InputText));
        let m = p
            .missing
            .iter()
            .find(|m| m.capability == Capability::ScreenRead)
            .unwrap();
        assert_eq!(m.reason, ACCESSIBILITY_REASON);
        assert!(
            m.reason
                .contains("System Settings › Privacy & Security › Accessibility")
        );
        assert_eq!(p.setup.state, SetupState::NeedsCarbon);
        let s = p.setup.steps.iter().find(|s| s.key == "accessibility").unwrap();
        assert_eq!(s.status, StepStatus::NeedsCarbon);
        assert_eq!(s.help.as_deref(), Some(ACCESSIBILITY_HELP));
    }

    #[test]
    fn missing_screen_recording_drops_capture() {
        let p = build_probe(
            &MacFacts {
                screen_recording: false,
                ..all_good()
            },
            &input(),
        );
        assert!(!p.capabilities.contains(&Capability::ScreenCapture));
        assert!(p.capabilities.contains(&Capability::ScreenRead));
        assert!(
            p.missing
                .iter()
                .any(|m| m.capability == Capability::ScreenRecord && m.reason == SCREEN_RECORDING_REASON)
        );
    }

    #[test]
    fn native_input_and_recording_do_not_need_xctest() {
        let p = build_probe(
            &MacFacts {
                automation: AutomationMode::NeedsAuthentication,
                ..all_good()
            },
            &input(),
        );
        assert!(p.capabilities.contains(&Capability::InputText));
        // Only Accessibility and Screen Recording are setup steps; this doesn't block sessions.
        assert_eq!(p.setup.state, SetupState::Complete);
        assert_eq!(
            p.setup.steps.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            ["accessibility", "screen_recording"]
        );
        assert!(p.capabilities.contains(&Capability::InputPointer));
        assert!(p.capabilities.contains(&Capability::ScreenRecord));
        assert!(p.capabilities.contains(&Capability::ScreenCapture));
        let p = build_probe(
            &MacFacts {
                xcode: false,
                ..all_good()
            },
            &input(),
        );
        assert!(p.capabilities.contains(&Capability::InputText));
        assert!(p.capabilities.contains(&Capability::ScreenRecord));
    }

    #[test]
    fn no_helper_means_only_takeover() {
        let p = build_probe(
            &all_good(),
            &ProbeInput {
                problem: Some("helper gone"),
                commands: None,
            },
        );
        assert_eq!(p.capabilities, vec![Capability::Takeover]);
        assert!(p.missing.iter().all(|m| m.reason == "helper gone"));
        // The terminal still works, so a missing helper doesn't block sessions.
        assert_eq!(p.setup.state, SetupState::Complete);
    }

    #[test]
    fn unsupported_commands_become_missing() {
        let cmds: Vec<String> = ["snapshot", "click", "clipboard"].map(String::from).to_vec();
        let p = build_probe(
            &all_good(),
            &ProbeInput {
                problem: None,
                commands: Some(&cmds),
            },
        );
        assert!(p.capabilities.contains(&Capability::Clipboard));
        assert!(!p.capabilities.contains(&Capability::Logs));
        assert!(!p.capabilities.contains(&Capability::ScreenRecord));
    }

    #[test]
    fn automation_mode_output() {
        assert_eq!(
            parse_automation_mode(
                "Automation Mode is disabled.\nThis device requires user authentication to enable Automation Mode.\n"
            ),
            AutomationMode::NeedsAuthentication
        );
        assert_eq!(
            parse_automation_mode(
                "Automation Mode is disabled.\nThis device does not require user authentication to enable Automation Mode.\n"
            ),
            AutomationMode::NoAuthentication
        );
        assert_eq!(
            parse_automation_mode("Automation Mode is enabled."),
            AutomationMode::Enabled
        );
        assert_eq!(parse_automation_mode(""), AutomationMode::Unknown);
    }
}
