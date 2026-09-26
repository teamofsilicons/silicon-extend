//! Pieces every hosted driver shares: remote buttons, argument parsing, persisted state, deadlines.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bridge_driver::{Invocation, Output};
use bridge_protocol::ErrorCode;
use bridge_protocol::model::{SetupStep, StepStatus};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Name the TVs show in their "allow this device?" prompts and connection lists.
pub(crate) const CLIENT_NAME: &str = "Silicon Bridge";

/// How long `tv-remote longpress` holds a button when no `--duration-ms` is given
/// (matches agent-device's Android TV preset).
pub(crate) const LONGPRESS_DEFAULT: Duration = Duration::from_millis(500);

/// The remote buttons `bridge tv-remote` accepts (`understanding/cli.yaml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
    Home,
    Menu,
    PlayPause,
    VolumeUp,
    VolumeDown,
    Mute,
    Power,
}

impl Button {
    pub(crate) const ALL: [Button; 13] = [
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::Select,
        Self::Back,
        Self::Home,
        Self::Menu,
        Self::PlayPause,
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
        Self::Power,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Select => "select",
            Self::Back => "back",
            Self::Home => "home",
            Self::Menu => "menu",
            Self::PlayPause => "play-pause",
            Self::VolumeUp => "volume-up",
            Self::VolumeDown => "volume-down",
            Self::Mute => "mute",
            Self::Power => "power",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Button> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        let alias = match s.as_str() {
            "ok" | "enter" | "center" => "select",
            "playpause" | "play" | "pause" => "play-pause",
            "volumeup" | "vol-up" => "volume-up",
            "volumedown" | "vol-down" => "volume-down",
            other => other,
        };
        Self::ALL.into_iter().find(|b| b.as_str() == alias)
    }
}

/// A parsed `tv-remote press|longpress <button> [--duration-ms <ms>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemotePress {
    pub button: Button,
    /// `None` is a plain click; `Some` holds the button for that long.
    pub hold: Option<Duration>,
}

pub(crate) fn parse_tv_remote(args: &[String]) -> Result<RemotePress, String> {
    let mut action: Option<&str> = None;
    let mut button: Option<Button> = None;
    let mut duration: Option<Duration> = None;
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        match a {
            "--duration-ms" | "--duration" => {
                let v = it
                    .next()
                    .ok_or("--duration-ms needs a number of milliseconds")?;
                duration = Some(parse_ms(v)?);
            }
            _ if a.starts_with("--duration-ms=") => {
                duration = Some(parse_ms(&a["--duration-ms=".len()..])?)
            }
            "--json" => {}
            _ if a.starts_with("--") => return Err(format!("tv-remote doesn't take {a}")),
            "press" | "longpress" if action.is_none() => action = Some(a),
            _ if button.is_none() => {
                button = Some(Button::parse(a).ok_or_else(|| {
                    format!(
                        "unknown button \"{a}\"; use one of: {}",
                        Button::ALL.map(Button::as_str).join(", ")
                    )
                })?)
            }
            _ => return Err(format!("unexpected argument \"{a}\"")),
        }
    }
    let button = button.ok_or("usage: tv-remote press|longpress <button> [--duration-ms <ms>]")?;
    let hold = match (action.unwrap_or("press"), duration) {
        (_, Some(d)) if d.is_zero() => None,
        (_, Some(d)) => Some(d),
        ("longpress", None) => Some(LONGPRESS_DEFAULT),
        _ => None,
    };
    Ok(RemotePress { button, hold })
}

fn parse_ms(v: &str) -> Result<Duration, String> {
    let ms: u64 = v
        .parse()
        .map_err(|_| format!("\"{v}\" is not a number of milliseconds"))?;
    if ms > 30_000 {
        return Err("a button can be held for at most 30000 ms".into());
    }
    Ok(Duration::from_millis(ms))
}

/// What `open` was asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenTarget {
    /// An app by name or id.
    App(String),
    /// A link on its own.
    Url(String),
    /// An app, with a link for it to open.
    AppWithUrl(String, String),
}

/// Parses `open <app|url> [url]`. `--surface` is for computers and is refused; `--json` is ignored.
pub(crate) fn parse_open(args: &[String]) -> Result<OpenTarget, String> {
    let mut positionals = Vec::new();
    for a in args {
        match a.as_str() {
            "--json" => {}
            f if f.starts_with("--") => return Err(format!("open doesn't take {f} on a TV")),
            p => positionals.push(p.to_owned()),
        }
    }
    match positionals.as_slice() {
        [one] if is_url_or_scheme(one) => Ok(OpenTarget::Url(one.clone())),
        [one] => Ok(OpenTarget::App(one.clone())),
        [app, url] => Ok(OpenTarget::AppWithUrl(app.clone(), url.clone())),
        [] => Err("usage: open <app|url> [url]".into()),
        _ => Err("open takes an app and at most one link".into()),
    }
}

/// Positional app name for `close [app]`.
pub(crate) fn parse_close(args: &[String]) -> Result<Option<String>, String> {
    let mut app = None;
    for a in args {
        match a.as_str() {
            "--json" => {}
            f if f.starts_with("--") => return Err(format!("close doesn't take {f} on a TV")),
            p if app.is_none() => app = Some(p.to_owned()),
            p => return Err(format!("unexpected argument \"{p}\"")),
        }
    }
    Ok(app)
}

/// `http://…` or `https://…`.
pub(crate) fn is_web_url(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// A URL or an app URL scheme (`youtube://…`, `tel:…`): a letter, then letters, digits, `+`, `-`,
/// `.`, then `:`. Reverse-DNS app ids (`com.apple.TVSettings`) have no colon and aren't URLs.
pub(crate) fn is_url_or_scheme(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    first_ok
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty()
        && !rest.chars().all(|c| c.is_ascii_digit()) // "host:8080" is not a scheme
}

/// Case-insensitive match of an app named by the Silicon against an app list entry.
pub(crate) fn find_app<'a, T>(
    apps: &'a [T],
    wanted: &str,
    id: impl Fn(&T) -> &str,
    name: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    let w = wanted.trim().to_lowercase();
    apps.iter()
        .find(|a| id(a) == wanted)
        .or_else(|| apps.iter().find(|a| id(a).to_lowercase() == w))
        .or_else(|| apps.iter().find(|a| name(a).to_lowercase() == w))
        .or_else(|| {
            let hits: Vec<&T> = apps
                .iter()
                .filter(|a| name(a).to_lowercase().contains(&w))
                .collect();
            if hits.len() == 1 { Some(hits[0]) } else { None }
        })
}

// ───────────── Outputs ─────────────

/// Arguments the driver can't parse. `docs/device-protocol.md` names this code `invalid_args`.
pub(crate) const INVALID_ARGS: &str = "invalid_args";

pub(crate) fn invalid(message: impl Into<String>) -> Output {
    Output::fail(INVALID_ARGS, message)
}

pub(crate) fn failed(message: impl Into<String>) -> Output {
    Output::fail(ErrorCode::CommandFailed.as_str().as_str(), message)
}

pub(crate) fn offline(message: impl Into<String>) -> Output {
    Output::fail(ErrorCode::DeviceOffline.as_str().as_str(), message)
}

pub(crate) fn not_ready(message: impl Into<String>) -> Output {
    Output::fail(ErrorCode::DeviceNotReady.as_str().as_str(), message)
}

pub(crate) fn unsupported(message: impl Into<String>) -> Output {
    Output::fail(ErrorCode::UnsupportedOnDevice.as_str().as_str(), message)
}

pub(crate) fn unsupported_command(command: &str, device: &str) -> Output {
    unsupported(format!("`{command}` isn't available on {device}"))
}

/// Runs a command body within the invocation's deadline, stopping promptly on cancel.
pub(crate) async fn guarded<F: Future<Output = Output>>(inv: &Invocation<'_>, body: F) -> Output {
    let cancel = inv.cancel.clone();
    tokio::select! {
        out = body => out,
        _ = tokio::time::sleep(inv.timeout) => Output::fail(
            ErrorCode::CommandTimeout.as_str().as_str(),
            format!("`{}` didn't finish within {} ms", inv.command, inv.timeout.as_millis()),
        ),
        _ = cancel.cancelled() => failed(format!("`{}` was cancelled", inv.command)),
    }
}

/// Sleeps, returning early (false) when the invocation is cancelled.
pub(crate) async fn sleep_or_cancel(inv: &Invocation<'_>, d: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => true,
        _ = inv.cancel.cancelled() => false,
    }
}

// ───────────── Setup steps ─────────────

pub(crate) fn step(key: &str, title: &str, status: StepStatus) -> SetupStep {
    SetupStep {
        key: key.into(),
        title: title.into(),
        status,
        help: None,
        error: None,
        input: None,
    }
}

pub(crate) fn step_help(key: &str, title: &str, status: StepStatus, help: &str) -> SetupStep {
    SetupStep {
        key: key.into(),
        title: title.into(),
        status,
        help: Some(help.into()),
        error: None,
        input: None,
    }
}

pub(crate) fn step_error(
    key: &str,
    title: &str,
    status: StepStatus,
    help: Option<&str>,
    error: impl Into<String>,
) -> SetupStep {
    SetupStep {
        key: key.into(),
        title: title.into(),
        status,
        help: help.map(Into::into),
        error: Some(error.into()),
        input: None,
    }
}

// ───────────── Persisted state ─────────────

/// Reads `dir/file` as JSON; a missing or unreadable file is `None`.
pub(crate) fn load_json<T: DeserializeOwned>(dir: &Path, file: &str) -> Option<T> {
    let bytes = std::fs::read(dir.join(file)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(file, error = %e, "ignoring unreadable hosted-device state");
            None
        }
    }
}

/// Writes `dir/file` atomically (temp file, then rename), readable only by this user on Unix.
pub(crate) fn save_json<T: Serialize>(dir: &Path, file: &str, value: &T) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{file}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(file)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Finds the file an argument refers to: an existing path, else an attachment with that file name,
/// else the only attachment when there is exactly one.
pub(crate) fn resolve_attachment(arg: &str, attachments: &[PathBuf]) -> Option<PathBuf> {
    // The agent normally swaps `attachment:<name>` for the local path already (device-protocol.md,
    // "Attachments"); accept the unswapped form too.
    let arg = arg.strip_prefix("attachment:").unwrap_or(arg);
    let p = Path::new(arg);
    if p.is_absolute() && p.exists() {
        return Some(p.to_path_buf());
    }
    let wanted = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| arg.to_owned());
    attachments
        .iter()
        .find(|a| a.file_name().is_some_and(|n| n.to_string_lossy() == wanted))
        .cloned()
        .or_else(|| {
            if attachments.len() == 1 {
                Some(attachments[0].clone())
            } else {
                None
            }
        })
}

/// Content type from a file extension, for files handed back to the agent.
pub(crate) fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("heic") => "image/heic",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("m4v") => "video/x-m4v",
        Some("json") => "application/json",
        Some("txt" | "log" | "ad") => "text/plain",
        Some("yaml" | "yml") => "application/yaml",
        _ => "application/octet-stream",
    }
}

/// Splits `host:port` (IPv4, name, or `[v6]:port`) into its parts; a bare host has no port.
pub(crate) fn split_host_port(addr: &str) -> (String, Option<u16>) {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return (host.to_owned(), port);
    }
    match addr.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(port) => (h.to_owned(), Some(port)),
            Err(_) => (addr.to_owned(), None),
        },
        _ => (addr.to_owned(), None),
    }
}

/// Formats a host for a URL authority (brackets around IPv6).
pub(crate) fn url_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn remote_parsing() {
        assert_eq!(
            parse_tv_remote(&s(&["press", "up"])).unwrap(),
            RemotePress {
                button: Button::Up,
                hold: None
            }
        );
        assert_eq!(
            parse_tv_remote(&s(&["longpress", "select"])).unwrap(),
            RemotePress {
                button: Button::Select,
                hold: Some(LONGPRESS_DEFAULT)
            }
        );
        assert_eq!(
            parse_tv_remote(&s(&["press", "select", "--duration-ms", "900"]))
                .unwrap()
                .hold,
            Some(Duration::from_millis(900))
        );
        assert_eq!(
            parse_tv_remote(&s(&["press", "Play-Pause"]))
                .unwrap()
                .button,
            Button::PlayPause
        );
        assert_eq!(
            parse_tv_remote(&s(&["press", "volume_up"])).unwrap().button,
            Button::VolumeUp
        );
        assert!(
            parse_tv_remote(&s(&["press", "rewind"]))
                .unwrap_err()
                .contains("unknown button")
        );
        assert!(parse_tv_remote(&s(&["press"])).is_err());
        assert!(parse_tv_remote(&s(&["press", "up", "--platform", "ios"])).is_err());
        for b in Button::ALL {
            assert_eq!(Button::parse(b.as_str()), Some(b));
        }
    }

    #[test]
    fn open_parsing() {
        assert_eq!(
            parse_open(&s(&["YouTube"])).unwrap(),
            OpenTarget::App("YouTube".into())
        );
        assert_eq!(
            parse_open(&s(&["https://example.com"])).unwrap(),
            OpenTarget::Url("https://example.com".into())
        );
        assert_eq!(
            parse_open(&s(&["com.google.ios.youtube", "youtube://watch?v=1"])).unwrap(),
            OpenTarget::AppWithUrl(
                "com.google.ios.youtube".into(),
                "youtube://watch?v=1".into()
            )
        );
        assert!(parse_open(&s(&["Finder", "--surface", "app"])).is_err());
        assert!(parse_open(&[]).is_err());
    }

    #[test]
    fn url_detection() {
        assert!(is_url_or_scheme("https://a.b"));
        assert!(is_url_or_scheme("youtube://x"));
        assert!(is_url_or_scheme("tel:+15551234567"));
        assert!(!is_url_or_scheme("com.apple.TVSettings"));
        assert!(!is_url_or_scheme("111299001912"));
        assert!(!is_url_or_scheme("tv.local:8080"));
        assert!(is_web_url("HTTP://x"));
        assert!(!is_web_url("youtube://x"));
    }

    #[test]
    fn app_matching() {
        let apps = vec![
            ("111299001912", "YouTube"),
            ("3201907018807", "Netflix"),
            ("org.tizen.browser", "Internet"),
        ];
        let f = |w: &str| find_app(&apps, w, |a| a.0, |a| a.1).map(|a| a.0);
        assert_eq!(f("youtube"), Some("111299001912"));
        assert_eq!(f("3201907018807"), Some("3201907018807"));
        assert_eq!(f("netf"), Some("3201907018807"));
        assert_eq!(f("Disney+"), None);
    }

    #[test]
    fn host_port() {
        assert_eq!(split_host_port("192.168.1.2"), ("192.168.1.2".into(), None));
        assert_eq!(
            split_host_port("192.168.1.2:49153"),
            ("192.168.1.2".into(), Some(49153))
        );
        assert_eq!(
            split_host_port("[fe80::1]:7000"),
            ("fe80::1".into(), Some(7000))
        );
        assert_eq!(split_host_port("fe80::1"), ("fe80::1".into(), None));
        assert_eq!(url_host("fe80::1"), "[fe80::1]");
    }

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        save_json(dir.path(), "x.json", &serde_json::json!({"token": "1"})).unwrap();
        let v: serde_json::Value = load_json(dir.path(), "x.json").unwrap();
        assert_eq!(v["token"], "1");
        assert!(load_json::<serde_json::Value>(dir.path(), "missing.json").is_none());
    }
}
