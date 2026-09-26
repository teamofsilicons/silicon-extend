//! Where the agent keeps its state, which service it talks to, and how it finds agent-device.
//!
//! Home is `$SILICON_HOME` when set, else the OS home. State lives in `{home}/.extend-agent/`
//! (directory 0700, files 0600). An optional `{state}/config.json` holds overrides:
//!
//! ```json
//! {"service_url": "http://127.0.0.1:8480", "credential_store": "file",
//!  "agent_device": ["/opt/node/bin/node", "/opt/agent-device/bin/agent-device.mjs"]}
//! ```
//!
//! Environment variables win over the file, and command-line flags win over both:
//! `EXTEND_API_URL`, `EXTEND_AGENT_CREDENTIAL_STORE` (`auto`, `keyring`, `file`),
//! `EXTEND_AGENT_DEVICE` (path to agent-device's `bin/agent-device.mjs` or an executable),
//! `EXTEND_NODE` (the node binary).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use url::Url;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_SERVICE_URL: &str = "https://backend.extend.teamofsilicons.com";
/// Directory name under home.
pub const STATE_DIR_NAME: &str = ".extend-agent";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStoreKind {
    /// The OS secret store, falling back to a 0600 file when there is none (headless Linux).
    #[default]
    Auto,
    Keyring,
    File,
}

impl std::str::FromStr for CredentialStoreKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Ok(Self::Auto),
            "keyring" | "keychain" | "os" => Ok(Self::Keyring),
            "file" => Ok(Self::File),
            other => Err(format!("credential store must be auto, keyring or file, got {other:?}")),
        }
    }
}

/// `{state}/config.json`. Every field is optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileConfig {
    #[serde(default)]
    pub service_url: Option<String>,
    #[serde(default)]
    pub credential_store: Option<CredentialStoreKind>,
    /// Full command that runs agent-device, e.g. `["node", "/path/bin/agent-device.mjs"]`.
    #[serde(default)]
    pub agent_device: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub state_dir: PathBuf,
    pub service_url: Url,
    pub credential_store: CredentialStoreKind,
    /// The command that runs agent-device, when found.
    pub agent_device: Option<Vec<String>>,
    /// Why agent-device wasn't found, for the probe.
    pub agent_device_problem: Option<String>,
}

/// Values given on the command line.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub service_url: Option<String>,
    pub credential_store: Option<CredentialStoreKind>,
    pub home: Option<PathBuf>,
}

pub fn home_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("SILICON_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(h));
    }
    std::env::home_dir()
}

impl Config {
    pub fn load(overrides: &Overrides) -> Result<Self> {
        let home = overrides
            .home
            .clone()
            .or_else(home_dir)
            .context("couldn't find a home directory; set SILICON_HOME")?;
        let state_dir = home.join(STATE_DIR_NAME);
        ensure_private_dir(&state_dir)?;
        let file = read_file_config(&state_dir.join("config.json"))?;

        let service_raw = overrides
            .service_url
            .clone()
            .or_else(|| std::env::var("EXTEND_API_URL").ok().filter(|v| !v.trim().is_empty()))
            .or(file.service_url.clone())
            .unwrap_or_else(|| DEFAULT_SERVICE_URL.to_owned());
        let service_url = parse_service_url(&service_raw)?;

        let credential_store = match overrides.credential_store {
            Some(k) => k,
            None => match std::env::var("EXTEND_AGENT_CREDENTIAL_STORE") {
                Ok(v) if !v.trim().is_empty() => v.parse().map_err(anyhow::Error::msg)?,
                _ => file.credential_store.unwrap_or_default(),
            },
        };

        let (agent_device, agent_device_problem) = match locate_agent_device(file.agent_device.as_deref()) {
            Ok(cmd) => (Some(cmd), None),
            Err(why) => (None, Some(why)),
        };

        Ok(Self { home, state_dir, service_url, credential_store, agent_device, agent_device_problem })
    }

    /// A config for tests: everything under `dir`, the given service, a file credential store.
    pub fn for_tests(dir: &Path, service_url: &str) -> Self {
        let state_dir = dir.join(STATE_DIR_NAME);
        ensure_private_dir(&state_dir).expect("state dir");
        Self {
            home: dir.to_path_buf(),
            state_dir,
            service_url: parse_service_url(service_url).expect("service url"),
            credential_store: CredentialStoreKind::File,
            agent_device: None,
            agent_device_problem: Some("not used in tests".into()),
        }
    }

    pub fn work_dir(&self) -> PathBuf {
        self.state_dir.join("work")
    }
    pub fn status_path(&self) -> PathBuf {
        self.state_dir.join("status.json")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }
    pub fn hosted_dir(&self) -> PathBuf {
        self.state_dir.join("hosted")
    }
    /// agent-device's own state (its daemon, sessions and helper builds).
    pub fn agent_device_state_dir(&self) -> PathBuf {
        self.state_dir.join("agent-device")
    }
    /// Files that outlive one command inside a session (recordings, armed replay scripts).
    pub fn session_data_dir(&self) -> PathBuf {
        self.state_dir.join("sessions")
    }
}

pub fn parse_service_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw.trim()).with_context(|| format!("service URL {raw:?} isn't a URL"))?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "service URL must start with http:// or https://, got {raw:?}"
    );
    if !url.path().ends_with('/') {
        let p = format!("{}/", url.path());
        url.set_path(&p);
    }
    Ok(url)
}

fn read_file_config(path: &Path) -> Result<FileConfig> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("{} isn't valid JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
        Err(e) => Err(e).with_context(|| format!("couldn't read {}", path.display())),
    }
}

/// Creates `dir` (and parents) and makes it private to this user.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("couldn't make {} private", dir.display()))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` atomically (temporary file, fsync, rename), readable only by this user.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let dir = path.parent().context("file has no parent directory")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file"), std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("couldn't write {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("couldn't replace {}", path.display()))?;
    Ok(())
}

/// Finds the command that runs agent-device.
///
/// Order: `EXTEND_AGENT_DEVICE`, then `config.json`, then a copy bundled next to this executable
/// (the macOS app's `Resources/`, or `agent-device/` beside the binary on Linux and Windows), then
/// the source checkout this binary was built from (development).
pub fn locate_agent_device(from_file: Option<&[String]>) -> std::result::Result<Vec<String>, String> {
    if let Ok(raw) = std::env::var("EXTEND_AGENT_DEVICE") {
        let raw = raw.trim();
        if !raw.is_empty() {
            let path = PathBuf::from(raw);
            if !path.exists() {
                return Err(format!("EXTEND_AGENT_DEVICE points at {raw}, which doesn't exist"));
            }
            return Ok(command_for_entry(&path, None));
        }
    }
    if let Some(argv) = from_file.filter(|a| !a.is_empty()) {
        return Ok(argv.to_vec());
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(dir) = &exe_dir {
        for root in bundle_roots(dir) {
            let entry = root.join("agent-device").join("bin").join("agent-device.mjs");
            if entry.exists() {
                return Ok(command_for_entry(&entry, Some(&root)));
            }
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/agent-device/bin/agent-device.mjs");
    if dev.exists() {
        let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/agent-device/dist/src/internal/bin.js");
        if !dist.exists() {
            return Err(format!(
                "agent-device isn't built. Run `pnpm install && pnpm build` in {}",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/agent-device").display()
            ));
        }
        return Ok(command_for_entry(&dev, None));
    }
    Err("Silicon Extend couldn't find its automation helper (agent-device). Reinstall Silicon Extend.".into())
}

/// Directories a packaged agent-device and node may sit in, relative to the executable's directory.
fn bundle_roots(exe_dir: &Path) -> Vec<PathBuf> {
    vec![
        exe_dir.join("../Resources"),            // macOS: Silicon Extend.app/Contents/MacOS/extend-agent
        exe_dir.to_path_buf(),                   // Windows zip, Linux tarball
        exe_dir.join("../lib/silicon-extend"),   // Linux: /usr/bin/extend-agent + /usr/lib/silicon-extend
    ]
}

/// `[node, entry]` for a JavaScript entry point, or `[entry]` for an executable.
fn command_for_entry(entry: &Path, bundle_root: Option<&Path>) -> Vec<String> {
    let is_js = entry.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e, "js" | "mjs" | "cjs"));
    if !is_js {
        return vec![entry.display().to_string()];
    }
    vec![find_node(bundle_root), entry.display().to_string()]
}

/// The node binary: `EXTEND_NODE`, a bundled copy, then `PATH` and the usual install locations
/// (a login item starts with a minimal `PATH`).
pub fn find_node(bundle_root: Option<&Path>) -> String {
    if let Ok(n) = std::env::var("EXTEND_NODE")
        && !n.trim().is_empty()
    {
        return n;
    }
    let exe = if cfg!(windows) { "node.exe" } else { "node" };
    if let Some(root) = bundle_root {
        for candidate in [root.join("node").join("bin").join(exe), root.join("node").join(exe)] {
            if candidate.exists() {
                return candidate.display().to_string();
            }
        }
    }
    if let Some(found) = which(exe) {
        return found.display().to_string();
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let p = Path::new(dir).join(exe);
        if p.exists() {
            return p.display().to_string();
        }
    }
    exe.to_owned()
}

/// Looks a program up on `PATH`.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            let c = dir.join(format!("{program}.{ext}"));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_urls_get_a_trailing_slash() {
        let u = parse_service_url("http://127.0.0.1:8480").unwrap();
        assert_eq!(u.as_str(), "http://127.0.0.1:8480/");
        assert_eq!(u.join("api/v1/enrollments").unwrap().as_str(), "http://127.0.0.1:8480/api/v1/enrollments");
        assert!(parse_service_url("ftp://x").is_err());
        assert!(parse_service_url("nope").is_err());
    }

    #[test]
    fn credential_store_names() {
        assert_eq!("file".parse::<CredentialStoreKind>().unwrap(), CredentialStoreKind::File);
        assert_eq!("Keychain".parse::<CredentialStoreKind>().unwrap(), CredentialStoreKind::Keyring);
        assert!("x".parse::<CredentialStoreKind>().is_err());
    }

    #[test]
    fn private_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b.json");
        write_private_file(&p, b"{}").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn js_entries_run_through_node() {
        let cmd = command_for_entry(Path::new("/x/bin/agent-device.mjs"), None);
        assert_eq!(cmd.len(), 2);
        assert!(cmd[1].ends_with("agent-device.mjs"));
        let cmd = command_for_entry(Path::new("/x/agent-device"), None);
        assert_eq!(cmd, vec!["/x/agent-device".to_string()]);
    }
}
