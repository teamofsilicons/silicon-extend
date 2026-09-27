//! Everything the CLI keeps on disk, under `{home}/.extend/` (`understanding/cli.yaml`, state_files).
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

/// The state files and directories, relative to the state directory.
pub const STATE_ENTRIES: &[&str] = &["auth.json", "config.toml", "test", "sessions"];

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Auth {
    pub api_url: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when the access token expires.
    pub expires_at: i64,
    pub member_id: String,
    pub member_kind: String,
    pub teams: Vec<String>,
    pub team: Option<String>,
}

/// Where login state lives: production, or one test environment.
#[derive(Debug, Clone)]
pub enum Plane {
    Production,
    Test { id: String, secret: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestEnv {
    /// Saved by `extend config test add`. Empty when the environment is used through
    /// `EXTEND_TEST_SECRET`, which is never written to disk.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
    pub name: Option<String>,
    pub auth: Option<Auth>,
    /// Digest of an `EXTEND_TEST_SECRET` already checked to belong to this environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_env_secret: Option<String>,
}

impl Plane {
    pub fn auth_path(&self) -> PathBuf {
        match self {
            Plane::Production => root().join("auth.json"),
            Plane::Test { id, .. } => test_path(id),
        }
    }
    pub fn is_test(&self) -> bool {
        matches!(self, Plane::Test { .. })
    }
}

fn test_path(id: &str) -> PathBuf {
    root().join("test").join(format!("{id}.json"))
}

/// The saved test environment `id`, or `None` when it was never added or used.
pub fn find_test(id: &str) -> anyhow::Result<Option<TestEnv>> {
    let path = test_path(id);
    match fs::read(&path) {
        Ok(raw) => Ok(Some(
            serde_json::from_slice(&raw).with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save_test(id: &str, env: &TestEnv) -> anyhow::Result<()> {
    write_private(&test_path(id), &serde_json::to_vec_pretty(env)?)
}

pub fn remove_test(id: &str) -> bool {
    fs::remove_file(test_path(id)).is_ok()
}

pub fn list_tests() -> Vec<(String, TestEnv)> {
    let Ok(dir) = fs::read_dir(root().join("test")) else {
        return vec![];
    };
    let mut v: Vec<_> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".json")?.to_owned();
            let env: TestEnv = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            Some((id, env))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

pub fn load_auth(plane: &Plane) -> Option<Auth> {
    match plane {
        Plane::Production => serde_json::from_slice(&fs::read(plane.auth_path()).ok()?).ok(),
        Plane::Test { id, .. } => find_test(id).ok()??.auth,
    }
}

pub fn save_auth(plane: &Plane, auth: Option<&Auth>) -> anyhow::Result<()> {
    match plane {
        Plane::Production => match auth {
            Some(a) => write_private(&plane.auth_path(), &serde_json::to_vec_pretty(a)?),
            None => {
                let _ = fs::remove_file(plane.auth_path());
                Ok(())
            }
        },
        Plane::Test { id, .. } => {
            let mut env = find_test(id)?.unwrap_or_default();
            env.auth = auth.cloned();
            save_test(id, &env)
        }
    }
}

/// A simple lock so two CLI processes don't refresh the same token at once.
pub struct Lock(PathBuf);

impl Lock {
    pub fn acquire(name: &str) -> Self {
        let path = root().join(format!("{name}.lock"));
        let _ = fs::create_dir_all(root());
        for _ in 0..200 {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Self(path),
                Err(_) => {
                    // A lock older than 30 s belongs to a process that died.
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|e| e.as_secs() > 30);
                    if stale {
                        let _ = fs::remove_file(&path);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        Self(path)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
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
        about: "Extend service URL: https, or http for a local address only",
        default: "https://backend.extend.teamofsilicons.com",
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
        key: "team",
        about: "default team handle (a team this login reaches)",
        default: "the first team of the login",
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
    pub test_id: Option<String>,
    #[serde(default)]
    pub missing: Vec<MissingNote>,
    /// Unix seconds of the last refresh from the service.
    #[serde(default)]
    pub refreshed_at: i64,
}

fn sessions_dir(plane: &Plane) -> PathBuf {
    match plane {
        Plane::Production => root().join("sessions"),
        Plane::Test { id, .. } => root().join("sessions").join(format!("test-{id}")),
    }
}

pub fn current_session(plane: &Plane) -> Option<String> {
    fs::read_to_string(sessions_dir(plane).join("current"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

pub fn set_current_session(plane: &Plane, id: Option<&str>) -> anyhow::Result<()> {
    let path = sessions_dir(plane).join("current");
    match id {
        Some(id) => write_private(&path, id.as_bytes()),
        None => {
            let _ = fs::remove_file(path);
            Ok(())
        }
    }
}

pub fn save_session_cache(plane: &Plane, c: &SessionCache) -> anyhow::Result<()> {
    write_private(
        &sessions_dir(plane).join(format!("{}.json", c.session_id)),
        &serde_json::to_vec_pretty(c)?,
    )
}

pub fn load_session_cache(plane: &Plane, id: &str) -> Option<SessionCache> {
    serde_json::from_slice(&fs::read(sessions_dir(plane).join(format!("{id}.json"))).ok()?).ok()
}

pub fn remove_session_cache(plane: &Plane, id: &str) {
    let _ = fs::remove_file(sessions_dir(plane).join(format!("{id}.json")));
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
