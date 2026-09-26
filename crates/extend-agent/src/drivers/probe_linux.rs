//! What a Linux computer can do right now: a screen (X11 or Wayland), the AT-SPI accessibility
//! bus, and the helper programs agent-device's Linux support drives (xdotool or ydotool, a
//! screenshot tool, a clipboard tool, xdg-open).
//!
//! A computer without a screen (a server) gets only `terminal`, `apps.launch` and `replay`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use extend_driver::Probe;
use extend_protocol::model::{MissingCapability, Setup, SetupStep, StepStatus};
use extend_protocol::{Capability, DeviceOs};

use crate::drivers::agent_device::ProbeInput;

pub const NO_SCREEN_REASON: &str =
    "This computer has no screen (no DISPLAY or WAYLAND_DISPLAY), so a Silicon can only use the terminal here.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    X11,
    Wayland,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFacts {
    pub display: Option<DisplayServer>,
    /// `Ok` when the AT-SPI bus answered; the error says what's wrong otherwise.
    pub atspi: Result<(), String>,
    pub input_tool: Option<String>,
    pub screenshot_tool: Option<String>,
    pub clipboard_tool: Option<String>,
    pub recording_tools: bool,
    pub xdg_open: bool,
}

fn miss(c: Capability, reason: &str, missing: &mut Vec<MissingCapability>) {
    if !missing.iter().any(|m| m.capability == c) {
        missing.push(MissingCapability { capability: c, reason: reason.to_owned() });
    }
}

pub fn detect_display(display: Option<&str>, wayland: Option<&str>, session_type: Option<&str>) -> Option<DisplayServer> {
    let set = |v: Option<&str>| v.is_some_and(|s| !s.trim().is_empty());
    if set(wayland) || (session_type == Some("wayland") && set(display)) {
        Some(DisplayServer::Wayland)
    } else if set(display) {
        Some(DisplayServer::X11)
    } else {
        None
    }
}

/// Builds the probe from what was found. Pure, so every combination is testable.
pub fn build_probe(facts: &LinuxFacts, input: &ProbeInput<'_>) -> Probe {
    use Capability::*;
    let mut caps = Vec::new();
    let mut missing = Vec::new();
    let mut steps = Vec::new();

    let Some(display) = facts.display else {
        // A server: terminal (added by the local driver), opening apps and replay only.
        if let Some(problem) = input.problem {
            for c in [AppsLaunch, Replay] {
                miss(c, problem, &mut missing);
            }
        } else {
            caps.extend([AppsLaunch, Replay]);
        }
        for c in DeviceOs::Linux.full_capabilities() {
            if !matches!(c, Terminal | AppsLaunch | Replay) {
                miss(*c, NO_SCREEN_REASON, &mut missing);
            }
        }
        return finish(caps, missing, steps);
    };

    caps.push(Takeover);
    if let Some(problem) = input.problem {
        for c in DeviceOs::Linux.full_capabilities() {
            if !matches!(c, Terminal | Takeover) {
                miss(*c, problem, &mut missing);
            }
        }
        return finish(caps, missing, steps);
    }

    caps.extend([AppsLaunch, Replay]);
    if input.supports("apps") {
        caps.push(AppsList);
    } else {
        miss(AppsList, "Listing apps isn't available on Linux in this version of Silicon Extend.", &mut missing);
    }
    if facts.xdg_open {
        caps.push(Links);
    } else {
        miss(Links, "Install xdg-utils so links open in the default browser (for example `sudo apt install xdg-utils`).", &mut missing);
    }

    match &facts.atspi {
        Ok(()) => {
            caps.push(ScreenRead);
            steps.push(done_step("accessibility", "Turn on the accessibility bus (AT-SPI)"));
        }
        Err(why) => {
            let reason = format!(
                "The accessibility bus (AT-SPI) isn't answering: {why}. Install at-spi2-core, python3-gi and gir1.2-atspi-2.0, and turn on assistive technologies (GNOME: Settings › Accessibility)."
            );
            miss(ScreenRead, &reason, &mut missing);
            steps.push(SetupStep {
                key: "accessibility".into(),
                title: "Turn on the accessibility bus (AT-SPI)".into(),
                status: StepStatus::NeedsCarbon,
                help: Some("Install at-spi2-core, python3-gi and gir1.2-atspi-2.0; GNOME: Settings › Accessibility".into()),
                error: Some(why.clone()),
                input: None,
            });
        }
    }

    let input_help = match display {
        DisplayServer::X11 => "Install xdotool (for example `sudo apt install xdotool`).",
        DisplayServer::Wayland => "Install ydotool and start ydotoold, then approve remote control when your desktop asks.",
    };
    if facts.input_tool.is_some() {
        caps.extend([InputPointer, InputText]);
        steps.push(done_step("input", "Allow remote control of the mouse and keyboard"));
    } else {
        for c in [InputPointer, InputText] {
            miss(c, input_help, &mut missing);
        }
        steps.push(needs_step("input", "Allow remote control of the mouse and keyboard", input_help));
    }

    let shot_help = match display {
        DisplayServer::X11 => "Install a screenshot tool: gnome-screenshot, scrot or ImageMagick (import).",
        DisplayServer::Wayland => "Install grim (or gnome-screenshot) and approve screen sharing when your desktop asks.",
    };
    if facts.screenshot_tool.is_some() {
        caps.push(ScreenCapture);
        steps.push(done_step("screen_capture", "Allow screen capture"));
    } else {
        miss(ScreenCapture, shot_help, &mut missing);
        steps.push(needs_step("screen_capture", "Allow screen capture", shot_help));
    }

    if display == DisplayServer::X11 && facts.recording_tools && input.commands.is_some_and(|commands| commands.iter().any(|command| command == "record")) {
        caps.push(ScreenRecord);
    } else {
        let reason = if display == DisplayServer::Wayland {
            "Wayland recording requires the ScreenCast portal; this version does not implement portal recording yet."
        } else if !facts.recording_tools {
            "Install python3, ffmpeg (including ffprobe) and x11-utils (xwininfo) for X11 recording."
        } else {
            "The installed recording runtime did not report support. Update Silicon Extend and retry."
        };
        miss(ScreenRecord, reason, &mut missing);
    }
    if !input.supports("logs") {
        miss(Logs, "Device logs aren't available on Linux in this version of Silicon Extend.", &mut missing);
    } else {
        caps.push(Logs);
    }
    match (&facts.clipboard_tool, input.supports("clipboard")) {
        (Some(_), true) => caps.push(Clipboard),
        (None, true) => miss(
            Clipboard,
            match display {
                DisplayServer::X11 => "Install xclip or xsel for the clipboard.",
                DisplayServer::Wayland => "Install wl-clipboard for the clipboard.",
            },
            &mut missing,
        ),
        (_, false) => miss(Clipboard, "The clipboard isn't available on Linux in this version of Silicon Extend.", &mut missing),
    }
    finish(caps, missing, steps)
}

fn done_step(key: &str, title: &str) -> SetupStep {
    SetupStep { key: key.into(), title: title.into(), status: StepStatus::Done, help: None, error: None, input: None }
}

fn needs_step(key: &str, title: &str, help: &str) -> SetupStep {
    SetupStep { key: key.into(), title: title.into(), status: StepStatus::NeedsCarbon, help: Some(help.into()), error: None, input: None }
}

fn finish(mut caps: Vec<Capability>, missing: Vec<MissingCapability>, steps: Vec<SetupStep>) -> Probe {
    caps.sort();
    caps.dedup();
    let missing = missing.into_iter().filter(|m| !caps.contains(&m.capability)).collect();
    Probe {
        os: DeviceOs::Linux,
        os_version: crate::sysinfo::os_version(),
        model: crate::sysinfo::model(),
        capabilities: caps,
        missing,
        // Linux has nothing a Carbon must do before a Silicon can use the computer (the terminal
        // always works); what each missing capability needs is in `missing`. The steps are kept
        // only when every one is done, as a record of what was checked.
        setup: if steps.iter().all(|s| s.status == StepStatus::Done) { Setup::from_steps(steps) } else { Setup::complete() },
        agent_device_version: None,
        online: true,
    }
}

/// Checks the screen and helper programs.
pub fn gather() -> LinuxFacts {
    let env = |k: &str| std::env::var(k).ok();
    let display = detect_display(env("DISPLAY").as_deref(), env("WAYLAND_DISPLAY").as_deref(), env("XDG_SESSION_TYPE").as_deref());
    let have = |p: &str| crate::config::which(p).is_some();
    let first = |list: &[&str]| list.iter().find(|p| have(p)).map(|p| (*p).to_owned());
    let (input_tool, screenshot_tool, clipboard_tool) = match display {
        None => (None, None, None),
        Some(DisplayServer::X11) => (
            first(&["xdotool"]),
            first(&["gnome-screenshot", "scrot", "import"]),
            first(&["xclip", "xsel"]),
        ),
        Some(DisplayServer::Wayland) => (first(&["ydotool"]), first(&["grim", "gnome-screenshot"]), first(&["wl-paste"])),
    };
    let atspi = if display.is_some() { check_atspi() } else { Err("no screen".into()) };
    LinuxFacts { display, atspi, input_tool, screenshot_tool, clipboard_tool, recording_tools: ["python3", "ffmpeg", "ffprobe", "xwininfo"].iter().all(|tool| have(tool)), xdg_open: have("xdg-open") }
}

/// Asks the AT-SPI registry for the desktop, the same way agent-device's dumper starts.
fn check_atspi() -> Result<(), String> {
    const SCRIPT: &str = "import gi\ngi.require_version('Atspi', '2.0')\nfrom gi.repository import Atspi\nprint(Atspi.get_desktop(0).get_child_count())";
    let Some(python) = crate::config::which("python3") else {
        return Err("python3 isn't installed".into());
    };
    let mut child = Command::new(python)
        .args(["-c", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run python3: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                if status.success() {
                    return Ok(());
                }
                let err = String::from_utf8_lossy(&out.stderr);
                let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error").trim().to_owned();
                return Err(last);
            }
            Ok(None) if started.elapsed() > Duration::from_secs(10) => {
                let _ = child.kill();
                return Err("the accessibility bus didn't answer within 10 seconds".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extend_protocol::model::SetupState;

    fn desktop() -> LinuxFacts {
        LinuxFacts {
            display: Some(DisplayServer::X11),
            atspi: Ok(()),
            input_tool: Some("xdotool".into()),
            screenshot_tool: Some("import".into()),
            clipboard_tool: Some("xclip".into()),
            recording_tools: true,
            xdg_open: true,
        }
    }
    fn input() -> ProbeInput<'static> {
        ProbeInput { problem: None, commands: None }
    }

    #[test]
    fn display_detection() {
        assert_eq!(detect_display(Some(":0"), None, None), Some(DisplayServer::X11));
        assert_eq!(detect_display(Some(":0"), Some("wayland-0"), None), Some(DisplayServer::Wayland));
        assert_eq!(detect_display(None, None, Some("tty")), None);
        assert_eq!(detect_display(Some(""), None, None), None);
    }

    #[test]
    fn a_server_gets_terminal_apps_and_replay_only() {
        let facts = LinuxFacts { display: None, atspi: Err("no screen".into()), input_tool: None, screenshot_tool: None, clipboard_tool: None, recording_tools: false, xdg_open: false };
        let p = build_probe(&facts, &input());
        assert_eq!(p.capabilities, vec![Capability::AppsLaunch, Capability::Replay]);
        assert!(p.missing.iter().any(|m| m.capability == Capability::ScreenRead && m.reason == NO_SCREEN_REASON));
        assert!(!p.missing.iter().any(|m| m.capability == Capability::Terminal));
        assert_eq!(p.setup.state, SetupState::Complete);
    }

    #[test]
    fn a_full_desktop() {
        let cmds: Vec<String> = ["snapshot", "click", "clipboard", "screenshot"].map(String::from).to_vec();
        let p = build_probe(&desktop(), &ProbeInput { problem: None, commands: Some(&cmds) });
        // agent-device doesn't list apps on Linux.
        assert!(p.missing.iter().any(|m| m.capability == Capability::AppsList));
        for c in [Capability::ScreenRead, Capability::ScreenCapture, Capability::InputPointer, Capability::InputText, Capability::Clipboard, Capability::Links, Capability::Takeover] {
            assert!(p.capabilities.contains(&c), "{c:?}");
        }
        // This runtime inventory reports neither recording nor logs.
        assert!(p.missing.iter().any(|m| m.capability == Capability::ScreenRecord));
        assert!(p.missing.iter().any(|m| m.capability == Capability::Logs));
        assert_eq!(p.setup.state, SetupState::Complete);
    }

    #[test]
    fn recording_support_is_independent_of_screenshot_tools_and_refuses_wayland() {
        let commands = vec!["record".to_owned()];
        let admitted = ProbeInput { problem: None, commands: Some(&commands) };
        let facts = LinuxFacts { screenshot_tool: None, ..desktop() };
        assert!(build_probe(&facts, &admitted).capabilities.contains(&Capability::ScreenRecord));
        let missing = LinuxFacts { recording_tools: false, ..desktop() };
        assert!(!build_probe(&missing, &admitted).capabilities.contains(&Capability::ScreenRecord));
        assert!(!build_probe(&desktop(), &input()).capabilities.contains(&Capability::ScreenRecord));
        let wayland = LinuxFacts { display: Some(DisplayServer::Wayland), ..desktop() };
        assert!(!build_probe(&wayland, &admitted).capabilities.contains(&Capability::ScreenRecord));
    }

    #[test]
    fn missing_tools_are_named() {
        let facts = LinuxFacts { input_tool: None, screenshot_tool: None, atspi: Err("No module named 'gi'".into()), ..desktop() };
        let p = build_probe(&facts, &input());
        let reason = |c| p.missing.iter().find(|m| m.capability == c).unwrap().reason.clone();
        assert!(reason(Capability::InputPointer).contains("xdotool"));
        assert!(reason(Capability::ScreenCapture).contains("scrot"));
        assert!(reason(Capability::ScreenRead).contains("No module named 'gi'"));
        // Missing tools never block sessions: the terminal still works.
        assert_eq!(p.setup.state, SetupState::Complete);
        let facts = LinuxFacts { display: Some(DisplayServer::Wayland), input_tool: None, ..desktop() };
        let p = build_probe(&facts, &input());
        assert!(p.missing.iter().find(|m| m.capability == Capability::InputText).unwrap().reason.contains("ydotool"));
    }

    #[test]
    fn no_helper() {
        let p = build_probe(&desktop(), &ProbeInput { problem: Some("gone"), commands: None });
        assert_eq!(p.capabilities, vec![Capability::Takeover]);
        assert_eq!(p.setup.state, SetupState::Complete);
    }
}
