//! Everything the CLI keeps on disk, under `{home}/.bridge/` (`understanding/cli.yaml`, state_files).
//!
//! `home` is `$SILICON_HOME`, else the user's home directory. `bridge config home <dir>` moves the
//! state; the chosen directory is recorded in `{default}/.bridge/home` so later runs find it.

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
    home.join(".bridge")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// The directory state actually lives in.
pub fn root() -> PathBuf {
    let default = default_root();
    match fs::read_to_string(default.join("home")) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()).join(".bridge"),
        _ => default,
    }
}

pub fn set_home(dir: &Path) -> anyhow::Result<PathBuf> {
    if !dir.is_dir() {
        bail!("not a directory: {}", dir.display());
    }
    let dir = dir.canonicalize()?;
    let default = default_root();
    fs::create_dir_all(&default)?;
    write_private(&default.join("home"), dir.to_string_lossy().as_bytes())?;
    let new_root = dir.join(".bridge");
    fs::create_dir_all(&new_root)?;
    Ok(new_root)
}

/// Writes a file readable only by the user, atomically (write, sync, rename).
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
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
    fs::rename(&tmp, path)?;
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
    pub secret: String,
    pub name: Option<String>,
    pub auth: Option<Auth>,
}

impl Plane {
    pub fn auth_path(&self) -> PathBuf {
        match self {
            Plane::Production => root().join("auth.json"),
            Plane::Test { id, .. } => root().join("test").join(format!("{id}.json")),
        }
    }
    pub fn is_test(&self) -> bool {
        matches!(self, Plane::Test { .. })
    }
}

pub fn load_test(id: &str) -> anyhow::Result<TestEnv> {
    let path = root().join("test").join(format!("{id}.json"));
    let raw = fs::read(&path).with_context(|| format!("test environment {id} is not added (run `bridge config test add {id}` and paste its app secret)"))?;
    Ok(serde_json::from_slice(&raw)?)
}

pub fn save_test(id: &str, env: &TestEnv) -> anyhow::Result<()> {
    write_private(&root().join("test").join(format!("{id}.json")), &serde_json::to_vec_pretty(env)?)
}

pub fn list_tests() -> Vec<(String, TestEnv)> {
    let Ok(dir) = fs::read_dir(root().join("test")) else { return vec![] };
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
        Plane::Test { id, .. } => load_test(id).ok()?.auth,
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
            let mut env = load_test(id)?;
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
                    let stale = fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() > 30);
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

pub const CONFIG_KEYS: &[(&str, &str)] = &[
    ("api_url", "Bridge service URL (default https://backend.bridge.teamofsilicons.com)"),
    ("telemetry", "on|off (default on)"),
    ("output", "text|json (default text)"),
    ("team", "default team handle"),
    ("screenshot_scale", "0.01–1, used when screenshot has no --scale"),
    ("self_destruct", "default file self-destruct, like 1d, 90m, 30d (1m–30d, default 1d)"),
    ("download_dir", "where --out and `bridge file get` save by default"),
    ("color", "auto|always|never"),
];

pub fn load_config() -> BTreeMap<String, String> {
    let Ok(raw) = fs::read_to_string(root().join("config.toml")) else { return BTreeMap::new() };
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
    let mut out = String::from("# Silicon Bridge CLI settings. Edit with `bridge config set <key> <value>`.\n");
    for (k, v) in cfg {
        out.push_str(&format!("{k} = \"{}\"\n", v.replace('"', "\\\"")));
    }
    write_private(&root().join("config.toml"), out.as_bytes())
}

// ── Sessions ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCache {
    pub session_id: String,
    pub device_id: String,
    pub device_name: String,
    pub os: String,
    pub capabilities: Vec<String>,
    pub commands: Vec<String>,
    pub test_id: Option<String>,
}

fn sessions_dir(plane: &Plane) -> PathBuf {
    match plane {
        Plane::Production => root().join("sessions"),
        Plane::Test { id, .. } => root().join("sessions").join(format!("test-{id}")),
    }
}

pub fn current_session(plane: &Plane) -> Option<String> {
    fs::read_to_string(sessions_dir(plane).join("current")).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
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
    write_private(&sessions_dir(plane).join(format!("{}.json", c.session_id)), &serde_json::to_vec_pretty(c)?)
}

pub fn load_session_cache(plane: &Plane, id: &str) -> Option<SessionCache> {
    serde_json::from_slice(&fs::read(sessions_dir(plane).join(format!("{id}.json"))).ok()?).ok()
}
