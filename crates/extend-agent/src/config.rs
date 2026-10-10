//! Where the agent keeps its state, which service it talks to, and how it finds the device engine.
//!
//! Home is `$SILICON_HOME` when set, else the OS home. State lives in `{home}/.extend-agent/`
//! (directory 0700, files 0600). An optional `{state}/config.json` holds overrides:
//!
//! ```json
//! {"service_url": "http://127.0.0.1:8480", "credential_store": "file",
//!  "engine": ["/opt/node/bin/node", "/opt/extend-engine/bin/extend-engine.mjs"]}
//! ```
//!
//! Environment variables win over the file, and command-line flags win over both:
//! `EXTEND_API_URL`, `EXTEND_AGENT_CREDENTIAL_STORE` (`auto`, `keyring`, `file`),
//! `EXTEND_ENGINE` (path to the device engine's `bin/extend-engine.mjs` or an executable),
//! `EXTEND_NODE` (the node binary), `EXTEND_DOWNLOAD_URL` (`download_url` in the file: where
//! "Download the update" goes when Extend needs a newer app; by default the website's download
//! page for this OS, [`default_download_url`]).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use url::Url;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_SERVICE_URL: &str = "https://api.extend.teamofsilicons.com";
/// Directory name under home.
pub const STATE_DIR_NAME: &str = ".extend-agent";
/// The Extend website, whose `/download/<platform>` pages have the apps (`web/src/config.ts`).
pub const DEFAULT_WEBSITE_URL: &str = "https://extend.teamofsilicons.com";

/// The website's download page for this OS: `{website}/download/mac|windows|linux`.
pub fn default_download_url() -> String {
    let platform = if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    };
    format!("{DEFAULT_WEBSITE_URL}/download/{platform}")
}

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
    /// Full command that runs the device engine, e.g. `["node", "/path/bin/extend-engine.mjs"]`.
    #[serde(default)]
    pub engine: Option<Vec<String>>,
    /// The same, under the key 1.0 used; `engine` wins when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_device: Option<Vec<String>>,
    /// Where "Download the update" goes (http or https).
    #[serde(default)]
    pub download_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub state_dir: PathBuf,
    pub service_url: Url,
    pub credential_store: CredentialStoreKind,
    /// The command that runs the device engine, when found.
    pub agent_device: Option<Vec<String>>,
    /// Why the device engine wasn't found, for the probe.
    pub agent_device_problem: Option<String>,
    /// Where "Download the update" goes when Extend needs a newer app.
    pub download_url: Url,
}

/// Values given on the command line.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub service_url: Option<String>,
    pub credential_store: Option<CredentialStoreKind>,
    pub home: Option<PathBuf>,
    pub download_url: Option<String>,
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
        adopt_old_engine_state(&state_dir);
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

        let from_file = file.engine.as_deref().or(file.agent_device.as_deref());
        let (agent_device, agent_device_problem) = match locate_agent_device(from_file) {
            Ok(cmd) => (Some(cmd), None),
            Err(why) => (None, Some(why)),
        };

        let download_raw = overrides
            .download_url
            .clone()
            .or_else(|| {
                std::env::var("EXTEND_DOWNLOAD_URL")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .or(file.download_url.clone())
            .unwrap_or_else(default_download_url);
        let download_url = parse_download_url(&download_raw)?;

        Ok(Self {
            home,
            state_dir,
            service_url,
            credential_store,
            agent_device,
            agent_device_problem,
            download_url,
        })
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
            download_url: parse_download_url(&default_download_url()).expect("download url"),
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
    /// The device engine's own state (its daemon, sessions and helper builds).
    pub fn agent_device_state_dir(&self) -> PathBuf {
        self.state_dir.join(ENGINE_STATE_DIR_NAME)
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

/// The download page, which is opened in the Carbon's browser: only http and https.
pub fn parse_download_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw.trim()).with_context(|| {
        format!("the download URL {raw:?} isn't a URL; set download_url (or EXTEND_DOWNLOAD_URL) to the page with the Silicon Extend apps")
    })?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host().is_some(),
        "the download URL must start with http:// or https:// and name a host, got {raw:?}; set download_url (or EXTEND_DOWNLOAD_URL) to the page with the Silicon Extend apps"
    );
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
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("couldn't write {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("couldn't replace {}", path.display()))?;
    Ok(())
}

/// The device engine's state directory under the agent's state directory.
pub const ENGINE_STATE_DIR_NAME: &str = "engine";
/// Its name before 1.1; [`adopt_old_engine_state`] moves it once.
const OLD_ENGINE_STATE_DIR_NAME: &str = "agent-device";
/// Shown when no device engine is found: the Carbon's way out is reinstalling the app.
pub const ENGINE_MISSING: &str = "Silicon Extend's device engine is missing. Reinstall Silicon Extend.";

/// Moves the engine state an earlier version kept in `{state}/agent-device` (1.0's name) to `{state}/engine`,
/// once, so an update keeps the engine's helper builds and sessions. Best effort: when it can't be
/// moved the engine starts over in a fresh directory, which costs a helper rebuild and nothing else.
pub fn adopt_old_engine_state(state_dir: &Path) {
    let old = state_dir.join(OLD_ENGINE_STATE_DIR_NAME);
    let new = state_dir.join(ENGINE_STATE_DIR_NAME);
    if !old.is_dir() || new.symlink_metadata().is_ok() {
        return;
    }
    if let Err(e) = std::fs::rename(&old, &new) {
        tracing::warn!(from = %old.display(), to = %new.display(), "couldn't move the device engine's state: {e}");
    }
}

/// The locator environment variables, newest first: `EXTEND_AGENT_DEVICE` is 1.0's name.
const ENGINE_ENV_VARS: [&str; 2] = ["EXTEND_ENGINE", "EXTEND_AGENT_DEVICE"];

/// Finds the command that runs the device engine.
///
/// Order: `EXTEND_ENGINE` (or 1.0's `EXTEND_AGENT_DEVICE`), then `config.json` (the caller passes
/// its `engine`, or 1.0's `agent_device`), then a copy bundled next to this executable (the macOS
/// app's `Resources/engine/`, or `engine/` beside the binary on Linux and Windows), then the source
/// checkout this binary was built from (development).
pub fn locate_agent_device(from_file: Option<&[String]>) -> std::result::Result<Vec<String>, String> {
    for var in ENGINE_ENV_VARS {
        let Ok(raw) = std::env::var(var) else { continue };
        let raw = raw.trim();
        if !raw.is_empty() {
            let path = PathBuf::from(raw);
            if !path.exists() {
                return Err(format!("{var} points at {raw}, which doesn't exist"));
            }
            return Ok(command_for_entry(&path, None));
        }
    }
    if let Some(argv) = from_file.filter(|a| !a.is_empty()) {
        return Ok(argv.to_vec());
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(dir) = &exe_dir {
        for root in bundle_roots(dir) {
            let entry = bundled_engine_entry(&root);
            if entry.exists() {
                return Ok(command_for_entry(&entry, Some(&root)));
            }
        }
    }
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/extend-engine");
    let dev = checkout.join("bin").join("extend-engine.mjs");
    if dev.exists() {
        if !checkout.join("dist/src/internal/bin.js").exists() {
            return Err(format!(
                "The device engine isn't built. Run `pnpm install && pnpm build` in {}",
                checkout.display()
            ));
        }
        return Ok(command_for_entry(&dev, None));
    }
    Err(ENGINE_MISSING.into())
}

/// Where packaging puts the engine's entry under a bundle root (`apps/desktop/packaging.sh`).
fn bundled_engine_entry(root: &Path) -> PathBuf {
    root.join("engine").join("bin").join("extend-engine.mjs")
}

/// Directories a packaged device engine and node may sit in, relative to the executable's directory.
fn bundle_roots(exe_dir: &Path) -> Vec<PathBuf> {
    vec![
        exe_dir.join("../Resources"), // macOS: Silicon Extend.app/Contents/MacOS/extend-agent
        exe_dir.to_path_buf(),        // Windows zip, Linux tarball
        exe_dir.join("../lib/silicon-extend"), // Linux: /usr/bin/extend-agent + /usr/lib/silicon-extend
    ]
}

/// `[node, entry]` for a JavaScript entry point, or `[entry]` for an executable.
fn command_for_entry(entry: &Path, bundle_root: Option<&Path>) -> Vec<String> {
    let is_js = entry
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "js" | "mjs" | "cjs"));
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
        assert_eq!(
            u.join("api/v1/enrollments").unwrap().as_str(),
            "http://127.0.0.1:8480/api/v1/enrollments"
        );
        assert!(parse_service_url("ftp://x").is_err());
        assert!(parse_service_url("nope").is_err());
    }

    #[test]
    fn download_urls_come_from_configuration() {
        let d = default_download_url();
        assert!(d.starts_with("https://extend.teamofsilicons.com/download/"), "{d}");
        assert!(["mac", "windows", "linux"].iter().any(|p| d.ends_with(p)), "{d}");
        assert_eq!(
            parse_download_url("https://downloads.example.com/extend/mac?channel=beta")
                .unwrap()
                .as_str(),
            "https://downloads.example.com/extend/mac?channel=beta"
        );
        // Opened in the browser, so nothing that could run something else.
        for bad in ["file:///Applications", "javascript:alert(1)", "nope", "https://"] {
            let e = parse_download_url(bad).unwrap_err().to_string();
            assert!(e.contains("download_url"), "{bad}: {e}");
        }
        // config.json's download_url is read, and a broken one refuses to start with why.
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(STATE_DIR_NAME);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("config.json"),
            r#"{"download_url":"https://mirror.example.org/silicon-extend"}"#,
        )
        .unwrap();
        let c = Config::load(&Overrides {
            home: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        // EXTEND_DOWNLOAD_URL, when set in the environment running the tests, wins over the file.
        if std::env::var_os("EXTEND_DOWNLOAD_URL").is_none() {
            assert_eq!(c.download_url.as_str(), "https://mirror.example.org/silicon-extend");
        }
        let c = Config::load(&Overrides {
            home: Some(dir.path().to_path_buf()),
            download_url: Some("https://flag.example.org/get".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(c.download_url.as_str(), "https://flag.example.org/get");
    }

    #[test]
    fn credential_store_names() {
        assert_eq!(
            "file".parse::<CredentialStoreKind>().unwrap(),
            CredentialStoreKind::File
        );
        assert_eq!(
            "Keychain".parse::<CredentialStoreKind>().unwrap(),
            CredentialStoreKind::Keyring
        );
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
        let cmd = command_for_entry(Path::new("/x/bin/extend-engine.mjs"), None);
        assert_eq!(cmd.len(), 2);
        assert!(cmd[1].ends_with("extend-engine.mjs"));
        let cmd = command_for_entry(Path::new("/x/extend-engine"), None);
        assert_eq!(cmd, vec!["/x/extend-engine".to_string()]);
    }

    #[test]
    fn the_bundled_engine_sits_under_engine() {
        assert_eq!(
            bundled_engine_entry(Path::new("/app/Resources")),
            Path::new("/app/Resources/engine/bin/extend-engine.mjs")
        );
        assert_eq!(
            ENGINE_MISSING,
            "Silicon Extend's device engine is missing. Reinstall Silicon Extend."
        );
    }

    #[test]
    fn config_json_names_the_engine_under_either_key() {
        let engine: FileConfig = serde_json::from_str(r#"{"engine":["node","/new/bin/extend-engine.mjs"]}"#).unwrap();
        assert_eq!(engine.engine.unwrap()[1], "/new/bin/extend-engine.mjs");
        let old: FileConfig = serde_json::from_str(r#"{"agent_device":["node","/old/bin/agent-device.mjs"]}"#).unwrap();
        assert_eq!(old.agent_device.unwrap()[1], "/old/bin/agent-device.mjs");

        // Config::load passes `engine` when both are set, else `agent_device`, and the locator
        // returns what the file says (unless EXTEND_ENGINE or EXTEND_AGENT_DEVICE is set in the
        // environment running the tests, which wins).
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(STATE_DIR_NAME);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("config.json"),
            r#"{"engine":["node","/new/bin/extend-engine.mjs"],"agent_device":["node","/old/bin/agent-device.mjs"]}"#,
        )
        .unwrap();
        let c = Config::load(&Overrides {
            home: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        if ENGINE_ENV_VARS.iter().all(|v| std::env::var_os(v).is_none()) {
            assert_eq!(
                c.agent_device.as_deref(),
                Some(&["node".to_string(), "/new/bin/extend-engine.mjs".to_string()][..])
            );
        }
        std::fs::write(
            state.join("config.json"),
            r#"{"agent_device":["node","/old/bin/agent-device.mjs"]}"#,
        )
        .unwrap();
        let c = Config::load(&Overrides {
            home: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        if ENGINE_ENV_VARS.iter().all(|v| std::env::var_os(v).is_none()) {
            assert_eq!(c.agent_device.unwrap()[1], "/old/bin/agent-device.mjs");
        }
    }

    #[test]
    fn old_engine_state_moves_once() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(STATE_DIR_NAME);
        std::fs::create_dir_all(state.join("agent-device/ios-runner")).unwrap();
        std::fs::write(state.join("agent-device/daemon.json"), b"{}").unwrap();
        let c = Config::load(&Overrides {
            home: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(c.agent_device_state_dir(), state.join("engine"));
        assert!(!state.join("agent-device").exists());
        assert!(state.join("engine/ios-runner").is_dir());
        assert_eq!(std::fs::read(state.join("engine/daemon.json")).unwrap(), b"{}");

        // An old directory that shows up again later (a 1.0 app run once more) is left alone: the
        // engine directory is the one in use.
        std::fs::create_dir_all(state.join("agent-device")).unwrap();
        std::fs::write(state.join("engine/marker"), b"kept").unwrap();
        adopt_old_engine_state(&state);
        assert!(state.join("agent-device").is_dir());
        assert_eq!(std::fs::read(state.join("engine/marker")).unwrap(), b"kept");
    }
}
