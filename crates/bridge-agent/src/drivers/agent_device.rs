//! Runs Bridge's fork of agent-device (`vendor/agent-device`) for Mac and Linux.
//!
//! Every command becomes one CLI run:
//!
//! ```text
//! node …/bin/agent-device.mjs <command> <args…> --platform macos|linux --session bridge-<session_id> --json
//! ```
//!
//! with `AGENT_DEVICE_STATE_DIR` pointing at the agent's own state, so agent-device's daemon and
//! sessions are private to Bridge, and `AGENT_DEVICE_JSON_TEXT=1` (a fork addition) so one run
//! gives both the JSON result and the text the CLI would print.
//!
//! Files: output paths are always chosen by the agent, inside the command's work directory, and
//! the files found there are handed back for upload. Input files (replay scripts, baselines,
//! step files) must come as attachments; a path on the Silicon's machine means nothing here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bridge_driver::cancel::CancelToken;
use bridge_driver::{Driver, Invocation, LocalFile, Output, Probe};
use bridge_protocol::model::{CommandError, FileKind};
use tokio::io::AsyncReadExt as _;

use crate::drivers::args::{content_type_for, parse, safe_file_name, strip_agent_added, with_extension};

/// agent-device flags that take a value (from its flag registry), so positionals can be told apart.
pub const VALUE_FLAGS: &[&str] = &[
    "--activity", "--app", "--artifact", "--artifacts-dir", "--baseline", "--button", "--count", "--delay-ms",
    "--depth", "--duration-ms", "--env", "--fps", "--from", "--header", "--hold-ms", "--include", "--interval-ms",
    "--jitter-px", "--keyframes", "--kind", "--level", "--max-steps", "--on-error", "--out", "--pattern", "--pause-ms",
    "--pixels", "--plan-digest", "--pointer-count", "--quality", "--record-as", "--report-junit", "--retention-ms",
    "--retries", "--scope", "--settle-quiet", "--steps", "--steps-file", "--surface", "--target-app", "--template",
    "--threshold", "--timeout", "--until", "--wait", "--scale", "--crop-on", "-d", "-s", "-e", "-b",
];

/// What the driver remembers about one Bridge session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFacts {
    /// Where `record start` is writing.
    pub recording: Option<PathBuf>,
    /// Where `open --save-script` asked for the script to go on `close`.
    pub armed_script: Option<PathBuf>,
    /// `logs start` ran and `logs stop` hasn't.
    pub logs_running: bool,
    /// macOS without UI Automation: agent-device's app sessions go through its XCTest runner, which
    /// blocks on the "Enable UI Automation" prompt. The session instead follows the frontmost app
    /// through agent-device's macOS helper (`--surface frontmost-app`), which needs only
    /// Accessibility and Screen Recording.
    pub helper_surface: bool,
}

/// Whether agent-device's XCTest runner can drive macOS apps without a prompt.
fn macos_runner_ready() -> bool {
    #[cfg(target_os = "macos")]
    {
        let f = super::probe_macos::gather();
        matches!(f.automation, super::probe_macos::AutomationMode::Enabled | super::probe_macos::AutomationMode::NoAuthentication) && f.xcode
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The app or link an `open` names, when it names one and picks no surface itself.
fn open_target(args: &[String]) -> Option<String> {
    if args.iter().any(|a| a.starts_with("--surface")) {
        return None;
    }
    positional_indices(args).first().map(|&index| args[index].clone())
}

/// A file the plan expects the command to produce.
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
}

/// Follow-up work once the command succeeded.
#[derive(Debug, Clone, PartialEq)]
pub enum After {
    Nothing,
    RecordingStarted(PathBuf),
    RecordingStopped,
    LogsStarted,
    LogsStopped,
    ScriptArmed(PathBuf),
    SessionClosed,
}

/// How to run one command.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Command name and rewritten arguments (the agent appends platform, session and `--json`).
    pub argv: Vec<String>,
    pub expect: Vec<Expected>,
    /// Every regular file under this directory is returned too (test suite artifacts).
    pub expect_dir: Option<(PathBuf, FileKind)>,
    pub after: After,
}

pub struct PlanContext<'a> {
    pub workdir: &'a Path,
    /// Per-session directory for files that outlive one command.
    pub session_dir: &'a Path,
    pub attachments: &'a [PathBuf],
    pub facts: &'a SessionFacts,
}

fn invalid(message: impl Into<String>) -> Output {
    Output::fail("invalid_args", message)
}

/// Finds an attachment by the name the Silicon used (its last path component).
fn attachment<'a>(ctx: &'a PlanContext<'_>, wanted: &str) -> Option<&'a PathBuf> {
    let want = safe_file_name(wanted, "");
    ctx.attachments
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(want.as_str()))
        .or_else(|| {
            // The dispatcher prefixes duplicate names with their index ("2-flow.ad").
            ctx.attachments.iter().find(|p| {
                p.file_name().and_then(|n| n.to_str()).and_then(|n| n.split_once('-')).is_some_and(|(_, rest)| rest == want)
            })
        })
}

fn missing_attachment(what: &str, name: &str) -> Output {
    invalid(format!(
        "{what} {name:?} wasn't sent with the command. Files are read on the Silicon's side and sent along; a path on the Silicon's machine doesn't exist on this computer."
    ))
}

/// Replaces the value of `flag` (in either `--flag v` or `--flag=v` form) with `value`, or appends it.
fn set_flag(argv: &mut Vec<String>, flag: &str, value: &str) {
    let mut i = 0;
    let mut found = false;
    while i < argv.len() {
        if argv[i] == flag {
            if i + 1 < argv.len() {
                argv[i + 1] = value.to_owned();
            } else {
                argv.push(value.to_owned());
            }
            found = true;
            i += 2;
            continue;
        }
        if argv[i].starts_with(&format!("{flag}=")) {
            argv[i] = format!("{flag}={value}");
            found = true;
        }
        i += 1;
    }
    if !found {
        argv.push(flag.to_owned());
        argv.push(value.to_owned());
    }
}

/// Indices of positional tokens in `args` (so they can be replaced in place).
fn positional_indices(args: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut after_separator = false;
    while i < args.len() {
        let t = &args[i];
        if after_separator {
            out.push(i);
        } else if t == "--" {
            after_separator = true;
        } else if let Some(name) = crate::drivers::args::flag_name(t) {
            let takes_value = !t.contains('=') && VALUE_FLAGS.contains(&name);
            let takes_path = name == "--save-script" && args.get(i + 1).is_some_and(|n| looks_like_path(n));
            if takes_value || takes_path {
                i += 1;
            }
        } else {
            out.push(i);
        }
        i += 1;
    }
    out
}

/// agent-device's rule for whether `--save-script` consumes the next token.
fn looks_like_path(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty() && !t.contains("://") && (t.starts_with('/') || t.starts_with("./") || t.starts_with("../") || t.starts_with("~/") || t.contains('/') || t.contains('\\'))
}

/// Removes `--save-script` (and a path value it consumed) and returns the value, if any.
fn take_save_script(argv: &mut Vec<String>) -> Option<Option<String>> {
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--save-script" {
            argv.remove(i);
            if i < argv.len() && looks_like_path(&argv[i]) {
                return Some(Some(argv.remove(i)));
            }
            return Some(None);
        }
        if let Some(v) = argv[i].strip_prefix("--save-script=") {
            let v = v.to_owned();
            argv.remove(i);
            return Some(Some(v));
        }
        i += 1;
    }
    None
}

/// Builds the run for one command. Pure: no processes, no file system.
#[allow(clippy::result_large_err)] // The error is the command's answer, returned as-is.
pub fn plan(command: &str, args: &[String], ctx: &PlanContext<'_>) -> Result<Plan, Output> {
    let mut rest = strip_agent_added(args);
    let parsed = parse(&rest, VALUE_FLAGS);
    let mut expect = Vec::new();
    let mut expect_dir = None;
    let mut after = After::Nothing;
    let pos = positional_indices(&rest);

    match command {
        "screenshot" => {
            let name = parsed.positional(0).map(|n| with_extension(&safe_file_name(n, "screenshot.png"), &[".png"]));
            let name = name.unwrap_or_else(|| "screenshot.png".into());
            let path = ctx.workdir.join(&name);
            match pos.first() {
                Some(&i) => rest[i] = path.display().to_string(),
                None => rest.insert(0, path.display().to_string()),
            }
            expect.push(Expected { path, name, kind: FileKind::Screenshot });
        }
        "diff" => {
            if parsed.positional(0) == Some("screenshot") {
                let Some(baseline) = parsed.value("--baseline") else {
                    return Err(invalid("diff screenshot needs --baseline <file>."));
                };
                let Some(b) = attachment(ctx, baseline) else { return Err(missing_attachment("The baseline", baseline)) };
                set_flag(&mut rest, "--baseline", &b.display().to_string());
                if let (Some(current), Some(&i)) = (parsed.positional(1), pos.get(1)) {
                    let Some(c) = attachment(ctx, current) else { return Err(missing_attachment("The screenshot", current)) };
                    rest[i] = c.display().to_string();
                }
                let name = with_extension(&safe_file_name(parsed.value("--out").unwrap_or("diff.png"), "diff.png"), &[".png"]);
                let path = ctx.workdir.join(&name);
                set_flag(&mut rest, "--out", &path.display().to_string());
                expect.push(Expected { path, name, kind: FileKind::Diff });
            }
        }
        "record" => match parsed.positional(0) {
            Some("start") => {
                if ctx.facts.recording.is_some() {
                    return Err(Output::fail("recording_in_progress", "A recording is already running in this session. Run `record stop` first."));
                }
                let name = parsed
                    .positional(1)
                    .map(|n| with_extension(&safe_file_name(n, "recording.mp4"), &[".mp4", ".mov", ".webm"]))
                    .unwrap_or_else(|| "recording.mp4".into());
                let path = ctx.session_dir.join("recordings").join(&name);
                match pos.get(1) {
                    Some(&i) => rest[i] = path.display().to_string(),
                    None => {
                        let at = pos.first().map_or(rest.len(), |&i| i + 1);
                        rest.insert(at, path.display().to_string());
                    }
                }
                after = After::RecordingStarted(path);
            }
            Some("stop") => after = After::RecordingStopped,
            _ => {}
        },
        "logs" => match parsed.positional(0) {
            Some("start") => after = After::LogsStarted,
            Some("stop") => after = After::LogsStopped,
            _ => {}
        },
        "open" => {
            if let Some(value) = take_save_script(&mut rest) {
                let name = value.as_deref().map(|v| with_extension(&safe_file_name(v, "session.ad"), &[".ad"])).unwrap_or_else(|| "session.ad".into());
                let path = ctx.session_dir.join("scripts").join(name);
                rest.push(format!("--save-script={}", path.display()));
                after = After::ScriptArmed(path);
            }
        }
        "close" => {
            if let Some(value) = take_save_script(&mut rest) {
                let name = value.as_deref().map(|v| with_extension(&safe_file_name(v, "session.ad"), &[".ad"])).unwrap_or_else(|| "session.ad".into());
                let path = ctx.workdir.join(&name);
                rest.push(format!("--save-script={}", path.display()));
                expect.push(Expected { path, name, kind: FileKind::ReplayScript });
            } else if let Some(armed) = &ctx.facts.armed_script {
                let name = armed.file_name().and_then(|n| n.to_str()).unwrap_or("session.ad").to_owned();
                expect.push(Expected { path: armed.clone(), name, kind: FileKind::ReplayScript });
            }
            after = After::SessionClosed;
        }
        "replay" | "test" => {
            if pos.is_empty() {
                return Err(invalid(format!("{command} needs a script (.ad) sent with the command.")));
            }
            for &i in &pos {
                let wanted = rest[i].clone();
                let Some(a) = attachment(ctx, &wanted) else { return Err(missing_attachment("The script", &wanted)) };
                rest[i] = a.display().to_string();
            }
            if command == "test" {
                let dir = ctx.workdir.join("test-artifacts");
                set_flag(&mut rest, "--artifacts-dir", &dir.display().to_string());
                expect_dir = Some((dir, FileKind::Other));
                if let Some(j) = parsed.value("--report-junit") {
                    let name = with_extension(&safe_file_name(j, "junit.xml"), &[".xml"]);
                    let path = ctx.workdir.join(&name);
                    set_flag(&mut rest, "--report-junit", &path.display().to_string());
                    expect.push(Expected { path, name, kind: FileKind::Log });
                }
            }
        }
        "batch" => {
            if let Some(file) = parsed.value("--steps-file") {
                let Some(a) = attachment(ctx, file) else { return Err(missing_attachment("The steps file", file)) };
                set_flag(&mut rest, "--steps-file", &a.display().to_string());
            }
        }
        "install" | "reinstall" => {
            if let (Some(file), Some(&i)) = (parsed.positional(1), pos.get(1)) {
                let Some(a) = attachment(ctx, file) else { return Err(missing_attachment("The app file", file)) };
                rest[i] = a.display().to_string();
            }
        }
        _ => {}
    }
    // Anything else asked to write somewhere goes to the work directory and comes back as a file.
    if command != "diff" && let Some(out) = parsed.value("--out") {
        let name = safe_file_name(out, "output");
        let path = ctx.workdir.join(&name);
        set_flag(&mut rest, "--out", &path.display().to_string());
        expect.push(Expected { path, name, kind: FileKind::Other });
    }

    let mut argv = Vec::with_capacity(rest.len() + 1);
    argv.push(command.to_owned());
    argv.extend(rest);
    Ok(Plan { argv, expect, expect_dir, after })
}

/// agent-device's error codes, as Bridge reports them.
pub fn map_error_code(code: &str) -> String {
    match code {
        "INVALID_ARGS" => "invalid_args".into(),
        "UNSUPPORTED_OPERATION" | "UNSUPPORTED_PLATFORM" | "NOT_SUPPORTED" | "UNSUPPORTED_COMMAND" => "unsupported_on_device".into(),
        "" => "command_failed".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// Reads agent-device's `--json` document (`{"success":…,"data"|"error":…,"text"?}`) into an Output.
pub fn parse_result(stdout: &str, stderr: &str, exit_ok: bool) -> Output {
    let doc = find_json(stdout);
    let Some(doc) = doc else {
        let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        let tail: String = detail.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        let message = if exit_ok {
            "agent-device finished without a result".to_string()
        } else {
            format!("agent-device failed without a result: {tail}")
        };
        return Output::fail("command_failed", message);
    };
    let success = doc.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
    let text = doc.get("text").and_then(|t| t.as_str()).map(|t| t.trim_end().to_owned());
    if success {
        let data = doc.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let text = text
            .or_else(|| data.get("message").and_then(|m| m.as_str()).map(str::to_owned))
            .or_else(|| summarize(&data));
        return Output { ok: true, output: data, text, error: None, files: vec![] };
    }
    let err = doc.get("error").cloned().unwrap_or(serde_json::Value::Null);
    let raw_code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
    let message = err.get("message").and_then(|m| m.as_str()).unwrap_or("agent-device reported an error").to_owned();
    let hint = err.get("hint").and_then(|h| h.as_str()).map(str::to_owned);
    let mut details = serde_json::Map::new();
    details.insert("agent_device_code".into(), raw_code.into());
    if let Some(h) = &hint {
        details.insert("hint".into(), h.clone().into());
    }
    if let Some(d) = err.get("details").filter(|d| !d.is_null()) {
        details.insert("agent_device".into(), d.clone());
    }
    let text = match &hint {
        Some(h) => format!("{message}\nHint: {h}"),
        None => message.clone(),
    };
    Output {
        ok: false,
        output: serde_json::Value::Null,
        text: Some(text),
        error: Some(CommandError { code: map_error_code(raw_code), message, details: serde_json::Value::Object(details) }),
        files: vec![],
    }
}

/// Drops what only makes sense on this computer from the text a Silicon reads: agent-device's
/// `Session state:` line, and the work directory in file paths (files arrive as Briefcase links).
pub fn tidy_text(out: &mut Output, workdir: &Path) {
    let Some(text) = out.text.take() else { return };
    let dir = format!("{}{}", workdir.display(), std::path::MAIN_SEPARATOR);
    let cleaned: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with("Session state: "))
        .map(|l| l.replace(&dir, ""))
        .collect();
    out.text = Some(cleaned.join("\n"));
}

/// `key: value` lines for a result agent-device prints no text for (`appstate`).
fn summarize(data: &serde_json::Value) -> Option<String> {
    let obj = data.as_object()?;
    let lines: Vec<String> = obj
        .iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) => Some(format!("{k}: {s}")),
            serde_json::Value::Number(n) => Some(format!("{k}: {n}")),
            serde_json::Value::Bool(b) => Some(format!("{k}: {b}")),
            _ => None,
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The JSON document in agent-device's stdout (it may be preceded by progress lines).
fn find_json(stdout: &str) -> Option<serde_json::Value> {
    let trimmed = stdout.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return v.is_object().then_some(v);
    }
    let mut offset = 0;
    for line in stdout.split_inclusive('\n') {
        if line.starts_with('{')
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout[offset..].trim())
            && v.is_object()
        {
            return Some(v);
        }
        offset += line.len();
    }
    None
}

/// A file path agent-device reported in its result (`path`, `outputPath`, …) that exists.
pub fn reported_path(data: &serde_json::Value) -> Option<PathBuf> {
    for key in ["outputPath", "path", "videoPath", "output", "file", "logPath"] {
        if let Some(p) = data.get(key).and_then(|v| v.as_str()) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// What a platform probe gets to work with.
pub struct ProbeInput<'a> {
    /// Why agent-device can't run here, when it can't.
    pub problem: Option<&'a str>,
    /// The commands agent-device says this platform supports (`capabilities --json`), when known.
    pub commands: Option<&'a [String]>,
}

impl ProbeInput<'_> {
    /// True when agent-device supports `name` here (or when that isn't known).
    pub fn supports(&self, name: &str) -> bool {
        self.commands.is_none_or(|c| c.iter().any(|x| x == name))
    }
}

/// What the platform can do right now; supplied per OS.
pub type Prober = std::sync::Arc<dyn Fn(&ProbeInput<'_>) -> Probe + Send + Sync>;

pub struct AgentDeviceDriver {
    command: Option<Vec<String>>,
    problem: Option<String>,
    platform: &'static str,
    state_dir: PathBuf,
    data_dir: PathBuf,
    sessions: Mutex<HashMap<String, SessionFacts>>,
    version: tokio::sync::OnceCell<Option<String>>,
    commands: tokio::sync::OnceCell<Option<Vec<String>>>,
    prober: Prober,
}

impl AgentDeviceDriver {
    pub fn new(
        command: Option<Vec<String>>,
        problem: Option<String>,
        platform: &'static str,
        state_dir: PathBuf,
        data_dir: PathBuf,
        prober: Prober,
    ) -> Self {
        Self { command, problem, platform, state_dir, data_dir, sessions: Mutex::new(HashMap::new()), version: tokio::sync::OnceCell::new(), commands: tokio::sync::OnceCell::new(), prober }
    }

    pub fn available(&self) -> bool {
        self.command.is_some()
    }

    fn agent_session(session_id: &str) -> String {
        format!("bridge-{session_id}")
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join(safe_file_name(session_id, "session"))
    }

    fn facts(&self, session_id: &str) -> SessionFacts {
        self.sessions.lock().unwrap().get(session_id).cloned().unwrap_or_default()
    }

    fn update_facts(&self, session_id: &str, f: impl FnOnce(&mut SessionFacts)) {
        let mut map = self.sessions.lock().unwrap();
        f(map.entry(session_id.to_owned()).or_default());
    }

    /// agent-device's version (`--version`), read once.
    pub async fn agent_device_version(&self) -> Option<String> {
        self.version
            .get_or_init(|| async {
                let cmd = self.command.as_ref()?;
                let out = run_process(cmd, &["--version".to_string()], &self.state_dir, None, Duration::from_secs(20), &CancelToken::new()).await.ok()?;
                let v = out.stdout.trim().lines().last()?.trim().to_owned();
                (!v.is_empty() && v.len() < 32).then_some(v)
            })
            .await
            .clone()
    }

    /// The commands agent-device supports on this platform (`capabilities --json`), read once.
    pub async fn platform_commands(&self) -> Option<Vec<String>> {
        self.commands
            .get_or_init(|| async {
                let cmd = self.command.as_ref()?;
                let args: Vec<String> = ["capabilities", "--platform", self.platform, "--json"].map(String::from).to_vec();
                let out = run_process(cmd, &args, &self.state_dir, Some(&self.state_dir), Duration::from_secs(60), &CancelToken::new()).await.ok()?;
                let parsed = parse_result(&out.stdout, &out.stderr, out.success);
                let list = parsed.output.get("availableCommands")?.as_array()?;
                Some(list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            })
            .await
            .clone()
    }

    /// Runs agent-device once with the session and platform set.
    async fn invoke(&self, session_id: &str, argv: &[String], cwd: &Path, timeout: Duration, cancel: &CancelToken) -> Output {
        let Some(cmd) = &self.command else {
            return Output::fail("unsupported_on_device", self.problem.clone().unwrap_or_else(|| "agent-device isn't available on this computer".into()));
        };
        let mut full = argv.to_vec();
        full.extend(["--platform".into(), self.platform.into(), "--session".into(), Self::agent_session(session_id), "--json".into()]);
        match run_process(cmd, &full, &self.state_dir, Some(cwd), timeout, cancel).await {
            Ok(p) => parse_result(&p.stdout, &p.stderr, p.success),
            Err(ProcessError::Timeout) => Output::fail("command_timeout", format!("agent-device didn't answer within {} ms and was stopped.", timeout.as_millis())),
            Err(ProcessError::Cancelled) => Output::fail("cancelled", "Bridge cancelled this command."),
            Err(ProcessError::Spawn(e)) => Output::fail("unsupported_on_device", format!("couldn't start agent-device ({}): {e}", cmd.join(" "))),
        }
    }

    async fn collect(&self, plan: &Plan, out: &mut Output) {
        for e in &plan.expect {
            if tokio::fs::metadata(&e.path).await.is_ok_and(|m| m.is_file()) {
                out.files.push(LocalFile { path: e.path.clone(), name: e.name.clone(), content_type: content_type_for(&e.name).into(), kind: e.kind });
            }
        }
        if let Some((dir, kind)) = &plan.expect_dir {
            for path in walk_files(dir) {
                let name = path.strip_prefix(dir).map(|p| p.to_string_lossy().replace(['/', '\\'], "_")).unwrap_or_default();
                let name = safe_file_name(&name, "artifact");
                out.files.push(LocalFile { content_type: content_type_for(&name).into(), path, name, kind: *kind });
            }
        }
    }

    /// Copies the session's app log into the work directory after `logs stop`.
    async fn collect_logs(&self, session_id: &str, workdir: &Path, out: &mut Output) {
        let path = self.invoke(session_id, &["logs".into(), "path".into()], workdir, Duration::from_secs(20), &CancelToken::new()).await;
        let Some(src) = path.output.get("path").and_then(|p| p.as_str()).map(PathBuf::from).or_else(|| reported_path(&path.output)) else {
            return;
        };
        let dst = workdir.join("app.log");
        if tokio::fs::copy(&src, &dst).await.is_ok() {
            out.files.push(LocalFile { path: dst, name: "app.log".into(), content_type: "text/plain".into(), kind: FileKind::Log });
        }
    }
}

#[async_trait]
impl Driver for AgentDeviceDriver {
    async fn probe(&self) -> Probe {
        let (version, commands) = if self.available() {
            let _ = tokio::fs::create_dir_all(&self.state_dir).await;
            (self.agent_device_version().await, self.platform_commands().await)
        } else {
            (None, None)
        };
        let problem = (!self.available()).then(|| self.problem.clone().unwrap_or_else(|| "agent-device isn't available".into()));
        let prober = self.prober.clone();
        // Platform checks run helper programs; keep them off the async threads.
        let probed = tokio::task::spawn_blocking(move || {
            prober(&ProbeInput { problem: problem.as_deref(), commands: commands.as_deref() })
        })
        .await;
        let mut probe = match probed {
            Ok(p) => p,
            Err(e) => return fallback_probe(self.platform, &format!("the device check crashed: {e}")),
        };
        probe.agent_device_version = version;
        probe
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        let facts = self.facts(inv.session_id);
        if facts.helper_surface && inv.command == "open"
            && let Some(target) = open_target(inv.args).filter(|target| target.contains("://")) {
                // Links have no explicit app identity; named apps use the bound native app surface.
                let mut launch = std::process::Command::new("/usr/bin/open");
                launch.arg(&target);
                match launch.output() {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => {
                        return Output::fail("app_not_found", format!("Couldn't open {target}: {}", String::from_utf8_lossy(&o.stderr).trim()));
                    }
                    Err(e) => return Output::fail("app_not_found", format!("Couldn't open {target}: {e}")),
                }
                tokio::time::sleep(Duration::from_millis(1200)).await;
                let argv = ["open".to_owned(), "--surface".into(), "frontmost-app".into()];
                let mut out = self.invoke(inv.session_id, &argv, inv.workdir, inv.timeout, &inv.cancel).await;
                tidy_text(&mut out, inv.workdir);
                return out;
            }
        let session_dir = self.session_dir(inv.session_id);
        let ctx = PlanContext { workdir: inv.workdir, session_dir: &session_dir, attachments: inv.attachments, facts: &facts };
        let plan = match plan(inv.command, inv.args, &ctx) {
            Ok(p) => p,
            Err(out) => return out,
        };
        for dir in [inv.workdir.to_path_buf(), session_dir.join("recordings"), session_dir.join("scripts")] {
            let _ = tokio::fs::create_dir_all(&dir).await;
        }
        let mut out = self.invoke(inv.session_id, &plan.argv, inv.workdir, inv.timeout, &inv.cancel).await;
        tidy_text(&mut out, inv.workdir);
        if !out.ok {
            // A failed `test` still leaves useful artifacts.
            self.collect(&plan, &mut out).await;
            return out;
        }
        self.collect(&plan, &mut out).await;
        match &plan.after {
            After::Nothing => {}
            After::RecordingStarted(path) => self.update_facts(inv.session_id, |f| f.recording = Some(path.clone())),
            After::RecordingStopped => {
                let remembered = self.facts(inv.session_id).recording;
                self.update_facts(inv.session_id, |f| f.recording = None);
                let found = reported_path(&out.output).or(remembered.filter(|p| p.is_file()));
                if let Some(src) = found {
                    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("recording.mp4").to_owned();
                    let dst = inv.workdir.join(&name);
                    if tokio::fs::rename(&src, &dst).await.is_ok() || tokio::fs::copy(&src, &dst).await.is_ok() {
                        out.files.push(LocalFile { path: dst, content_type: content_type_for(&name).into(), name, kind: FileKind::Recording });
                    }
                }
            }
            After::LogsStarted => self.update_facts(inv.session_id, |f| f.logs_running = true),
            After::LogsStopped => {
                self.update_facts(inv.session_id, |f| f.logs_running = false);
                self.collect_logs(inv.session_id, inv.workdir, &mut out).await;
            }
            After::ScriptArmed(path) => self.update_facts(inv.session_id, |f| f.armed_script = Some(path.clone())),
            After::SessionClosed => self.update_facts(inv.session_id, |f| f.armed_script = None),
        }
        out
    }

    async fn session_started(&self, session_id: &str) {
        let helper_surface = self.platform == "macos" && !tokio::task::spawn_blocking(macos_runner_ready).await.unwrap_or(false);
        self.sessions.lock().unwrap().insert(session_id.to_owned(), SessionFacts { helper_surface, ..SessionFacts::default() });
        if helper_surface && self.available() {
            // Start on whatever app is in front, so `snapshot` works before any `open`.
            let dir = self.session_dir(session_id);
            let _ = tokio::fs::create_dir_all(&dir).await;
            let argv = ["open".to_owned(), "--surface".into(), "frontmost-app".into()];
            let _ = self.invoke(session_id, &argv, &dir, Duration::from_secs(20), &CancelToken::new()).await;
        }
    }

    async fn session_ended(&self, session_id: &str) {
        let facts = self.facts(session_id);
        let dir = self.session_dir(session_id);
        let cancel = CancelToken::new();
        let t = Duration::from_secs(30);
        if self.available() {
            if facts.recording.is_some() {
                let _ = self.invoke(session_id, &["record".into(), "stop".into()], &dir, t, &cancel).await;
            }
            if facts.logs_running {
                let _ = self.invoke(session_id, &["logs".into(), "stop".into()], &dir, t, &cancel).await;
            }
            let mut closed = self.invoke(session_id, &["close".into()], &dir, t, &cancel).await;
            if closed.error.as_ref().is_some_and(|e| e.details["agent_device"]["reason"] == "session_cleanup_incomplete") {
                // Failed recording export may still dispose the native recorder. A second close
                // confirms the remaining cleanup and releases the retained claim.
                closed = self.invoke(session_id, &["close".into()], &dir, t, &cancel).await;
            }
            if !closed.ok {
                tracing::warn!(session_id, error = ?closed.error, "session cleanup remains incomplete; retaining its state and artifacts");
                return;
            }
        }
        self.sessions.lock().unwrap().remove(session_id);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}

/// A probe that reports nothing works, with why.
fn fallback_probe(platform: &str, why: &str) -> Probe {
    use bridge_protocol::model::{MissingCapability, Setup};
    let os = if platform == "macos" { bridge_protocol::DeviceOs::Macos } else { bridge_protocol::DeviceOs::Linux };
    Probe {
        os,
        os_version: crate::sysinfo::os_version(),
        model: crate::sysinfo::model(),
        capabilities: vec![],
        missing: os
            .full_capabilities()
            .iter()
            .filter(|c| **c != bridge_protocol::Capability::Terminal)
            .map(|c| MissingCapability { capability: *c, reason: why.to_owned() })
            .collect(),
        setup: Setup::complete(),
        agent_device_version: None,
        online: true,
    }
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

#[derive(Debug)]
pub struct ProcessOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub enum ProcessError {
    Spawn(std::io::Error),
    Timeout,
    Cancelled,
}

/// Runs `cmd + args` with agent-device's environment, killing it (and its children) on timeout or
/// cancel. agent-device's daemon detaches itself, so it survives and keeps the session warm.
pub async fn run_process(
    cmd: &[String],
    args: &[String],
    state_dir: &Path,
    cwd: Option<&Path>,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<ProcessOutput, ProcessError> {
    let mut c = tokio::process::Command::new(&cmd[0]);
    c.args(&cmd[1..])
        .args(args)
        .env("AGENT_DEVICE_STATE_DIR", state_dir)
        .env("AGENT_DEVICE_NO_UPDATE_NOTIFIER", "1")
        .env("AGENT_DEVICE_JSON_TEXT", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    if let Some(helper) = bundled_macos_helper() {
        c.env("AGENT_DEVICE_MACOS_HELPER_BIN", helper);
    }
    #[cfg(unix)]
    c.process_group(0);
    let mut child = c.spawn().map_err(ProcessError::Spawn)?;
    let pid = child.id();
    let mut so = child.stdout.take().expect("stdout");
    let mut se = child.stderr.take().expect("stderr");
    let read_out = tokio::spawn(async move {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s).await;
        s
    });
    let read_err = tokio::spawn(async move {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s).await;
        s
    });
    let status = tokio::select! {
        s = child.wait() => s.ok(),
        _ = tokio::time::sleep(timeout) => {
            crate::drivers::terminal::kill_tree(pid);
            return Err(ProcessError::Timeout);
        }
        _ = cancel.cancelled() => {
            crate::drivers::terminal::kill_tree(pid);
            return Err(ProcessError::Cancelled);
        }
    };
    let stdout = tokio::time::timeout(Duration::from_secs(5), read_out).await.ok().and_then(Result::ok).unwrap_or_default();
    let stderr = tokio::time::timeout(Duration::from_secs(5), read_err).await.ok().and_then(Result::ok).unwrap_or_default();
    Ok(ProcessOutput { success: status.is_some_and(|s| s.success()), stdout, stderr })
}

/// A signed copy of agent-device's macOS helper shipped inside the app, so Accessibility and
/// Screen Recording stay granted across updates.
fn bundled_macos_helper() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("agent-device-macos-helper");
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::result_large_err)]
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    struct Env {
        work: PathBuf,
        session: PathBuf,
        attachments: Vec<PathBuf>,
        facts: SessionFacts,
    }

    impl Env {
        fn new() -> Self {
            Self {
                work: PathBuf::from("/w/cmd"),
                session: PathBuf::from("/w/sessions/a3f"),
                attachments: vec![PathBuf::from("/w/cmd/attachments/flow.ad"), PathBuf::from("/w/cmd/attachments/base.png")],
                facts: SessionFacts::default(),
            }
        }
        fn plan(&self, command: &str, args: &[&str]) -> Result<Plan, Output> {
            let ctx = PlanContext { workdir: &self.work, session_dir: &self.session, attachments: &self.attachments, facts: &self.facts };
            plan(command, &s(args), &ctx)
        }
    }

    #[test]
    fn plain_commands_pass_through() {
        let e = Env::new();
        let p = e.plan("click", &["@e2", "--button", "secondary"]).unwrap();
        assert_eq!(p.argv, s(&["click", "@e2", "--button", "secondary"]));
        assert!(p.expect.is_empty());
        let p = e.plan("snapshot", &["-i", "--json"]).unwrap();
        assert_eq!(p.argv, s(&["snapshot", "-i"]));
        let p = e.plan("fill", &["@e3", "hello world"]).unwrap();
        assert_eq!(p.argv, s(&["fill", "@e3", "hello world"]));
    }

    #[test]
    fn screenshots_land_in_the_workdir() {
        let e = Env::new();
        let p = e.plan("screenshot", &[]).unwrap();
        assert_eq!(p.argv, s(&["screenshot", "/w/cmd/screenshot.png"]));
        assert_eq!(p.expect[0].kind, FileKind::Screenshot);
        let p = e.plan("screenshot", &["--scale", "0.5", "../../etc/page"]).unwrap();
        assert_eq!(p.argv, s(&["screenshot", "--scale", "0.5", "/w/cmd/page.png"]));
        assert_eq!(p.expect[0].name, "page.png");
        let p = e.plan("screenshot", &["home.png", "--overlay-refs"]).unwrap();
        assert_eq!(p.argv, s(&["screenshot", "/w/cmd/home.png", "--overlay-refs"]));
    }

    #[test]
    fn diff_screenshot_uses_attachments_and_writes_the_diff_locally() {
        let e = Env::new();
        let p = e.plan("diff", &["screenshot", "--baseline", "~/shots/base.png"]).unwrap();
        assert_eq!(p.argv, s(&["diff", "screenshot", "--baseline", "/w/cmd/attachments/base.png", "--out", "/w/cmd/diff.png"]));
        assert_eq!(p.expect[0].kind, FileKind::Diff);
        let err = e.plan("diff", &["screenshot", "--baseline", "nope.png"]).unwrap_err();
        assert_eq!(err.error.unwrap().code, "invalid_args");
        assert!(e.plan("diff", &["screenshot"]).is_err());
        let p = e.plan("diff", &["snapshot", "-i"]).unwrap();
        assert_eq!(p.argv, s(&["diff", "snapshot", "-i"]));
    }

    #[test]
    fn recordings_live_in_the_session() {
        let e = Env::new();
        let p = e.plan("record", &["start"]).unwrap();
        assert_eq!(p.argv, s(&["record", "start", "/w/sessions/a3f/recordings/recording.mp4"]));
        assert_eq!(p.after, After::RecordingStarted(PathBuf::from("/w/sessions/a3f/recordings/recording.mp4")));
        let p = e.plan("record", &["start", "demo", "--fps", "30"]).unwrap();
        assert_eq!(p.argv, s(&["record", "start", "/w/sessions/a3f/recordings/demo.mp4", "--fps", "30"]));
        let p = e.plan("record", &["stop"]).unwrap();
        assert_eq!(p.after, After::RecordingStopped);
        let mut busy = Env::new();
        busy.facts.recording = Some(PathBuf::from("/x.mp4"));
        assert_eq!(busy.plan("record", &["start"]).unwrap_err().error.unwrap().code, "recording_in_progress");
    }

    #[test]
    fn native_named_open_retains_target_and_script_plan() {
        let mut e = Env::new();
        e.facts.helper_surface = true;
        let p = e.plan("open", &["com.example.Editor", "--save-script"]).unwrap();
        assert_eq!(p.argv, s(&["open", "com.example.Editor", "--save-script=/w/sessions/a3f/scripts/session.ad"]));
        assert!(matches!(p.after, After::ScriptArmed(_)));
        assert_eq!(e.plan("open", &["--surface", "frontmost-app"]).unwrap().argv, s(&["open", "--surface", "frontmost-app"]));
        assert_eq!(open_target(&s(&["--save-script", "/tmp/flow.ad", "Editor"])), Some("Editor".into()));
    }

    #[test]
    fn save_script_paths_are_chosen_here() {
        let e = Env::new();
        let p = e.plan("open", &["TextEdit", "--save-script"]).unwrap();
        assert_eq!(p.argv, s(&["open", "TextEdit", "--save-script=/w/sessions/a3f/scripts/session.ad"]));
        let p = e.plan("open", &["TextEdit", "--save-script", "./flows/login.ad"]).unwrap();
        assert_eq!(p.argv, s(&["open", "TextEdit", "--save-script=/w/sessions/a3f/scripts/login.ad"]));
        let p = e.plan("close", &["--save-script", "/tmp/x.ad"]).unwrap();
        assert_eq!(p.argv, s(&["close", "--save-script=/w/cmd/x.ad"]));
        assert_eq!(p.expect[0].kind, FileKind::ReplayScript);
        // `--save-script Notes` doesn't consume "Notes" (not a path), exactly like agent-device.
        let p = e.plan("close", &["--save-script", "Notes"]).unwrap();
        assert_eq!(p.argv, s(&["close", "Notes", "--save-script=/w/cmd/session.ad"]));
        let mut armed = Env::new();
        armed.facts.armed_script = Some(PathBuf::from("/w/sessions/a3f/scripts/login.ad"));
        let p = armed.plan("close", &[]).unwrap();
        assert_eq!(p.expect[0].path, PathBuf::from("/w/sessions/a3f/scripts/login.ad"));
        assert_eq!(p.after, After::SessionClosed);
    }

    #[test]
    fn scripts_must_be_attached() {
        let e = Env::new();
        let p = e.plan("replay", &["./flows/flow.ad", "--keep-session"]).unwrap();
        assert_eq!(p.argv, s(&["replay", "/w/cmd/attachments/flow.ad", "--keep-session"]));
        let err = e.plan("replay", &["other.ad"]).unwrap_err();
        assert!(err.error.unwrap().message.contains("other.ad"));
        assert!(e.plan("replay", &[]).is_err());
        let p = e.plan("test", &["flow.ad", "--retries", "1"]).unwrap();
        assert!(p.argv.contains(&"--artifacts-dir".to_string()));
        assert!(p.expect_dir.is_some());
    }

    #[test]
    fn generic_out_flags_are_rewritten() {
        let e = Env::new();
        let p = e.plan("record", &["contact-sheet", "x.mp4", "--out", "/etc/sheet.png"]).unwrap();
        assert!(p.argv.contains(&"/w/cmd/sheet.png".to_string()));
        assert_eq!(p.expect[0].name, "sheet.png");
    }

    #[test]
    fn value_flags_keep_positionals_straight() {
        assert_eq!(positional_indices(&s(&["--scale", "0.5", "a.png"])), vec![2]);
        assert_eq!(positional_indices(&s(&["-d", "3", "-i"])), Vec::<usize>::new());
        assert_eq!(positional_indices(&s(&["x", "--", "-y"])), vec![0, 2]);
    }

    #[test]
    fn parses_success_with_text() {
        let out = parse_result(r#"{"success":true,"data":{"path":"/x.png","width":10},"text":"/x.png (10x5)\n"}"#, "", true);
        assert!(out.ok);
        assert_eq!(out.text.as_deref(), Some("/x.png (10x5)"));
        assert_eq!(out.output["width"], 10);
        let out = parse_result("Replacing daemon\n{\n  \"success\": true,\n  \"data\": {\"message\": \"Opened: TextEdit\"}\n}\n", "", true);
        assert!(out.ok);
        assert_eq!(out.text.as_deref(), Some("Opened: TextEdit"));
        let out = parse_result(r#"{"success":true,"data":{"appName":"TextEdit","surface":"frontmost-app","nested":{}}}"#, "", true);
        assert_eq!(out.text.as_deref(), Some("appName: TextEdit\nsurface: frontmost-app"));
    }

    #[test]
    fn text_drops_local_details() {
        let mut out = Output::ok(serde_json::Value::Null, "Opened: TextEdit\nSession state: /x/sessions/bridge-a3f");
        tidy_text(&mut out, Path::new("/w/cmd"));
        assert_eq!(out.text.as_deref(), Some("Opened: TextEdit"));
        let mut out = Output::ok(serde_json::Value::Null, "/w/cmd/e2e.png (864x559)");
        tidy_text(&mut out, Path::new("/w/cmd"));
        assert_eq!(out.text.as_deref(), Some("e2e.png (864x559)"));
    }

    #[test]
    fn parses_errors_into_bridge_codes() {
        let out = parse_result(
            r#"{"success":false,"error":{"code":"INVALID_ARGS","message":"bad ref","hint":"Run snapshot","details":{"x":1}}}"#,
            "",
            false,
        );
        assert!(!out.ok);
        let e = out.error.unwrap();
        assert_eq!(e.code, "invalid_args");
        assert_eq!(e.message, "bad ref");
        assert_eq!(e.details["agent_device_code"], "INVALID_ARGS");
        assert_eq!(e.details["hint"], "Run snapshot");
        assert_eq!(e.details["agent_device"]["x"], 1);
        assert_eq!(out.text.as_deref(), Some("bad ref\nHint: Run snapshot"));
        assert_eq!(map_error_code("UNSUPPORTED_OPERATION"), "unsupported_on_device");
        assert_eq!(map_error_code("COMMAND_FAILED"), "command_failed");
        let out = parse_result("", "node: not found", false);
        assert_eq!(out.error.unwrap().code, "command_failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_cleanup_retries_incomplete_cleanup_and_preserves_unreleased_state() {
        use std::os::unix::fs::PermissionsExt as _;
        for (recover, reason, attempts) in [
            (true, "session_cleanup_incomplete", "2"),
            (false, "session_cleanup_incomplete", "2"),
            (false, "unrelated_failure", "1"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let state = dir.path().join("state");
            std::fs::create_dir_all(&state).unwrap();
            let script = dir.path().join("fake-ad");
            std::fs::write(&script, format!(r#"#!/bin/sh
count=0
[ ! -f "$AGENT_DEVICE_STATE_DIR/count" ] || count=$(cat "$AGENT_DEVICE_STATE_DIR/count")
count=$((count + 1))
echo "$count" > "$AGENT_DEVICE_STATE_DIR/count"
if [ "$count" -ge 2 ] && {recover}; then
  echo '{{"success":true,"data":{{}}}}'
else
  echo '{{"success":false,"error":{{"code":"COMMAND_FAILED","message":"cleanup failed","details":{{"reason":"{reason}"}}}}}}'
  exit 1
fi
"#)).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let driver = AgentDeviceDriver::new(
                Some(vec![script.display().to_string()]), None, "linux", state.clone(),
                dir.path().join("sessions"), std::sync::Arc::new(|_: &ProbeInput<'_>| unreachable!()),
            );
            driver.sessions.lock().unwrap().insert("a3f".into(), SessionFacts::default());
            let scratch = driver.session_dir("a3f");
            std::fs::create_dir_all(&scratch).unwrap();
            std::fs::write(scratch.join("capture.mp4"), "retry material").unwrap();
            driver.session_ended("a3f").await;
            assert_eq!(std::fs::read_to_string(state.join("count")).unwrap().trim(), attempts);
            assert_eq!(scratch.exists(), !recover);
            assert_eq!(driver.sessions.lock().unwrap().contains_key("a3f"), !recover);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_fake_agent_device_end_to_end() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        // A stand-in CLI that records its argv and writes the screenshot it's asked for.
        let script = dir.path().join("fake-ad");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"$@\" > \"$AGENT_DEVICE_STATE_DIR/argv\"\nif [ \"$1\" = screenshot ]; then printf png > \"$2\"; fi\n\
             echo '{\"success\":true,\"data\":{\"path\":\"'\"$2\"'\"},\"text\":\"done\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let driver = AgentDeviceDriver::new(
            Some(vec![script.display().to_string()]),
            None,
            "macos",
            state.clone(),
            dir.path().join("sessions"),
            std::sync::Arc::new(|_: &ProbeInput<'_>| unreachable!()),
        );
        let work = dir.path().join("work");
        let args = s(&["shot"]);
        let inv = Invocation {
            id: uuid::Uuid::nil(),
            session_id: "a3f",
            command: "screenshot",
            args: &args,
            attachments: &[],
            workdir: &work,
            timeout: Duration::from_secs(20),
            cancel: CancelToken::new(),
        };
        let out = driver.run(inv).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.text.as_deref(), Some("done"));
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].name, "shot.png");
        assert_eq!(std::fs::read(&out.files[0].path).unwrap(), b"png");
        let argv = std::fs::read_to_string(state.join("argv")).unwrap();
        assert!(argv.ends_with("--platform macos --session bridge-a3f --json\n"), "{argv}");
    }
}
