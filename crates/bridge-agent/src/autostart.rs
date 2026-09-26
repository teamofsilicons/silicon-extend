//! Start at login: a LaunchAgent on macOS, a Run key on Windows, an XDG autostart entry (or a
//! systemd user unit, for servers) on Linux.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

pub const LABEL: &str = "com.teamofsilicons.bridge-agent";

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

/// The XDG autostart entry (`~/.config/autostart/silicon-bridge.desktop`).
pub fn xdg_desktop_entry(program: &Path, args: &[String]) -> String {
    let quote = |s: &str| if s.contains(' ') { format!("\"{}\"", s.replace('"', "\\\"")) } else { s.to_owned() };
    let mut exec = quote(&program.display().to_string());
    for a in args {
        exec.push(' ');
        exec.push_str(&quote(a));
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Silicon Bridge\nComment=Lets the Silicons you choose use this computer\nExec={exec}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nNoDisplay=false\n"
    )
}

/// A systemd user unit (`~/.config/systemd/user/silicon-bridge.service`).
pub fn systemd_unit(program: &Path, args: &[String]) -> String {
    let exec = std::iter::once(program.display().to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
    format!(
        "[Unit]\nDescription=Silicon Bridge device agent\nAfter=network-online.target\n\n[Service]\nExecStart={exec}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
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
        Ok(PathBuf::from(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Silicon Bridge"))
    } else if opts.systemd {
        Ok(xdg_config()?.join("systemd/user/silicon-bridge.service"))
    } else {
        Ok(xdg_config()?.join("autostart/silicon-bridge.desktop"))
    }
}

/// Installs the start-at-login entry. Returns where it went.
pub fn install(opts: &AutostartOptions) -> Result<String> {
    let program = exe()?;
    let args = run_args(opts);
    let path = entry_path(opts)?;
    if cfg!(windows) {
        let value = std::iter::once(format!("\"{}\"", program.display())).chain(args).collect::<Vec<_>>().join(" ");
        let out = std::process::Command::new("reg")
            .args(["add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "Silicon Bridge", "/t", "REG_SZ", "/d", &value, "/f"])
            .output()
            .context("couldn't run reg.exe")?;
        anyhow::ensure!(out.status.success(), "reg.exe failed: {}", String::from_utf8_lossy(&out.stderr));
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
        let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status();
        let _ = std::process::Command::new("systemctl").args(["--user", "enable", "silicon-bridge.service"]).status();
    }
    Ok(path.display().to_string())
}

/// Removes the start-at-login entries this program may have installed.
pub fn uninstall() -> Result<Vec<String>> {
    let mut removed = Vec::new();
    if cfg!(windows) {
        let out = std::process::Command::new("reg")
            .args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "Silicon Bridge", "/f"])
            .output()
            .context("couldn't run reg.exe")?;
        if out.status.success() {
            removed.push(entry_path(&AutostartOptions::default())?.display().to_string());
        }
        return Ok(removed);
    }
    for opts in [AutostartOptions::default(), AutostartOptions { systemd: true, ..Default::default() }] {
        let path = entry_path(&opts)?;
        if path.exists() {
            if opts.systemd {
                let _ = std::process::Command::new("systemctl").args(["--user", "disable", "silicon-bridge.service"]).status();
            }
            std::fs::remove_file(&path).with_context(|| format!("couldn't remove {}", path.display()))?;
            removed.push(path.display().to_string());
        }
    }
    Ok(removed)
}

/// True when a start-at-login entry is installed.
pub fn is_installed() -> bool {
    if cfg!(windows) {
        return std::process::Command::new("reg")
            .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "Silicon Bridge"])
            .output()
            .is_ok_and(|o| o.status.success());
    }
    [AutostartOptions::default(), AutostartOptions { systemd: true, ..Default::default() }]
        .iter()
        .filter_map(|o| entry_path(o).ok())
        .any(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_well_formed() {
        let p = launch_agent_plist(Path::new("/Applications/Silicon Bridge.app/Contents/MacOS/bridge-agent"), &["run".into()]);
        assert!(p.contains("<string>com.teamofsilicons.bridge-agent</string>"));
        assert!(p.contains("<string>/Applications/Silicon Bridge.app/Contents/MacOS/bridge-agent</string>"));
        assert!(p.contains("<string>run</string>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        // plutil is the real judge when it's around.
        if Path::new("/usr/bin/plutil").exists() {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("x.plist");
            std::fs::write(&f, &p).unwrap();
            let out = std::process::Command::new("/usr/bin/plutil").arg("-lint").arg(&f).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        }
    }

    #[test]
    fn desktop_entry_quotes_paths() {
        let e = xdg_desktop_entry(Path::new("/opt/Silicon Bridge/bridge-agent"), &["run".into()]);
        assert!(e.contains("Exec=\"/opt/Silicon Bridge/bridge-agent\" run\n"));
        assert!(e.starts_with("[Desktop Entry]\n"));
        let u = systemd_unit(Path::new("/usr/bin/bridge-agent"), &["run".into(), "--headless".into()]);
        assert!(u.contains("ExecStart=/usr/bin/bridge-agent run --headless\n"));
    }
}
