//! Start at login: a LaunchAgent on macOS, a Run key on Windows, an XDG autostart entry (or a
//! systemd user unit, for servers) on Linux.
//!
//! `UNDERSTANDING.md`: "The app starts on its own when the device starts." So once a computer is
//! paired, the app with a window turns start at login on by itself ([`after_pairing`]), unless
//! the Carbon has turned it off: with the window's switch, the menu's "Start at login", `run
//! --no-autostart` or `uninstall-autostart`. That choice is kept in `{state}/start-at-login.json`
//! and never overridden. A headless run (servers, CI) leaves it alone unless asked with `run
//! --autostart` or `install-autostart`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub const LABEL: &str = "com.teamofsilicons.extend-agent";

/// How the agent should start.
#[derive(Debug, Clone, Default)]
pub struct AutostartOptions {
    /// Start without the tray icon (servers, CI).
    pub headless: bool,
    /// Linux: install a systemd user unit instead of an XDG autostart entry.
    pub systemd: bool,
}

fn exe() -> Result<PathBuf> {
    std::env::current_exe().context("couldn't find this program's path")
}

/// Who made the current start-at-login choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChosenBy {
    /// The app turned it on after pairing, because nobody had chosen.
    Default,
    /// The Carbon (the window, the menu, a flag or a command).
    Carbon,
}

/// `{state}/start-at-login.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub start_at_login: bool,
    pub by: ChosenBy,
}

pub fn choice_path(state_dir: &Path) -> PathBuf {
    state_dir.join("start-at-login.json")
}

/// The choice on record, if any (an unreadable file counts as none).
pub fn load_choice(state_dir: &Path) -> Option<Choice> {
    let bytes = std::fs::read(choice_path(state_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_choice(state_dir: &Path, choice: Choice) -> Result<()> {
    crate::config::write_private_file(&choice_path(state_dir), &serde_json::to_vec(&choice)?)
}

/// `run --autostart` / `run --no-autostart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunFlag {
    #[default]
    Unset,
    On,
    Off,
}

/// What to do about the start-at-login entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Write the entry (again), pointing at this program.
    Install,
    /// Remove the entry.
    Remove,
    /// Change nothing, and why.
    Leave(String),
}

/// Why this program can't be what a login starts, when it can't: macOS runs an app opened
/// straight from Downloads (or from a disk image) from a random read-only copy, gone after a
/// restart.
pub fn unfit_program(program: &Path) -> Option<String> {
    let p = program.to_string_lossy();
    if p.contains("/AppTranslocation/") || p.starts_with("/Volumes/") {
        return Some(
            "Silicon Extend is running from a temporary copy (it was opened from Downloads or a disk image), so it can't start at login from here. Move Silicon Extend to Applications, open it from there, and turn start at login on again."
                .into(),
        );
    }
    None
}

/// A binary Cargo built in a checkout (`…/target/debug/extend-agent`), not an installed app.
pub fn is_development_build(program: &Path) -> bool {
    let parts: Vec<&str> = program.components().filter_map(|c| c.as_os_str().to_str()).collect();
    parts
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
        || parts
            .windows(3)
            .any(|w| w[0] == "target" && (w[2] == "debug" || w[2] == "release"))
}

/// What a `run` flag asks for, before anything else happens.
pub fn at_start(flag: RunFlag, installed: bool) -> Decision {
    match (flag, installed) {
        (RunFlag::Unset, _) => Decision::Leave("no flag given".into()),
        (RunFlag::On, _) => Decision::Install,
        (RunFlag::Off, true) => Decision::Remove,
        (RunFlag::Off, false) => Decision::Leave("start at login is already off".into()),
    }
}

/// What the app with a window does once the computer is paired. `installed` is the program the
/// current entry starts, if there is one; `program` is this one.
pub fn after_pairing(choice: Option<Choice>, installed: Option<&str>, program: &Path) -> Decision {
    let wanted = match choice {
        None => true,
        Some(c) => c.start_at_login,
    };
    if !wanted {
        return Decision::Leave("the Carbon turned start at login off".into());
    }
    let here = program.to_string_lossy();
    if installed == Some(here.as_ref()) {
        return Decision::Leave("start at login is on".into());
    }
    if let Some(why) = unfit_program(program) {
        return Decision::Leave(why);
    }
    if is_development_build(program) {
        return Decision::Leave(
            "this is a development build (in a Cargo target directory), which is never registered by itself; `extend-agent install-autostart` still registers it".into(),
        );
    }
    if installed.is_some() || choice.is_none() {
        // Nobody chose yet: on by default. Or the entry starts a copy that isn't this one (the
        // app was moved or reinstalled elsewhere): point it here.
        return Decision::Install;
    }
    // Chosen on, but the entry is gone: removed outside the app, so that stands.
    Decision::Leave("start at login was turned on, but its entry has since been removed outside Silicon Extend".into())
}

fn run_args(opts: &AutostartOptions) -> Vec<String> {
    let mut a = vec!["run".to_owned()];
    if opts.headless {
        a.push("--headless".into());
    }
    a
}

/// The macOS LaunchAgent plist.
pub fn launch_agent_plist(program: &Path, args: &[String]) -> String {
    let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut items = format!("    <string>{}</string>\n", escape(&program.display().to_string()));
    for a in args {
        items.push_str(&format!("    <string>{}</string>\n", escape(a)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{items}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
</dict>
</plist>
"#
    )
}

/// The XDG autostart entry (`~/.config/autostart/silicon-extend.desktop`).
pub fn xdg_desktop_entry(program: &Path, args: &[String]) -> String {
    let quote = |s: &str| {
        if s.contains(' ') {
            format!("\"{}\"", s.replace('"', "\\\""))
        } else {
            s.to_owned()
        }
    };
    let mut exec = quote(&program.display().to_string());
    for a in args {
        exec.push(' ');
        exec.push_str(&quote(a));
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Silicon Extend\nComment=Lets the Silicons you choose use this computer\nExec={exec}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nNoDisplay=false\n"
    )
}

/// A systemd user unit (`~/.config/systemd/user/silicon-extend.service`).
pub fn systemd_unit(program: &Path, args: &[String]) -> String {
    let exec = std::iter::once(program.display().to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=Silicon Extend device agent\nAfter=network-online.target\n\n[Service]\nExecStart={exec}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn home() -> Result<PathBuf> {
    std::env::home_dir().context("couldn't find the home directory")
}

fn xdg_config() -> Result<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => Ok(home()?.join(".config")),
    }
}

/// Where the start-at-login entry lives on this OS.
pub fn entry_path(opts: &AutostartOptions) -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
    } else if cfg!(windows) {
        Ok(PathBuf::from(
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Silicon Extend",
        ))
    } else if opts.systemd {
        Ok(xdg_config()?.join("systemd/user/silicon-extend.service"))
    } else {
        Ok(xdg_config()?.join("autostart/silicon-extend.desktop"))
    }
}

/// Installs the start-at-login entry. Returns where it went.
pub fn install(opts: &AutostartOptions) -> Result<String> {
    let program = exe()?;
    let args = run_args(opts);
    let path = entry_path(opts)?;
    if cfg!(windows) {
        let value = std::iter::once(format!("\"{}\"", program.display()))
            .chain(args)
            .collect::<Vec<_>>()
            .join(" ");
        let out = std::process::Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Silicon Extend",
                "/t",
                "REG_SZ",
                "/d",
                &value,
                "/f",
            ])
            .output()
            .context("couldn't run reg.exe")?;
        anyhow::ensure!(
            out.status.success(),
            "reg.exe failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return Ok(path.display().to_string());
    }
    let content = if cfg!(target_os = "macos") {
        launch_agent_plist(&program, &args)
    } else if opts.systemd {
        systemd_unit(&program, &args)
    } else {
        xdg_desktop_entry(&program, &args)
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    }
    std::fs::write(&path, content).with_context(|| format!("couldn't write {}", path.display()))?;
    if !cfg!(target_os = "macos") && opts.systemd {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", "silicon-extend.service"])
            .status();
    }
    Ok(path.display().to_string())
}

/// Removes the start-at-login entries this program may have installed.
pub fn uninstall() -> Result<Vec<String>> {
    let mut removed = Vec::new();
    if cfg!(windows) {
        let out = std::process::Command::new("reg")
            .args([
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Silicon Extend",
                "/f",
            ])
            .output()
            .context("couldn't run reg.exe")?;
        if out.status.success() {
            removed.push(entry_path(&AutostartOptions::default())?.display().to_string());
        }
        return Ok(removed);
    }
    for opts in [
        AutostartOptions::default(),
        AutostartOptions {
            systemd: true,
            ..Default::default()
        },
    ] {
        let path = entry_path(&opts)?;
        if path.exists() {
            if opts.systemd {
                let _ = std::process::Command::new("systemctl")
                    .args(["--user", "disable", "silicon-extend.service"])
                    .status();
            }
            std::fs::remove_file(&path).with_context(|| format!("couldn't remove {}", path.display()))?;
            removed.push(path.display().to_string());
        }
    }
    Ok(removed)
}

/// The program the macOS LaunchAgent starts (its first `ProgramArguments` string).
pub fn plist_program(plist: &str) -> Option<String> {
    let args = plist.split_once("<key>ProgramArguments</key>")?.1;
    let first = args.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(first.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"))
}

/// The program an XDG entry (`Exec=`) or a systemd unit (`ExecStart=`) starts.
pub fn exec_program(entry: &str) -> Option<String> {
    let line = entry
        .lines()
        .find_map(|l| l.strip_prefix("Exec=").or_else(|| l.strip_prefix("ExecStart=")))?
        .trim();
    Some(match line.strip_prefix('"') {
        Some(rest) => rest.split_once('"').map_or(rest, |(p, _)| p).to_owned(),
        None => line.split_whitespace().next()?.to_owned(),
    })
}

/// The program in `reg query …\Run /v "Silicon Extend"` output (`"C:\…\extend-agent.exe" run`).
pub fn reg_program(output: &str) -> Option<String> {
    let line = output.lines().find(|l| l.contains("REG_SZ"))?;
    let value = line.split_once("REG_SZ")?.1.trim();
    Some(match value.strip_prefix('"') {
        Some(rest) => rest.split_once('"').map_or(rest, |(p, _)| p).to_owned(),
        None => value.split_whitespace().next()?.to_owned(),
    })
}

/// The program the installed start-at-login entry starts, if one is installed.
pub fn installed_program() -> Option<String> {
    if cfg!(windows) {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Silicon Extend",
            ])
            .output()
            .ok()?;
        return out
            .status
            .success()
            .then(|| reg_program(&String::from_utf8_lossy(&out.stdout)))
            .flatten();
    }
    [
        AutostartOptions::default(),
        AutostartOptions {
            systemd: true,
            ..Default::default()
        },
    ]
    .iter()
    .filter_map(|o| entry_path(o).ok())
    .filter_map(|p| std::fs::read_to_string(p).ok())
    .find_map(|text| {
        if cfg!(target_os = "macos") {
            plist_program(&text)
        } else {
            exec_program(&text)
        }
        // An entry this program can't read still counts as installed.
        .or(Some(String::new()))
    })
}

/// Turns start at login on or off for good: changes the entry and records the Carbon's choice.
pub fn set_by_carbon(state_dir: &Path, on: bool, opts: &AutostartOptions) -> Result<String> {
    let done = if on {
        if let Some(why) = exe().ok().as_deref().and_then(unfit_program) {
            anyhow::bail!("{why}");
        }
        install(opts)?
    } else {
        let removed = uninstall()?;
        if removed.is_empty() {
            "nothing was installed".into()
        } else {
            removed.join(", ")
        }
    };
    save_choice(
        state_dir,
        Choice {
            start_at_login: on,
            by: ChosenBy::Carbon,
        },
    )
    .context(
        "start at login changed, but the choice couldn't be saved, so it may be turned on again after the next pairing",
    )?;
    Ok(done)
}

/// Runs [`after_pairing`] for this program and carries it out. Returns what happened, for the log.
pub fn apply_after_pairing(state_dir: &Path) -> Result<String> {
    let program = exe()?;
    let choice = load_choice(state_dir);
    match after_pairing(choice, installed_program().as_deref(), &program) {
        Decision::Install => {
            let at = install(&AutostartOptions::default())?;
            if choice.is_none() {
                save_choice(
                    state_dir,
                    Choice {
                        start_at_login: true,
                        by: ChosenBy::Default,
                    },
                )?;
            }
            Ok(format!("start at login is on ({at})"))
        }
        Decision::Remove => {
            uninstall()?;
            Ok("start at login is off".into())
        }
        Decision::Leave(why) => Ok(format!("start at login unchanged: {why}")),
    }
}

/// True when a start-at-login entry is installed.
pub fn is_installed() -> bool {
    if cfg!(windows) {
        return std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Silicon Extend",
            ])
            .output()
            .is_ok_and(|o| o.status.success());
    }
    [
        AutostartOptions::default(),
        AutostartOptions {
            systemd: true,
            ..Default::default()
        },
    ]
    .iter()
    .filter_map(|o| entry_path(o).ok())
    .any(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed() {
        let p = launch_agent_plist(
            Path::new("/Applications/Silicon Extend.app/Contents/MacOS/extend-agent"),
            &["run".into()],
        );
        assert!(p.contains("<string>com.teamofsilicons.extend-agent</string>"));
        assert!(p.contains("<string>/Applications/Silicon Extend.app/Contents/MacOS/extend-agent</string>"));
        assert!(p.contains("<string>run</string>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        // plutil is the real judge when it's around.
        if Path::new("/usr/bin/plutil").exists() {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("x.plist");
            std::fs::write(&f, &p).unwrap();
            let out = std::process::Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(&f)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        }
    }

    const APP: &str = "/Applications/Silicon Extend.app/Contents/MacOS/extend-agent";

    #[test]
    fn nobody_chose_so_pairing_turns_it_on() {
        assert_eq!(after_pairing(None, None, Path::new(APP)), Decision::Install);
        // Already on for this copy: nothing to do.
        assert!(matches!(
            after_pairing(None, Some(APP), Path::new(APP)),
            Decision::Leave(_)
        ));
    }

    #[test]
    fn the_carbons_off_is_never_overridden() {
        for by in [ChosenBy::Carbon, ChosenBy::Default] {
            let off = Choice {
                start_at_login: false,
                by,
            };
            assert!(matches!(
                after_pairing(Some(off), None, Path::new(APP)),
                Decision::Leave(_)
            ));
            assert!(matches!(
                after_pairing(Some(off), Some("/elsewhere/extend-agent"), Path::new(APP)),
                Decision::Leave(_)
            ));
        }
    }

    #[test]
    fn a_moved_app_points_the_entry_at_itself() {
        let on = Choice {
            start_at_login: true,
            by: ChosenBy::Default,
        };
        assert_eq!(
            after_pairing(
                Some(on),
                Some("/Users/c/Downloads/Silicon Extend.app/Contents/MacOS/extend-agent"),
                Path::new(APP)
            ),
            Decision::Install
        );
        // Chosen on, then the entry was deleted by hand: that stands.
        assert!(matches!(
            after_pairing(Some(on), None, Path::new(APP)),
            Decision::Leave(_)
        ));
    }

    #[test]
    fn a_temporary_copy_is_never_what_a_login_starts() {
        let translocated = Path::new(
            "/private/var/folders/x/T/AppTranslocation/1A2B/d/Silicon Extend.app/Contents/MacOS/extend-agent",
        );
        match after_pairing(None, None, translocated) {
            Decision::Leave(why) => assert!(why.contains("Move Silicon Extend to Applications"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(
            unfit_program(Path::new(
                "/Volumes/Silicon Extend/Silicon Extend.app/Contents/MacOS/extend-agent"
            ))
            .is_some()
        );
        assert!(unfit_program(Path::new(APP)).is_none());
        assert!(unfit_program(Path::new("/usr/bin/extend-agent")).is_none());
    }

    #[test]
    fn a_development_build_is_never_registered_by_itself() {
        for dev in [
            "/Users/c/silicon-extend/target/debug/extend-agent",
            "/work/target/release/extend-agent",
            "/src/target/x86_64-pc-windows-msvc/release/extend-agent.exe",
        ] {
            assert!(is_development_build(Path::new(dev)), "{dev}");
            assert!(
                matches!(after_pairing(None, None, Path::new(dev)), Decision::Leave(_)),
                "{dev}"
            );
        }
        for installed in [APP, "/usr/bin/extend-agent", "/opt/target-tools/bin/extend-agent"] {
            assert!(!is_development_build(Path::new(installed)), "{installed}");
        }
    }

    #[test]
    fn run_flags() {
        assert_eq!(at_start(RunFlag::On, false), Decision::Install);
        assert_eq!(at_start(RunFlag::On, true), Decision::Install);
        assert_eq!(at_start(RunFlag::Off, true), Decision::Remove);
        assert!(matches!(at_start(RunFlag::Off, false), Decision::Leave(_)));
        assert!(matches!(at_start(RunFlag::Unset, true), Decision::Leave(_)));
    }

    #[test]
    fn the_choice_is_kept_privately() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_choice(dir.path()), None);
        let off = Choice {
            start_at_login: false,
            by: ChosenBy::Carbon,
        };
        save_choice(dir.path(), off).unwrap();
        assert_eq!(load_choice(dir.path()), Some(off));
        let text = std::fs::read_to_string(choice_path(dir.path())).unwrap();
        assert_eq!(text, r#"{"start_at_login":false,"by":"carbon"}"#);
        std::fs::write(choice_path(dir.path()), "not json").unwrap();
        assert_eq!(load_choice(dir.path()), None);
    }

    #[test]
    fn entries_name_the_program_they_start() {
        let plist = launch_agent_plist(
            Path::new("/Apps/A & B.app/Contents/MacOS/extend-agent"),
            &["run".into()],
        );
        assert_eq!(
            plist_program(&plist).as_deref(),
            Some("/Apps/A & B.app/Contents/MacOS/extend-agent")
        );
        let entry = xdg_desktop_entry(Path::new("/opt/Silicon Extend/extend-agent"), &["run".into()]);
        assert_eq!(
            exec_program(&entry).as_deref(),
            Some("/opt/Silicon Extend/extend-agent")
        );
        let entry = xdg_desktop_entry(Path::new("/usr/bin/extend-agent"), &["run".into()]);
        assert_eq!(exec_program(&entry).as_deref(), Some("/usr/bin/extend-agent"));
        let unit = systemd_unit(Path::new("/usr/bin/extend-agent"), &["run".into(), "--headless".into()]);
        assert_eq!(exec_program(&unit).as_deref(), Some("/usr/bin/extend-agent"));
        let reg = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    Silicon Extend    REG_SZ    \"C:\\Program Files\\Silicon Extend\\extend-agent.exe\" run\r\n";
        assert_eq!(
            reg_program(reg).as_deref(),
            Some("C:\\Program Files\\Silicon Extend\\extend-agent.exe")
        );
        assert_eq!(reg_program("ERROR: nothing"), None);
    }

    #[test]
    fn desktop_entry_quotes_paths() {
        let e = xdg_desktop_entry(Path::new("/opt/Silicon Extend/extend-agent"), &["run".into()]);
        assert!(e.contains("Exec=\"/opt/Silicon Extend/extend-agent\" run\n"));
        assert!(e.starts_with("[Desktop Entry]\n"));
        let u = systemd_unit(Path::new("/usr/bin/extend-agent"), &["run".into(), "--headless".into()]);
        assert!(u.contains("ExecStart=/usr/bin/extend-agent run --headless\n"));
    }
}
