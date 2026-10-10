//! Everything the CLI keeps on disk, under `{home}/.extend/` (`docs/migration/contracts/cli.yaml`,
//! state_files): the sign-in (`auth.json`, see `signin.rs`), settings (`config.toml`) and what it
//! last read about device sessions (`sessions/`).
//!
//! `home` is `$SILICON_HOME`, else the user's home directory. `extend config home <dir>` moves the
//! state to `<dir>/.extend`; the chosen directory is recorded in `{default}/.extend/home` so later
//! runs find it.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};

pub fn default_root() -> PathBuf {
    let home = std::env::var_os("SILICON_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".extend")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The file in the default state directory that says where state lives instead.
fn pointer() -> PathBuf {
    default_root().join("home")
}

/// The directory state actually lives in.
pub fn root() -> PathBuf {
    match fs::read_to_string(pointer()) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()).join(".extend"),
        _ => default_root(),
    }
}

/// Where state would live for home directory `dir`, which must exist.
pub fn root_for_home(dir: &Path) -> anyhow::Result<PathBuf> {
    if !dir.is_dir() {
        bail!("not a directory: {}", dir.display());
    }
    Ok(dir
        .canonicalize()
        .with_context(|| format!("reading {}", dir.display()))?
        .join(".extend"))
}

/// Makes `new_root` (a `<dir>/.extend`) where later runs keep state.
pub fn point_to(new_root: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(new_root).with_context(|| format!("creating {}", new_root.display()))?;
    let default = default_root();
    let same = |a: &Path, b: &Path| {
        a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
    };
    if same(new_root, &default) {
        // Back to the default: no pointer needed.
        let _ = fs::remove_file(pointer());
        return Ok(());
    }
    let home = new_root.parent().context("a state directory has a parent")?;
    write_private(&pointer(), home.to_string_lossy().as_bytes())
}

/// The state files and directories, relative to the state directory. `test` and `contexts` are
/// Extend 3's (test environments and per-organization sign-ins): moved along so nothing is left
/// behind, and removed at the next sign-in or sign-out.
pub const STATE_ENTRIES: &[&str] = &["auth.json", "config.toml", "test", "sessions", "contexts"];

/// Which of [`STATE_ENTRIES`] exist in `root`.
pub fn state_in(root: &Path) -> Vec<&'static str> {
    STATE_ENTRIES
        .iter()
        .copied()
        .filter(|e| root.join(e).exists())
        .collect()
}

/// Copies the state in `from` to `to` (files stay private to the user). Returns what it copied.
pub fn copy_state(from: &Path, to: &Path) -> anyhow::Result<Vec<&'static str>> {
    let entries = state_in(from);
    for e in &entries {
        copy_tree(&from.join(e), &to.join(e))?;
    }
    Ok(entries)
}

fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(to, fs::Permissions::from_mode(0o700));
        }
        for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            // Unfinished writes and locks belong to the running process only.
            if name.to_string_lossy().contains(".tmp") || name.to_string_lossy().ends_with(".lock") {
                continue;
            }
            copy_tree(&entry.path(), &to.join(name))?;
        }
        Ok(())
    } else {
        let bytes = fs::read(from).with_context(|| format!("reading {}", from.display()))?;
        write_private(to, &bytes)
    }
}

/// Deletes `entries` from `root`, and `root` itself if nothing else is left in it.
pub fn remove_state(root: &Path, entries: &[&str]) -> anyhow::Result<()> {
    for e in entries {
        let p = root.join(e);
        if p.is_dir() {
            fs::remove_dir_all(&p).with_context(|| format!("removing {}", p.display()))?;
        } else if p.exists() {
            fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
    }
    // Only succeeds when it's empty; the default directory keeps the pointer file.
    let _ = fs::remove_dir(root);
    Ok(())
}

/// Writes a file readable only by the user, atomically (write, sync, rename).
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ── config.toml: flat `key = "value"` lines ──

/// One setting: its key, what it does and takes, and its default.
pub struct Setting {
    pub key: &'static str,
    pub about: &'static str,
    pub default: &'static str,
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "api_url",
        about: "Extend service URL: https, or http for this machine only (EXTEND_API_URL overrides it)",
        default: "https://backend.extend.teamofsilicons.com",
    },
    Setting {
        key: "accounts_url",
        about: "Silicon Accounts URL to sign in at: https, or http for this machine only (ACCOUNTS_URL overrides it)",
        default: "https://accounts.teamofsilicons.com",
    },
    Setting {
        key: "telemetry",
        about: "on|off",
        default: "on",
    },
    Setting {
        key: "output",
        about: "text|json; json makes --json the default",
        default: "text",
    },
    Setting {
        key: "screenshot_scale",
        about: "0.01–1, used when screenshot has no --scale",
        default: "1",
    },
    Setting {
        key: "self_destruct",
        about: "default file self-destruct, 1m–30d, like 90m, 12h, 7d",
        default: "1d",
    },
    Setting {
        key: "download_dir",
        about: "an existing directory where `extend file get` saves without --out",
        default: "the current directory",
    },
    Setting {
        key: "color",
        about: "auto|always|never; auto colours a terminal unless NO_COLOR is set",
        default: "auto",
    },
];

pub fn setting(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key)
}

pub fn load_config() -> BTreeMap<String, String> {
    let Ok(raw) = fs::read_to_string(root().join("config.toml")) else {
        return BTreeMap::new();
    };
    raw.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') || l.is_empty() {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_owned(), v.trim().trim_matches('"').replace("\\\"", "\"")))
        })
        .collect()
}

pub fn save_config(cfg: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let mut out = String::from("# Silicon Extend CLI settings. Edit with `extend config set <key> <value>`.\n");
    for (k, v) in cfg {
        out.push_str(&format!("{k} = \"{}\"\n", v.replace('"', "\\\"")));
    }
    write_private(&root().join("config.toml"), out.as_bytes())
}

// ── Sessions ──

/// Why the connected device can't do something, as the service said at the last refresh.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MissingNote {
    pub capability: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCache {
    pub session_id: String,
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub commands: Vec<String>,
    #[serde(default)]
    pub missing: Vec<MissingNote>,
    /// Unix seconds of the last refresh from the service.
    #[serde(default)]
    pub refreshed_at: i64,
}

/// Where one account's device sessions are cached: `sessions/acct-<hex of its uuid>/`. Uuids are
/// case-sensitive (`zQo` is not `ZQO`) and some file systems are not, so the directory name spells
/// the uuid's bytes in hex. Signed out, `sessions/` itself.
pub fn sessions_dir(account: Option<&str>) -> PathBuf {
    let base = root().join("sessions");
    match account {
        Some(uuid) => base.join(format!("acct-{}", extend_protocol::ids::hex_lower(uuid.as_bytes()))),
        None => base,
    }
}

pub fn current_session(account: Option<&str>) -> Option<String> {
    fs::read_to_string(sessions_dir(account).join("current"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub fn set_current_session(account: Option<&str>, id: Option<&str>) -> anyhow::Result<()> {
    let path = sessions_dir(account).join("current");
    match id {
        Some(id) => write_private(&path, id.as_bytes()),
        None => {
            let _ = fs::remove_file(path);
            Ok(())
        }
    }
}

pub fn save_session_cache(account: Option<&str>, c: &SessionCache) -> anyhow::Result<()> {
    write_private(
        &sessions_dir(account).join(format!("{}.json", c.session_id)),
        &serde_json::to_vec_pretty(c)?,
    )
}

pub fn load_session_cache(account: Option<&str>, id: &str) -> Option<SessionCache> {
    serde_json::from_slice(&fs::read(sessions_dir(account).join(format!("{id}.json"))).ok()?).ok()
}

pub fn remove_session_cache(account: Option<&str>, id: &str) {
    let _ = fs::remove_file(sessions_dir(account).join(format!("{id}.json")));
}

/// Forgets every cached device session of one account (it signed out).
pub fn forget_sessions(account: &str) {
    let _ = fs::remove_dir_all(sessions_dir(Some(account)));
}

/// Extend 3's state that Extend 4 no longer reads: per-organization sign-ins (`contexts/`), test
/// environments (`test/`), the lock around them, and session caches kept per organization context
/// (`sessions/<64 hex>/`, and `sessions/current` from before sign-in). Returns what is there.
pub fn legacy_state() -> Vec<PathBuf> {
    let r = root();
    let mut found: Vec<PathBuf> = ["contexts", "test", "auth-context.lock"]
        .iter()
        .map(|e| r.join(e))
        .filter(|p| p.exists())
        .collect();
    if let Ok(dir) = fs::read_dir(r.join("sessions")) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                found.push(e.path());
            }
        }
    }
    found
}

/// Deletes [`legacy_state`]. Returns what it deleted.
pub fn remove_legacy_state() -> Vec<PathBuf> {
    let found = legacy_state();
    for p in &found {
        if p.is_dir() {
            let _ = fs::remove_dir_all(p);
        } else {
            let _ = fs::remove_file(p);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_state_moves_login_settings_tests_and_sessions_but_not_locks() {
        let base = std::env::temp_dir().join(format!("extend-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (from, to) = (base.join("old/.extend"), base.join("new/.extend"));
        write_private(&from.join("auth.json"), b"{\"member_id\":\"si:chef\"}").unwrap();
        write_private(&from.join("config.toml"), b"telemetry = \"off\"\n").unwrap();
        write_private(&from.join("test/9b3e.json"), b"{}").unwrap();
        write_private(&from.join("sessions/current"), b"a3f").unwrap();
        write_private(&from.join("sessions/test-9b3e/current"), b"b4c").unwrap();
        fs::write(from.join("refresh.lock"), b"").unwrap();
        fs::write(from.join("home"), b"/elsewhere").unwrap();

        let copied = copy_state(&from, &to).unwrap();
        assert_eq!(copied, vec!["auth.json", "config.toml", "test", "sessions"]);
        assert_eq!(
            fs::read_to_string(to.join("config.toml")).unwrap(),
            "telemetry = \"off\"\n"
        );
        assert_eq!(
            fs::read_to_string(to.join("sessions/test-9b3e/current")).unwrap(),
            "b4c"
        );
        assert!(!to.join("refresh.lock").exists() && !to.join("home").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(to.join("auth.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        remove_state(&from, &copied).unwrap();
        assert!(state_in(&from).is_empty());
        assert!(from.join("home").exists(), "the pointer file is not state");
        let _ = fs::remove_dir_all(base);
    }
}
