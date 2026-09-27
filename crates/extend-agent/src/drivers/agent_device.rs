//! Runs Silicon Extend's device engine (`vendor/extend-engine`) for Mac and Linux.
//!
//! Every command becomes one CLI run:
//!
//! ```text
//! node …/bin/extend-engine.mjs <command> <args…> --platform macos|linux --session extend-<session_id> --json
//! ```
//!
//! with `EXTEND_ENGINE_STATE_DIR` pointing at the agent's own state, so the device engine's daemon and
//! sessions are private to Extend, and `EXTEND_ENGINE_JSON_TEXT=1` (a fork addition) so one run
//! gives both the JSON result and the text the CLI would print. When the device engine is a node script
//! (always, as packaged), node gets the command's arguments over stdin through a small loader, so
//! typed text (`fill @e3 <password>`) never appears in the process list.
//!
//! Files: output paths are always chosen by the agent, inside the command's work directory, and
//! the files found there are handed back for upload. Input files (replay scripts, baselines,
//! step files) must come as attachments; a path on the Silicon's machine means nothing here.
//!
//! On a Mac every session runs on the device engine's native helper (Accessibility and Screen
//! Recording; no XCTest runner): it starts on the app in front, a link opens with the system and
//! is followed there, and an app named in `open`/`close` is found the way `open -a` finds it (its
//! bundle's file name, in the usual app folders, then Spotlight) and handed over by path.
//!
//! Ending a session closes its device-engine session, which releases the device engine's claim on the
//! computer. When that close fails, the session is kept on disk (`cleanup-pending.json`), agent-
//! device's daemon is restarted and the stale claim released. If even that fails, every
//! capability the device engine provides is reported missing with the reason (the computer stays
//! ready: the terminal works and a session can start), and the release is retried in the
//! background, forcing while no session is live, and again, forcing, when the next session
//! starts. The agent notes its live session on disk (`live-session`), so after a restart it
//! knows the session the service announces again and doesn't force a release under it.

use std::collections::{BTreeMap, HashMap};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use extend_driver::cancel::CancelToken;
use extend_driver::{Driver, Invocation, LocalFile, Output, Probe};
use extend_protocol::model::{CommandError, FileKind};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::drivers::args::{content_type_for, parse, safe_file_name, strip_agent_added, with_extension};

/// the device engine flags that take a value (from its flag registry), so positionals can be told apart.
pub const VALUE_FLAGS: &[&str] = &[
    "--activity",
    "--app",
    "--artifact",
    "--artifacts-dir",
    "--baseline",
    "--button",
    "--count",
    "--delay-ms",
    "--depth",
    "--duration-ms",
    "--env",
    "--fps",
    "--from",
    "--header",
    "--hold-ms",
    "--include",
    "--interval-ms",
    "--jitter-px",
    "--keyframes",
    "--kind",
    "--level",
    "--max-steps",
    "--on-error",
    "--out",
    "--pattern",
    "--pause-ms",
    "--pixels",
    "--plan-digest",
    "--pointer-count",
    "--quality",
    "--record-as",
    "--report-junit",
    "--retention-ms",
    "--retries",
    "--scope",
    "--settle-quiet",
    "--steps",
    "--steps-file",
    "--surface",
    "--target-app",
    "--template",
    "--threshold",
    "--timeout",
    "--until",
    "--wait",
    "--scale",
    "--crop-on",
    "-d",
    "-s",
    "-e",
    "-b",
];

/// What the driver remembers about one Extend session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFacts {
    /// Where `record start` is writing.
    pub recording: Option<PathBuf>,
    /// Where `open --save-script` asked for the script to go on `close`.
    pub armed_script: Option<PathBuf>,
    /// `logs start` ran and `logs stop` hasn't.
    pub logs_running: bool,
    /// the device engine ran for this session, so it may hold a device-engine session to close.
    pub touched: bool,
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
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.split_once('-'))
                    .is_some_and(|(_, rest)| rest == want)
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
            let takes_path = t == "--save-script" && args.get(i + 1).is_some_and(|n| looks_like_path(n));
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

/// the device engine's rule for whether `--save-script` consumes the next token.
fn looks_like_path(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty()
        && !t.contains("://")
        && (t.starts_with('/')
            || t.starts_with("./")
            || t.starts_with("../")
            || t.starts_with("~/")
            || t.contains('/')
            || t.contains('\\'))
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

/// the device engine's name for a recording quality Extend's CLI offers (`cli.yaml`: `normal` or
/// `high`). device-engine calls the default `medium`, and takes that spelling too.
#[allow(clippy::result_large_err)] // The error is the command's answer, returned as-is.
pub fn recording_quality(requested: &str) -> Result<&'static str, Output> {
    match requested.trim().to_ascii_lowercase().as_str() {
        "normal" | "medium" => Ok("medium"),
        "high" => Ok("high"),
        _ => Err(invalid(format!(
            "--quality {requested:?} isn't a recording quality. Use --quality normal (the default) or --quality high, or leave it out."
        ))),
    }
}

/// Puts the platform, session and `--json` flags the agent adds in front of a `--`, if the
/// command has one: after it the device engine reads every token as text (`type -- --json`), so flags
/// appended there would be typed instead of obeyed.
pub fn with_agent_flags(argv: &[String], flags: &[String]) -> Vec<String> {
    let mut full = Vec::with_capacity(argv.len() + flags.len());
    match argv.iter().position(|a| a == "--") {
        Some(at) => {
            full.extend_from_slice(&argv[..at]);
            full.extend_from_slice(flags);
            full.extend_from_slice(&argv[at..]);
        }
        None => {
            full.extend_from_slice(argv);
            full.extend_from_slice(flags);
        }
    }
    full
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
            let name = parsed
                .positional(0)
                .map(|n| with_extension(&safe_file_name(n, "screenshot.png"), &[".png"]));
            let name = name.unwrap_or_else(|| "screenshot.png".into());
            let path = ctx.workdir.join(&name);
            match pos.first() {
                Some(&i) => rest[i] = path.display().to_string(),
                None => rest.insert(0, path.display().to_string()),
            }
            expect.push(Expected {
                path,
                name,
                kind: FileKind::Screenshot,
            });
        }
        "diff" => {
            if parsed.positional(0) == Some("screenshot") {
                let Some(baseline) = parsed.value("--baseline") else {
                    return Err(invalid("diff screenshot needs --baseline <file>."));
                };
                let Some(b) = attachment(ctx, baseline) else {
                    return Err(missing_attachment("The baseline", baseline));
                };
                set_flag(&mut rest, "--baseline", &b.display().to_string());
                if let (Some(current), Some(&i)) = (parsed.positional(1), pos.get(1)) {
                    let Some(c) = attachment(ctx, current) else {
                        return Err(missing_attachment("The screenshot", current));
                    };
                    rest[i] = c.display().to_string();
                }
                let name = with_extension(
                    &safe_file_name(parsed.value("--out").unwrap_or("diff.png"), "diff.png"),
                    &[".png"],
                );
                let path = ctx.workdir.join(&name);
                set_flag(&mut rest, "--out", &path.display().to_string());
                expect.push(Expected {
                    path,
                    name,
                    kind: FileKind::Diff,
                });
            }
        }
        "record" => match parsed.positional(0) {
            Some("start") => {
                if ctx.facts.recording.is_some() {
                    return Err(Output::fail(
                        "recording_in_progress",
                        "A recording is already running in this session. Run `record stop` first.",
                    ));
                }
                let name = parsed
                    .positional(1)
                    .map(|n| with_extension(&safe_file_name(n, "recording.mp4"), &[".mp4", ".mov", ".webm"]))
                    .unwrap_or_else(|| "recording.mp4".into());
                if let Some(quality) = parsed.value("--quality") {
                    set_flag(&mut rest, "--quality", recording_quality(quality)?);
                }
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
                let name = value
                    .as_deref()
                    .map(|v| with_extension(&safe_file_name(v, "session.ad"), &[".ad"]))
                    .unwrap_or_else(|| "session.ad".into());
                let path = ctx.session_dir.join("scripts").join(name);
                rest.push(format!("--save-script={}", path.display()));
                after = After::ScriptArmed(path);
            }
        }
        "close" => {
            if let Some(value) = take_save_script(&mut rest) {
                let name = value
                    .as_deref()
                    .map(|v| with_extension(&safe_file_name(v, "session.ad"), &[".ad"]))
                    .unwrap_or_else(|| "session.ad".into());
                let path = ctx.workdir.join(&name);
                rest.push(format!("--save-script={}", path.display()));
                expect.push(Expected {
                    path,
                    name,
                    kind: FileKind::ReplayScript,
                });
            } else if let Some(armed) = &ctx.facts.armed_script {
                let name = armed
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("session.ad")
                    .to_owned();
                expect.push(Expected {
                    path: armed.clone(),
                    name,
                    kind: FileKind::ReplayScript,
                });
            }
            after = After::SessionClosed;
        }
        "replay" | "test" => {
            if pos.is_empty() {
                return Err(invalid(format!(
                    "{command} needs a script (.ad) sent with the command."
                )));
            }
            for &i in &pos {
                let wanted = rest[i].clone();
                let Some(a) = attachment(ctx, &wanted) else {
                    return Err(missing_attachment("The script", &wanted));
                };
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
                    expect.push(Expected {
                        path,
                        name,
                        kind: FileKind::Log,
                    });
                }
            }
        }
        "batch" => {
            if let Some(file) = parsed.value("--steps-file") {
                let Some(a) = attachment(ctx, file) else {
                    return Err(missing_attachment("The steps file", file));
                };
                set_flag(&mut rest, "--steps-file", &a.display().to_string());
            }
        }
        "install" | "reinstall" => {
            if let (Some(file), Some(&i)) = (parsed.positional(1), pos.get(1)) {
                let Some(a) = attachment(ctx, file) else {
                    return Err(missing_attachment("The app file", file));
                };
                rest[i] = a.display().to_string();
            }
        }
        _ => {}
    }
    // Anything else asked to write somewhere goes to the work directory and comes back as a file.
    if command != "diff"
        && let Some(out) = parsed.value("--out")
    {
        let name = safe_file_name(out, "output");
        let path = ctx.workdir.join(&name);
        set_flag(&mut rest, "--out", &path.display().to_string());
        expect.push(Expected {
            path,
            name,
            kind: FileKind::Other,
        });
    }

    let mut argv = Vec::with_capacity(rest.len() + 1);
    argv.push(command.to_owned());
    argv.extend(rest);
    Ok(Plan {
        argv,
        expect,
        expect_dir,
        after,
    })
}

/// the device engine's error codes, as Extend reports them.
pub fn map_error_code(code: &str) -> String {
    match code {
        "INVALID_ARGS" => "invalid_args".into(),
        "UNSUPPORTED_OPERATION" | "UNSUPPORTED_PLATFORM" | "NOT_SUPPORTED" | "UNSUPPORTED_COMMAND" => {
            "unsupported_on_device".into()
        }
        "" => "command_failed".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// Reads the device engine's `--json` document (`{"success":…,"data"|"error":…,"text"?}`) into an Output.
pub fn parse_result(stdout: &str, stderr: &str, exit_ok: bool) -> Output {
    let doc = find_json(stdout);
    let Some(doc) = doc else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        let tail: String = detail
            .chars()
            .rev()
            .take(2000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let message = if exit_ok {
            "The device engine finished without a result".to_string()
        } else {
            format!("The device engine failed without a result: {tail}")
        };
        return Output::fail("command_failed", message);
    };
    let success = doc.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
    let text = doc
        .get("text")
        .and_then(|t| t.as_str())
        .map(|t| t.trim_end().to_owned());
    if success {
        let data = doc.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let text = text
            .or_else(|| data.get("message").and_then(|m| m.as_str()).map(str::to_owned))
            .or_else(|| summarize(&data));
        return Output {
            ok: true,
            output: data,
            text,
            error: None,
            files: vec![],
        };
    }
    let err = doc.get("error").cloned().unwrap_or(serde_json::Value::Null);
    let raw_code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("The device engine reported an error")
        .to_owned();
    let hint = err.get("hint").and_then(|h| h.as_str()).map(str::to_owned);
    let mut details = serde_json::Map::new();
    details.insert("engine_code".into(), raw_code.into());
    if let Some(h) = &hint {
        details.insert("hint".into(), h.clone().into());
    }
    if let Some(d) = err.get("details").filter(|d| !d.is_null()) {
        details.insert("engine_details".into(), d.clone());
    }
    let text = match &hint {
        Some(h) => format!("{message}\nHint: {h}"),
        None => message.clone(),
    };
    Output {
        ok: false,
        output: serde_json::Value::Null,
        text: Some(text),
        error: Some(CommandError {
            code: map_error_code(raw_code),
            message,
            details: serde_json::Value::Object(details),
        }),
        files: vec![],
    }
}

/// Drops what only makes sense on this computer from the text a Silicon reads: the device engine's
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

/// `key: value` lines for a result the device engine prints no text for (`appstate`).
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

/// The JSON document in the device engine's stdout (it may be preceded by progress lines).
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

/// A file path the device engine reported in its result (`path`, `outputPath`, …) that exists.
pub fn reported_path(data: &serde_json::Value) -> Option<PathBuf> {
    for key in [
        "outPath",
        "outputPath",
        "path",
        "videoPath",
        "output",
        "file",
        "logPath",
    ] {
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
    /// Why the device engine can't run here, when it can't.
    pub problem: Option<&'a str>,
    /// The commands the device engine says this platform supports (`capabilities --json`), when known.
    pub commands: Option<&'a [String]>,
}

impl ProbeInput<'_> {
    /// True when the device engine supports `name` here (or when that isn't known).
    pub fn supports(&self, name: &str) -> bool {
        self.commands.is_none_or(|c| c.iter().any(|x| x == name))
    }
}

/// What the platform can do right now; supplied per OS.
pub type Prober = std::sync::Arc<dyn Fn(&ProbeInput<'_>) -> Probe + Send + Sync>;

/// File in a session's directory saying its device-engine session still has to be closed.
const CLEANUP_PENDING: &str = "cleanup-pending.json";

/// Background retries of a failed cleanup per run of the app, before waiting for the next
/// session or a restart.
const BACKGROUND_ATTEMPTS: u32 = 12;

/// Waits between background retries, as multiples of the first (15 s): 15 s, 30 s, 1, 2, then 5 min.
const RETRY_STEPS: [u32; 5] = [1, 2, 4, 8, 20];

/// A session whose device-engine session couldn't be closed yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Unreleased {
    session_id: String,
    /// Failed attempts so far.
    attempts: u32,
    /// What the last attempt said.
    error: String,
    /// Restarting the device engine's daemon was tried too, and the computer still isn't released.
    forced: bool,
    /// Failed attempts since this run of the app started.
    #[serde(skip)]
    tries: u32,
    #[serde(skip, default = "Instant::now")]
    next_retry: Instant,
}

#[derive(Default)]
struct Cleanups {
    pending: BTreeMap<String, Unreleased>,
    /// The background retry task is running.
    retrying: bool,
}

/// File in a live session's directory (the long-running agent's sessions), holding its id, so an
/// agent restarted mid-session knows the session the service announces again is already set up.
const LIVE: &str = "live-session";

/// Reads the sessions that were live when an earlier run of the agent stopped. What the device engine
/// did for them isn't known, so each counts as having used it.
fn load_live(data_dir: &Path) -> HashMap<String, SessionFacts> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.join(CLEANUP_PENDING).is_file() {
            continue;
        }
        let Ok(id) = std::fs::read_to_string(dir.join(LIVE)) else {
            continue;
        };
        let id = id.trim();
        if !id.is_empty() {
            out.insert(
                id.to_owned(),
                SessionFacts {
                    touched: true,
                    ..SessionFacts::default()
                },
            );
        }
    }
    out
}

/// Moves what only the device engine provides out of `probe.capabilities` into `probe.missing`, with
/// `reason`. Extend's own capabilities (the terminal, takeover) stay.
fn withhold_agent_device_capabilities(probe: &mut Probe, reason: &str) {
    use extend_protocol::Capability;
    use extend_protocol::capability::{COMMANDS, Origin};
    use extend_protocol::model::MissingCapability;
    let agent_device_only = |c: &Capability| {
        let users: Vec<_> = COMMANDS.iter().filter(|s| s.any_of.contains(c)).collect();
        !users.is_empty() && users.iter().all(|s| s.origin == Origin::AgentDevice)
    };
    let (held, kept): (Vec<Capability>, Vec<Capability>) =
        probe.capabilities.drain(..).partition(|c| agent_device_only(c));
    probe.capabilities = kept;
    probe
        .missing
        .extend(held.into_iter().map(|capability| MissingCapability {
            capability,
            reason: reason.to_owned(),
        }));
}

/// Reads the sessions an earlier run couldn't close; they are retried soon after start.
fn load_pending(data_dir: &Path) -> BTreeMap<String, Unreleased> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path().join(CLEANUP_PENDING);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        match serde_json::from_slice::<Unreleased>(&bytes) {
            Ok(mut u) => {
                u.next_retry = Instant::now();
                out.insert(u.session_id.clone(), u);
            }
            Err(error) => tracing::warn!("ignoring {}: {error}", path.display()),
        }
    }
    out
}

/// The device-engine driver. Cheap to share: the state lives behind one `Arc`, which the
/// background cleanup retry also holds.
pub struct AgentDeviceDriver {
    inner: Arc<AgentDevice>,
}

pub struct AgentDevice {
    command: Option<Vec<String>>,
    problem: Option<String>,
    platform: &'static str,
    state_dir: PathBuf,
    data_dir: PathBuf,
    /// Live sessions, plus ended ones still in `cleanups`.
    sessions: Mutex<HashMap<String, SessionFacts>>,
    cleanups: Mutex<Cleanups>,
    /// Session setup, cleanup and cleanup retries run one at a time.
    lifecycle: tokio::sync::Mutex<()>,
    /// This is the long-running agent: it knows which session is live (the service runs one at a
    /// time here), so it may force a release when none is, and it retries failed cleanup in the
    /// background. One-off `exec` and `probe` runs share its device-engine daemon and do neither.
    long_running: bool,
    first_retry: Duration,
    /// Where Mac apps are looked up by name, with how deep to look in each.
    app_roots: Vec<(PathBuf, usize)>,
    /// Ask Spotlight for an app that isn't in `app_roots`.
    spotlight: bool,
    /// What opens a link on a Mac (`/usr/bin/open`).
    link_opener: PathBuf,
    version: tokio::sync::OnceCell<Option<String>>,
    commands: tokio::sync::OnceCell<Option<Vec<String>>>,
    prober: Prober,
}

impl Deref for AgentDeviceDriver {
    type Target = AgentDevice;
    fn deref(&self) -> &AgentDevice {
        &self.inner
    }
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
        let macos = platform == "macos";
        let pending = load_pending(&data_dir);
        let live = load_live(&data_dir);
        Self {
            inner: Arc::new(AgentDevice {
                command,
                problem,
                platform,
                state_dir,
                data_dir,
                sessions: Mutex::new(live),
                cleanups: Mutex::new(Cleanups {
                    pending,
                    retrying: false,
                }),
                lifecycle: tokio::sync::Mutex::new(()),
                long_running: false,
                first_retry: Duration::from_secs(15),
                app_roots: if macos { macos_app_roots() } else { vec![] },
                spotlight: macos && cfg!(target_os = "macos"),
                link_opener: PathBuf::from("/usr/bin/open"),
                version: tokio::sync::OnceCell::new(),
                commands: tokio::sync::OnceCell::new(),
                prober,
            }),
        }
    }

    /// The driver of the long-running agent, which may force a stuck session's release at
    /// session boundaries and retries failed cleanup in the background.
    pub fn for_the_agent(mut self) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.long_running = true;
        }
        self
    }

    #[cfg(test)]
    #[cfg_attr(windows, allow(dead_code))]
    fn configured(mut self, f: impl FnOnce(&mut AgentDevice)) -> Self {
        f(Arc::get_mut(&mut self.inner).expect("not shared yet"));
        self
    }
}

impl AgentDevice {
    pub fn available(&self) -> bool {
        self.command.is_some()
    }

    fn agent_session(session_id: &str) -> String {
        format!("extend-{session_id}")
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join(safe_file_name(session_id, "session"))
    }

    fn facts(&self, session_id: &str) -> SessionFacts {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    fn update_facts(&self, session_id: &str, f: impl FnOnce(&mut SessionFacts)) {
        let mut map = self.sessions.lock().unwrap();
        f(map.entry(session_id.to_owned()).or_default());
    }

    /// the device engine's version (`--version`), read once.
    pub async fn engine_version(&self) -> Option<String> {
        self.version
            .get_or_init(|| async {
                let cmd = self.command.as_ref()?;
                let out = run_process(
                    cmd,
                    &["--version".to_string()],
                    &self.state_dir,
                    None,
                    Duration::from_secs(20),
                    &CancelToken::new(),
                )
                .await
                .ok()?;
                let v = out.stdout.trim().lines().last()?.trim().to_owned();
                (!v.is_empty() && v.len() < 32).then_some(v)
            })
            .await
            .clone()
    }

    /// The commands the device engine supports on this platform (`capabilities --json`), read once.
    pub async fn platform_commands(&self) -> Option<Vec<String>> {
        self.commands
            .get_or_init(|| async {
                let cmd = self.command.as_ref()?;
                let args: Vec<String> = ["capabilities", "--platform", self.platform, "--json"]
                    .map(String::from)
                    .to_vec();
                let out = run_process(
                    cmd,
                    &args,
                    &self.state_dir,
                    Some(&self.state_dir),
                    Duration::from_secs(60),
                    &CancelToken::new(),
                )
                .await
                .ok()?;
                let parsed = parse_result(&out.stdout, &out.stderr, out.success);
                let list = parsed.output.get("availableCommands")?.as_array()?;
                Some(list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            })
            .await
            .clone()
    }

    fn unavailable(&self) -> Output {
        Output::fail(
            "unsupported_on_device",
            self.problem
                .clone()
                .unwrap_or_else(|| "The device engine isn't available on this computer".into()),
        )
    }

    /// Runs the device engine once with the session and platform set.
    async fn invoke(
        &self,
        session_id: &str,
        argv: &[String],
        cwd: &Path,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Output {
        let Some(cmd) = &self.command else {
            return self.unavailable();
        };
        let full = with_agent_flags(
            argv,
            &[
                "--platform".into(),
                self.platform.into(),
                "--session".into(),
                Self::agent_session(session_id),
                "--json".into(),
            ],
        );
        let result = run_process(cmd, &full, &self.state_dir, Some(cwd), timeout, cancel).await;
        process_output(result, cmd, timeout)
    }

    /// Runs a device-engine command about the computer rather than one session
    /// (`daemon stop`, `device release`).
    async fn invoke_plain(&self, argv: &[&str], timeout: Duration) -> Output {
        let Some(cmd) = &self.command else {
            return self.unavailable();
        };
        let mut full: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
        full.push("--json".into());
        let _ = tokio::fs::create_dir_all(&self.state_dir).await;
        let result = run_process(
            cmd,
            &full,
            &self.state_dir,
            Some(&self.state_dir),
            timeout,
            &CancelToken::new(),
        )
        .await;
        process_output(result, cmd, timeout)
    }

    async fn collect(&self, plan: &Plan, out: &mut Output) {
        for e in &plan.expect {
            if tokio::fs::metadata(&e.path).await.is_ok_and(|m| m.is_file()) {
                out.files.push(LocalFile {
                    path: e.path.clone(),
                    name: e.name.clone(),
                    content_type: content_type_for(&e.name).into(),
                    kind: e.kind,
                });
            }
        }
        if let Some((dir, kind)) = &plan.expect_dir {
            for path in walk_files(dir) {
                let name = path
                    .strip_prefix(dir)
                    .map(|p| p.to_string_lossy().replace(['/', '\\'], "_"))
                    .unwrap_or_default();
                let name = safe_file_name(&name, "artifact");
                out.files.push(LocalFile {
                    content_type: content_type_for(&name).into(),
                    path,
                    name,
                    kind: *kind,
                });
            }
        }
    }

    /// Copies the session's app log into the work directory after `logs stop`.
    async fn collect_logs(&self, session_id: &str, workdir: &Path, out: &mut Output) {
        let path = self
            .invoke(
                session_id,
                &["logs".into(), "path".into()],
                workdir,
                Duration::from_secs(20),
                &CancelToken::new(),
            )
            .await;
        let Some(src) = path
            .output
            .get("path")
            .and_then(|p| p.as_str())
            .map(PathBuf::from)
            .or_else(|| reported_path(&path.output))
        else {
            return;
        };
        let dst = workdir.join("app.log");
        if tokio::fs::copy(&src, &dst).await.is_ok() {
            out.files.push(LocalFile {
                path: dst,
                name: "app.log".into(),
                content_type: "text/plain".into(),
                kind: FileKind::Log,
            });
        }
    }

    /// How a Mac app named in `open`/`close` is handed to the device engine, which matches only a
    /// bundle's display name in three folders: "Visual Studio Code" (whose bundle calls itself
    /// "Code") or an app in a subfolder would fail there. `Found` is the bundle's path, found by
    /// file name the way `open -a` finds it; a bare `Name.app` otherwise becomes `Name`, which
    /// the device engine would take for a bundle id. `None` leaves the target alone (a link, a path,
    /// the device engine's own `settings` alias).
    async fn resolve_macos_app(&self, target: &str) -> Option<AppTarget> {
        let t = target.trim();
        if t.is_empty()
            || t.contains("://")
            || t.contains('/')
            || t.starts_with('~')
            || t.eq_ignore_ascii_case("settings")
        {
            return None;
        }
        let roots = self.app_roots.clone();
        let name = t.to_owned();
        if let Ok(Some(path)) = tokio::task::spawn_blocking(move || find_app_bundle(&name, &roots)).await {
            return Some(AppTarget::Found(path.display().to_string()));
        }
        Some(AppTarget::Name(strip_app_suffix(t).map_or(t, str::trim).to_owned()))
    }

    /// Hands the app a planned Mac `open`/`close` names to the device engine by path, where one is
    /// found. `argv` is the planned run (command first). Returns the app's index when it stays a
    /// name, for a Spotlight retry.
    async fn resolve_named_app(&self, command: &str, argv: &mut [String]) -> Option<usize> {
        if self.platform != "macos"
            || !matches!(command, "open" | "close")
            || argv.iter().any(|a| a.starts_with("--surface"))
        {
            return None;
        }
        let i = positional_indices(argv.get(1..)?).first()? + 1;
        match self.resolve_macos_app(&argv[i]).await? {
            AppTarget::Found(path) => {
                tracing::debug!("{command} {:?} is {path}", argv[i]);
                argv[i] = path;
                None
            }
            AppTarget::Name(name) => {
                argv[i] = name;
                Some(i)
            }
        }
    }

    /// Opens a link with the system, then follows the app that comes to the front.
    async fn open_link(
        &self,
        session_id: &str,
        target: &str,
        workdir: &Path,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Output {
        let launch = tokio::process::Command::new(&self.link_opener)
            .arg(target)
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(20), launch).await {
            Ok(Ok(o)) if o.status.success() => {}
            Ok(Ok(o)) => {
                return Output::fail(
                    "app_not_found",
                    format!(
                        "macOS couldn't open {target}: {}. Check the link, or open the app that handles it by name.",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                );
            }
            Ok(Err(e)) => {
                return Output::fail(
                    "app_not_found",
                    format!("Couldn't run {} for {target}: {e}.", self.link_opener.display()),
                );
            }
            Err(_) => {
                return Output::fail(
                    "command_timeout",
                    format!(
                        "macOS didn't finish opening {target} within 20 s. Run `snapshot` to see what is in front, then try again."
                    ),
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let argv = ["open".to_owned(), "--surface".into(), "frontmost-app".into()];
        let mut out = self.invoke(session_id, &argv, workdir, timeout, cancel).await;
        tidy_text(&mut out, workdir);
        out
    }

    // ───────────── Session cleanup ─────────────

    fn retry_delay(&self, tries: u32) -> Duration {
        let step = RETRY_STEPS[(tries.max(1) as usize - 1).min(RETRY_STEPS.len() - 1)];
        self.first_retry * step
    }

    /// Records that `session_id`'s device-engine session still needs closing, on disk too so a
    /// restart retries it. `failed` counts an attempt; `forced` notes a failed forced release.
    fn note_pending(&self, session_id: &str, error: String, failed: bool, forced: bool) {
        let record = {
            let mut c = self.cleanups.lock().unwrap();
            let u = c.pending.entry(session_id.to_owned()).or_insert_with(|| Unreleased {
                session_id: session_id.to_owned(),
                attempts: 0,
                error: String::new(),
                forced: false,
                tries: 0,
                next_retry: Instant::now(),
            });
            if failed {
                u.attempts += 1;
                u.tries += 1;
            }
            u.error = error;
            u.forced |= forced;
            u.next_retry = Instant::now() + self.retry_delay(u.tries);
            u.clone()
        };
        let path = self.session_dir(session_id).join(CLEANUP_PENDING);
        if let Err(e) =
            crate::config::write_private_file(&path, &serde_json::to_vec_pretty(&record).unwrap_or_default())
        {
            tracing::warn!(
                session_id,
                "couldn't record the unfinished cleanup on disk (a restart won't retry it): {e:#}"
            );
        }
    }

    /// Sessions in use right now, as far as this driver knows.
    fn live_sessions(&self) -> Vec<String> {
        let pending: Vec<String> = self.cleanups.lock().unwrap().pending.keys().cloned().collect();
        self.sessions
            .lock()
            .unwrap()
            .keys()
            .filter(|id| !pending.contains(id))
            .cloned()
            .collect()
    }

    /// Drops everything kept for a session: its facts, a pending cleanup and its files.
    async fn forget_session(&self, session_id: &str) {
        self.sessions.lock().unwrap().remove(session_id);
        self.cleanups.lock().unwrap().pending.remove(session_id);
        let _ = tokio::fs::remove_dir_all(self.session_dir(session_id)).await;
    }

    /// Closes the session's device-engine session. A session the device engine doesn't have (it was
    /// never opened, the Silicon closed it, or the daemon was replaced) counts as closed.
    async fn close_session(&self, session_id: &str, timeout: Duration) -> Result<(), String> {
        let _ = tokio::fs::create_dir_all(&self.state_dir).await;
        let cancel = CancelToken::new();
        let close = ["close".to_owned()];
        let mut out = self.invoke(session_id, &close, &self.state_dir, timeout, &cancel).await;
        if closed_or_gone(&out) {
            return Ok(());
        }
        if out
            .error
            .as_ref()
            .is_some_and(|e| e.details["engine_details"]["reason"] == "session_cleanup_incomplete")
        {
            // A failed recording export may still dispose the native recorder. A second close
            // confirms the remaining cleanup and releases the retained claim.
            out = self.invoke(session_id, &close, &self.state_dir, timeout, &cancel).await;
            if closed_or_gone(&out) {
                return Ok(());
            }
        }
        Err(failure_text(&out))
    }

    /// Stops the device engine's daemon (ending every session it holds) and releases the claim it
    /// leaves on this computer.
    async fn force_release(&self) -> Result<(), String> {
        let stop = self.invoke_plain(&["daemon", "stop"], Duration::from_secs(45)).await;
        if !stop.ok {
            return Err(format!(
                "stopping the device engine's background process failed: {}",
                failure_text(&stop)
            ));
        }
        let release = self
            .invoke_plain(
                &["device", "release", "--stale", "--platform", self.platform],
                Duration::from_secs(20),
            )
            .await;
        if !release.ok {
            return Err(format!(
                "releasing the device engine's claim on this computer failed: {}",
                failure_text(&release)
            ));
        }
        let held: Vec<String> = ["retained", "refused"]
            .iter()
            .filter_map(|k| release.output.get(*k).and_then(|v| v.as_array()))
            .flatten()
            .map(|c| {
                c.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("no reason given")
                    .to_owned()
            })
            .collect();
        if !held.is_empty() {
            return Err(format!(
                "the device engine still holds its claim on this computer ({})",
                held.join("; ")
            ));
        }
        Ok(())
    }

    /// Closes one ended session. With `force`, a close that fails is followed by a forced
    /// release, which is safe only when no other session is live on this computer (session end,
    /// session start, or no session in use). True when the computer is released.
    async fn release(&self, session_id: &str, force: bool, timeout: Duration) -> bool {
        let error = match self.close_session(session_id, timeout).await {
            Ok(()) => {
                self.forget_session(session_id).await;
                return true;
            }
            Err(e) => e,
        };
        if !force {
            tracing::warn!(
                session_id,
                "the device engine still has the ended session open: {error}; retrying later"
            );
            self.note_pending(session_id, error, true, false);
            return false;
        }
        tracing::warn!(
            session_id,
            "closing the ended session failed ({error}); restarting the device engine to release this computer"
        );
        match self.force_release().await {
            Ok(()) => {
                tracing::info!(session_id, "the device engine restarted and this computer released");
                let mut ended: Vec<String> = self.cleanups.lock().unwrap().pending.keys().cloned().collect();
                ended.push(session_id.to_owned());
                for id in ended {
                    self.forget_session(&id).await;
                }
                true
            }
            Err(forced) => {
                tracing::error!(
                    session_id,
                    "this computer is still held by the ended session: {error}; {forced}"
                );
                self.note_pending(session_id, format!("{error}; then {forced}"), true, true);
                false
            }
        }
    }

    /// Retries pending cleanups: all of them, forcing, with `force` (no session is live in the
    /// agent), or those due for a background retry.
    async fn retry_pending(&self, force: bool) {
        let now = Instant::now();
        let ids: Vec<String> = self
            .cleanups
            .lock()
            .unwrap()
            .pending
            .values()
            .filter(|u| force || (u.next_retry <= now && u.tries < BACKGROUND_ATTEMPTS))
            .map(|u| u.session_id.clone())
            .collect();
        for id in ids {
            // A forced release for an earlier one covers the rest.
            if self.cleanups.lock().unwrap().pending.contains_key(&id) {
                self.release(&id, force, Duration::from_secs(20)).await;
            }
        }
    }

    /// Starts the background retry task when something is waiting for it.
    fn ensure_retrying(self: &Arc<Self>) {
        if !self.long_running {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        {
            let mut c = self.cleanups.lock().unwrap();
            if c.retrying || !c.pending.values().any(|u| u.tries < BACKGROUND_ATTEMPTS) {
                return;
            }
            c.retrying = true;
        }
        let me = self.clone();
        runtime.spawn(async move { me.retry_in_background().await });
    }

    async fn retry_in_background(self: Arc<Self>) {
        loop {
            let next = {
                let mut c = self.cleanups.lock().unwrap();
                match c
                    .pending
                    .values()
                    .filter(|u| u.tries < BACKGROUND_ATTEMPTS)
                    .map(|u| u.next_retry)
                    .min()
                {
                    Some(at) => at,
                    None => {
                        c.retrying = false;
                        return;
                    }
                }
            };
            tokio::time::sleep_until(next.into()).await;
            let _lifecycle = self.lifecycle.lock().await;
            // With no session in use, restarting the device engine's daemon ends nothing anyone needs.
            let force = self.long_running && self.live_sessions().is_empty();
            self.retry_pending(force).await;
        }
    }

    /// Why apps and the screen can't be used here, while an ended session still holds this
    /// computer after a forced release failed. It goes out as the reason for each capability
    /// the device engine provides, not as a setup step: the computer stays ready, so the terminal still
    /// works and the next session can start (which retries the release first).
    fn hold_reason(&self) -> Option<String> {
        let c = self.cleanups.lock().unwrap();
        let stuck: Vec<&Unreleased> = c.pending.values().filter(|u| u.forced).collect();
        let last = stuck.last()?;
        let word = crate::sysinfo::computer_word();
        let ids: Vec<&str> = stuck.iter().map(|u| u.session_id.as_str()).collect();
        let sessions = match ids.as_slice() {
            [one] => format!("Session {one}"),
            many => format!("Sessions {}", many.join(", ")),
        };
        Some(format!(
            "{sessions} ended, but the device engine couldn't release this {word}: {}. Until it does, apps and the screen can't be used here; the terminal still works. Extend keeps retrying on its own, restarting the device engine while no session is using this {word} and again when the next session starts. If this is still here after a few minutes, restarting this {word} clears it.",
            last.error
        ))
    }
}

/// What `resolve_macos_app` made of an app name.
#[derive(Debug, Clone, PartialEq)]
enum AppTarget {
    /// The bundle's path.
    Found(String),
    /// Not in the usual folders: the name, for the device engine's own lookup (then Spotlight).
    Name(String),
}

fn closed_or_gone(out: &Output) -> bool {
    out.ok || out.error.as_ref().is_some_and(|e| e.code == "session_not_found")
}

/// What a failed device-engine run said, for logs and the Carbon.
fn failure_text(out: &Output) -> String {
    match &out.error {
        Some(e) => format!("{} ({})", e.message.trim().trim_end_matches('.'), e.code),
        None => "the device engine gave no reason".into(),
    }
}

fn process_output(result: Result<ProcessOutput, ProcessError>, cmd: &[String], timeout: Duration) -> Output {
    match result {
        Ok(p) => parse_result(&p.stdout, &p.stderr, p.success),
        Err(ProcessError::Timeout) => Output::fail(
            "command_timeout",
            format!(
                "The device engine didn't answer within {} ms and was stopped.",
                timeout.as_millis()
            ),
        ),
        Err(ProcessError::Cancelled) => Output::fail("cancelled", "Extend cancelled this command."),
        Err(ProcessError::Spawn(e)) => Output::fail(
            "unsupported_on_device",
            format!("couldn't start the device engine ({}): {e}", cmd.join(" ")),
        ),
    }
}

// ───────────── Mac apps by name ─────────────

/// Characters some apps put in their names that nobody types (WhatsApp's starts with U+200E).
fn is_invisible_mark(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// `Name` for `Name.app` (any case).
fn strip_app_suffix(s: &str) -> Option<&str> {
    let n = s.len();
    (n > 4 && s.is_char_boundary(n - 4) && s[n - 4..].eq_ignore_ascii_case(".app")).then(|| &s[..n - 4])
}

/// How an app name compares: invisible marks and a trailing `.app` dropped, case folded.
fn app_name_key(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !is_invisible_mark(*c)).collect();
    let trimmed = cleaned.trim();
    strip_app_suffix(trimmed).unwrap_or(trimmed).trim().to_lowercase()
}

/// `com.example.Editor`: at least three dot-separated labels and no spaces.
fn looks_like_bundle_id(s: &str) -> bool {
    s.split('.').count() >= 3
        && s.split('.')
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// Where Mac apps live, in the order `open -a` prefers them, with how deep to look in each.
fn macos_app_roots() -> Vec<(PathBuf, usize)> {
    let mut roots = vec![(PathBuf::from("/Applications"), 3)];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push((PathBuf::from(home).join("Applications"), 3));
    }
    for (dir, depth) in [
        ("/System/Applications", 2),
        ("/System/Cryptexes/App/System/Applications", 1),
        ("/System/Library/CoreServices/Applications", 1),
        ("/System/Library/CoreServices", 1),
    ] {
        roots.push((PathBuf::from(dir), depth));
    }
    roots
}

/// Finds an app bundle whose file name is `name` (what `open -a` matches), searching each root
/// breadth-first down to its depth, without looking inside bundles.
pub fn find_app_bundle(name: &str, roots: &[(PathBuf, usize)]) -> Option<PathBuf> {
    let want = app_name_key(name);
    if want.is_empty() {
        return None;
    }
    for (root, depth) in roots {
        let mut level = vec![root.clone()];
        for _ in 0..*depth {
            let mut next = Vec::new();
            for dir in &level {
                let Ok(entries) = std::fs::read_dir(dir) else { continue };
                let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    let path = entry.path();
                    let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if file.starts_with('.') || !path.is_dir() {
                        continue;
                    }
                    if strip_app_suffix(file).is_some() {
                        if app_name_key(file) == want {
                            return Some(path);
                        }
                    } else if !entry.file_type().is_ok_and(|t| t.is_symlink()) {
                        next.push(path);
                    }
                }
            }
            level = next;
        }
    }
    None
}

/// Asks Spotlight for an app by file or display name (apps on other volumes, for example).
async fn spotlight_app(name: &str) -> Option<PathBuf> {
    let cleaned: String = name.chars().filter(|c| !is_invisible_mark(*c)).collect();
    let stem = strip_app_suffix(cleaned.trim())
        .unwrap_or(cleaned.trim())
        .trim()
        .to_owned();
    if stem.is_empty() {
        return None;
    }
    let quoted: String = stem
        .chars()
        .flat_map(|c| {
            if matches!(c, '"' | '\\' | '*' | '?') {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect();
    let query = format!(
        "kMDItemContentType == \"com.apple.application-bundle\" && (kMDItemFSName == \"{quoted}.app\"c || kMDItemDisplayName == \"{quoted}\"c)"
    );
    let run = tokio::process::Command::new("/usr/bin/mdfind")
        .arg(query)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(3), run).await.ok()?.ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(PathBuf::from)
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| strip_app_suffix(n).is_some())
                && p.is_dir()
        })
}

#[async_trait]
impl Driver for AgentDeviceDriver {
    async fn probe(&self) -> Probe {
        let (version, commands) = if self.available() {
            let _ = tokio::fs::create_dir_all(&self.state_dir).await;
            (self.engine_version().await, self.platform_commands().await)
        } else {
            (None, None)
        };
        let problem = (!self.available()).then(|| {
            self.problem
                .clone()
                .unwrap_or_else(|| "the device engine isn't available".into())
        });
        let prober = self.prober.clone();
        // Platform checks run helper programs; keep them off the async threads.
        let probed = tokio::task::spawn_blocking(move || {
            prober(&ProbeInput {
                problem: problem.as_deref(),
                commands: commands.as_deref(),
            })
        })
        .await;
        let mut probe = match probed {
            Ok(p) => p,
            Err(e) => return fallback_probe(self.platform, &format!("the device check crashed: {e}")),
        };
        probe.engine_version = version;
        if let Some(reason) = self.hold_reason() {
            withhold_agent_device_capabilities(&mut probe, &reason);
        }
        // Sessions an earlier run couldn't close are retried soon after start.
        self.inner.ensure_retrying();
        probe
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        self.update_facts(inv.session_id, |f| f.touched = true);
        if self.platform == "macos"
            && inv.command == "open"
            && let Some(target) = open_target(inv.args).filter(|target| target.contains("://"))
        {
            // Links have no app identity of their own; named apps bind to their native app surface.
            return self
                .open_link(inv.session_id, &target, inv.workdir, inv.timeout, &inv.cancel)
                .await;
        }
        self.run_planned(&inv).await
    }

    async fn session_started(&self, session_id: &str) {
        let _lifecycle = self.lifecycle.lock().await;
        let live = self.live_sessions();
        if live.iter().any(|id| id == session_id) {
            // Announced again, after a reconnect or a restart of the agent mid-session: the
            // session is live and set up; keep what it has (a running recording, say) and leave
            // the computer alone.
            return;
        }
        // The service runs one session at a time here, so any other session still thought live
        // ended while Extend wasn't told (it had quit, or lost its connection, by then).
        for id in live {
            if self.available() && self.facts(&id).touched {
                self.note_pending(
                    &id,
                    "It ended while Extend wasn't running or connected".into(),
                    false,
                    false,
                );
                let _ = tokio::fs::remove_file(self.session_dir(&id).join(LIVE)).await;
            } else {
                self.forget_session(&id).await;
            }
        }
        if self.available() {
            // An ended session whose cleanup failed may still hold this computer. Nothing else is
            // live now, so settle it, forcing if need be: the new session needs the computer.
            self.retry_pending(self.long_running).await;
            self.inner.ensure_retrying();
        }
        let macos = self.platform == "macos" && self.available();
        self.sessions.lock().unwrap().insert(
            session_id.to_owned(),
            SessionFacts {
                touched: macos,
                ..SessionFacts::default()
            },
        );
        if self.long_running
            && let Err(e) =
                crate::config::write_private_file(&self.session_dir(session_id).join(LIVE), session_id.as_bytes())
        {
            tracing::warn!(
                session_id,
                "couldn't note the live session on disk (a restart mid-session may cut it short): {e:#}"
            );
        }
        if macos {
            // Start on whatever app is in front, so `snapshot` works before any `open`.
            let dir = self.session_dir(session_id);
            let _ = tokio::fs::create_dir_all(&dir).await;
            let argv = ["open".to_owned(), "--surface".into(), "frontmost-app".into()];
            let _ = self
                .invoke(session_id, &argv, &dir, Duration::from_secs(20), &CancelToken::new())
                .await;
        }
    }

    async fn session_ended(&self, session_id: &str) {
        let _lifecycle = self.lifecycle.lock().await;
        let known = self.sessions.lock().unwrap().get(session_id).cloned();
        if !self.available() || known.as_ref().is_some_and(|f| !f.touched) {
            // the device engine never ran for this session (only `terminal`, say): nothing to close.
            self.forget_session(session_id).await;
            return;
        }
        let facts = known.unwrap_or_default();
        let _ = tokio::fs::create_dir_all(&self.state_dir).await;
        // Noted first, so a cleanup cut short (the app quits, the time limit passes) is retried.
        self.note_pending(
            session_id,
            "Its cleanup was interrupted before it finished".into(),
            false,
            false,
        );
        let _ = tokio::fs::remove_file(self.session_dir(session_id).join(LIVE)).await;
        let cancel = CancelToken::new();
        if facts.recording.is_some() {
            let _ = self
                .invoke(
                    session_id,
                    &["record".into(), "stop".into()],
                    &self.state_dir,
                    Duration::from_secs(30),
                    &cancel,
                )
                .await;
        }
        if facts.logs_running {
            let _ = self
                .invoke(
                    session_id,
                    &["logs".into(), "stop".into()],
                    &self.state_dir,
                    Duration::from_secs(15),
                    &cancel,
                )
                .await;
        }
        // Nothing else is live on this computer now, so the agent may force a failed close.
        if !self
            .release(session_id, self.long_running, Duration::from_secs(30))
            .await
        {
            self.inner.ensure_retrying();
        }
    }
}

impl AgentDeviceDriver {
    /// Plans and runs one command.
    async fn run_planned(&self, inv: &Invocation<'_>) -> Output {
        let facts = self.facts(inv.session_id);
        let session_dir = self.session_dir(inv.session_id);
        let ctx = PlanContext {
            workdir: inv.workdir,
            session_dir: &session_dir,
            attachments: inv.attachments,
            facts: &facts,
        };
        let mut plan = match plan(inv.command, inv.args, &ctx) {
            Ok(p) => p,
            Err(out) => return out,
        };
        // After planning, where `--save-script` carries its value (`--save-script=…`) and can't
        // take the app's path for the script's.
        let by_name = self.resolve_named_app(inv.command, &mut plan.argv).await;
        for dir in [
            inv.workdir.to_path_buf(),
            session_dir.join("recordings"),
            session_dir.join("scripts"),
        ] {
            let _ = tokio::fs::create_dir_all(&dir).await;
        }
        let mut out = self
            .invoke(inv.session_id, &plan.argv, inv.workdir, inv.timeout, &inv.cancel)
            .await;
        // An app the device engine couldn't find by name may still be somewhere Spotlight knows (another
        // volume, a deeper folder).
        if let Some(i) = by_name
            && self.spotlight
            && out.error.as_ref().is_some_and(|e| e.code == "app_not_installed")
            && !looks_like_bundle_id(&plan.argv[i])
            && let Some(path) = spotlight_app(&plan.argv[i]).await
        {
            plan.argv[i] = path.display().to_string();
            out = self
                .invoke(inv.session_id, &plan.argv, inv.workdir, inv.timeout, &inv.cancel)
                .await;
        }
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
                let Some(src) = found else {
                    return Output::fail(
                        "recording_export_failed",
                        "The recording stopped but its exported file is missing. Keep the session open and retry `record stop`.",
                    );
                };
                let name = src
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("recording.mp4")
                    .to_owned();
                let dst = inv.workdir.join(&name);
                // The completed recording manifest still names src; retain it for a retried export.
                if src != dst
                    && let Err(error) = tokio::fs::copy(&src, &dst).await
                {
                    return Output::fail(
                        "recording_export_failed",
                        format!(
                            "Couldn't copy the recording for upload: {error}. The original is retained; retry `record stop`."
                        ),
                    );
                }
                out.files.push(LocalFile {
                    path: dst,
                    content_type: content_type_for(&name).into(),
                    name,
                    kind: FileKind::Recording,
                });
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
}

/// A probe that reports nothing works, with why.
fn fallback_probe(platform: &str, why: &str) -> Probe {
    use extend_protocol::model::{MissingCapability, Setup};
    let os = if platform == "macos" {
        extend_protocol::DeviceOs::Macos
    } else {
        extend_protocol::DeviceOs::Linux
    };
    Probe {
        os,
        os_version: crate::sysinfo::os_version(),
        model: crate::sysinfo::model(),
        capabilities: vec![],
        missing: os
            .full_capabilities()
            .iter()
            .filter(|c| **c != extend_protocol::Capability::Terminal)
            .map(|c| MissingCapability {
                capability: *c,
                reason: why.to_owned(),
            })
            .collect(),
        setup: Setup::complete(),
        engine_version: None,
        online: true,
        awake: None,
        sleep_state: None,
        hardware_id: None,
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

/// Loads the device engine's node entry (`argv[1]`) with the arguments read as JSON from stdin, so
/// they never appear in the process list. The CLI reads `process.argv` as usual.
const ARGS_FROM_STDIN: &str = "import{pathToFileURL}from'node:url';let s='';process.stdin.setEncoding('utf8');for await(const c of process.stdin)s+=c;process.argv.push(...JSON.parse(s));await import(pathToFileURL(process.argv[1]).href);";

/// `[node, entry.mjs]`: device-engine run as a script, which can take its arguments over stdin.
fn node_script(cmd: &[String]) -> Option<&str> {
    match cmd {
        [_, entry] if [".mjs", ".js", ".cjs"].iter().any(|ext| entry.ends_with(ext)) => Some(entry),
        _ => None,
    }
}

/// Runs `cmd + args` with the device engine's environment, killing it (and its children) on timeout or
/// cancel. the device engine's daemon detaches itself, so it survives and keeps the session warm.
/// For a node script, `args` go over stdin instead of the command line (see [`ARGS_FROM_STDIN`]).
pub async fn run_process(
    cmd: &[String],
    args: &[String],
    state_dir: &Path,
    cwd: Option<&Path>,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<ProcessOutput, ProcessError> {
    let mut c = tokio::process::Command::new(&cmd[0]);
    let script = node_script(cmd);
    match script {
        Some(entry) => {
            c.args(["--input-type=module", "-e", ARGS_FROM_STDIN, "--", entry])
                .stdin(Stdio::piped());
        }
        None => {
            c.args(&cmd[1..]).args(args).stdin(Stdio::null());
        }
    }
    // The engine reads each EXTEND_ENGINE_<X> as its own setting (its entry points map them).
    c.env("EXTEND_ENGINE_STATE_DIR", state_dir)
        .env("EXTEND_ENGINE_NO_UPDATE_NOTIFIER", "1")
        .env("EXTEND_ENGINE_JSON_TEXT", "1")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    if let Some(helper) = bundled_macos_helper() {
        c.env("EXTEND_ENGINE_MACOS_HELPER_BIN", helper);
    }
    #[cfg(unix)]
    c.process_group(0);
    let mut child = c.spawn().map_err(ProcessError::Spawn)?;
    if script.is_some()
        && let Some(mut stdin) = child.stdin.take()
    {
        let payload = serde_json::to_vec(args).unwrap_or_else(|_| b"[]".to_vec());
        // Written apart from the wait below, so a child that exits early can't block it.
        tokio::spawn(async move {
            let _ = stdin.write_all(&payload).await;
        });
    }
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
    let stdout = tokio::time::timeout(Duration::from_secs(5), read_out)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let stderr = tokio::time::timeout(Duration::from_secs(5), read_err)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Ok(ProcessOutput {
        success: status.is_some_and(|s| s.success()),
        stdout,
        stderr,
    })
}

/// A signed copy of the device engine's macOS helper shipped inside the app, so Accessibility and
/// Screen Recording stay granted across updates.
fn bundled_macos_helper() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    // "Silicon Extend Helper" from 1.1 (the name System Settings shows); 1.0's name as a fallback.
    ["Silicon Extend Helper", "agent-device-macos-helper"]
        .iter()
        .map(|n| exe.parent().map(|d| d.join(n)))
        .find_map(|p| p.filter(|p| p.is_file()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::result_large_err)]
    // Most of these run the engine through a shell script, so a Windows test build compiles
    // helpers only Unix tests use.
    #![cfg_attr(windows, allow(unused_imports, dead_code))]
    use super::*;
    use extend_protocol::Capability;
    use extend_protocol::model::{MissingCapability, Setup, SetupState};

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
                attachments: vec![
                    PathBuf::from("/w/cmd/attachments/flow.ad"),
                    PathBuf::from("/w/cmd/attachments/base.png"),
                ],
                facts: SessionFacts::default(),
            }
        }
        fn plan(&self, command: &str, args: &[&str]) -> Result<Plan, Output> {
            let ctx = PlanContext {
                workdir: &self.work,
                session_dir: &self.session,
                attachments: &self.attachments,
                facts: &self.facts,
            };
            plan(command, &s(args), &ctx)
        }
    }

    #[test]
    fn recording_out_path_survives_a_new_driver_process() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("recording.mp4");
        std::fs::write(&output, b"recorded bytes").unwrap();
        assert_eq!(
            reported_path(&serde_json::json!({ "recording": "stopped", "outPath": output })),
            Some(output)
        );
        assert_eq!(reported_path(&serde_json::json!({ "outPath": dir.path() })), None);
        assert_eq!(
            reported_path(&serde_json::json!({ "outPath": dir.path().join("missing.mp4") })),
            None
        );
    }

    #[test]
    fn agent_flags_go_before_a_separator() {
        let flags = s(&["--platform", "linux", "--session", "extend-a3f", "--json"]);
        // After `--` the device engine types every token, so its flags must come first.
        assert_eq!(
            with_agent_flags(&s(&["type", "--", "--state-dir=~/notes"]), &flags),
            s(&[
                "type",
                "--platform",
                "linux",
                "--session",
                "extend-a3f",
                "--json",
                "--",
                "--state-dir=~/notes"
            ])
        );
        assert_eq!(
            with_agent_flags(&s(&["fill", "@e3", "--", "-x", "--"]), &flags),
            s(&[
                "fill",
                "@e3",
                "--platform",
                "linux",
                "--session",
                "extend-a3f",
                "--json",
                "--",
                "-x",
                "--"
            ])
        );
        assert_eq!(
            with_agent_flags(&s(&["snapshot", "-i"]), &flags),
            s(&[
                "snapshot",
                "-i",
                "--platform",
                "linux",
                "--session",
                "extend-a3f",
                "--json"
            ])
        );
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
        let p = e
            .plan("diff", &["screenshot", "--baseline", "~/shots/base.png"])
            .unwrap();
        assert_eq!(
            p.argv,
            s(&[
                "diff",
                "screenshot",
                "--baseline",
                "/w/cmd/attachments/base.png",
                "--out",
                "/w/cmd/diff.png"
            ])
        );
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
        assert_eq!(
            p.argv,
            s(&["record", "start", "/w/sessions/a3f/recordings/recording.mp4"])
        );
        assert_eq!(
            p.after,
            After::RecordingStarted(PathBuf::from("/w/sessions/a3f/recordings/recording.mp4"))
        );
        let p = e.plan("record", &["start", "demo", "--fps", "30"]).unwrap();
        assert_eq!(
            p.argv,
            s(&["record", "start", "/w/sessions/a3f/recordings/demo.mp4", "--fps", "30"])
        );
        let p = e.plan("record", &["stop"]).unwrap();
        assert_eq!(p.after, After::RecordingStopped);
        // `cli.yaml` offers normal and high; device-engine calls normal "medium".
        for (given, sent) in [
            (&["start", "--quality", "normal"][..], &["--quality", "medium"][..]),
            (&["start", "--quality=normal"][..], &["--quality=medium"][..]),
            (&["start", "--quality", "high"][..], &["--quality", "high"][..]),
            (&["start", "--quality", "HIGH"][..], &["--quality", "high"][..]),
            (&["start", "--quality", "medium"][..], &["--quality", "medium"][..]),
        ] {
            let p = e.plan("record", given).unwrap();
            assert!(
                p.argv.windows(sent.len()).any(|w| w == sent),
                "{given:?} became {:?}",
                p.argv
            );
            assert!(p.argv.contains(&"/w/sessions/a3f/recordings/recording.mp4".to_owned()));
        }
        let p = e
            .plan("record", &["start", "clip", "--quality", "normal", "--scope", "device"])
            .unwrap();
        assert_eq!(
            p.argv,
            s(&[
                "record",
                "start",
                "/w/sessions/a3f/recordings/clip.mp4",
                "--quality",
                "medium",
                "--scope",
                "device"
            ])
        );
        let err = e.plan("record", &["start", "--quality", "ultra"]).unwrap_err();
        let err = err.error.unwrap();
        assert_eq!(err.code, "invalid_args");
        assert!(
            err.message.contains("--quality normal") && err.message.contains("high"),
            "{}",
            err.message
        );
        let mut busy = Env::new();
        busy.facts.recording = Some(PathBuf::from("/x.mp4"));
        assert_eq!(
            busy.plan("record", &["start"]).unwrap_err().error.unwrap().code,
            "recording_in_progress"
        );
    }

    #[test]
    fn native_named_open_retains_target_and_script_plan() {
        let e = Env::new();
        let p = e.plan("open", &["com.example.Editor", "--save-script"]).unwrap();
        assert_eq!(
            p.argv,
            s(&[
                "open",
                "com.example.Editor",
                "--save-script=/w/sessions/a3f/scripts/session.ad"
            ])
        );
        assert!(matches!(p.after, After::ScriptArmed(_)));
        assert_eq!(
            e.plan("open", &["--surface", "frontmost-app"]).unwrap().argv,
            s(&["open", "--surface", "frontmost-app"])
        );
        assert_eq!(
            open_target(&s(&["--save-script", "/tmp/flow.ad", "Editor"])),
            Some("Editor".into())
        );
    }

    #[test]
    fn save_script_paths_are_chosen_here() {
        let e = Env::new();
        let p = e.plan("open", &["TextEdit", "--save-script"]).unwrap();
        assert_eq!(
            p.argv,
            s(&["open", "TextEdit", "--save-script=/w/sessions/a3f/scripts/session.ad"])
        );
        let p = e
            .plan("open", &["TextEdit", "--save-script", "./flows/login.ad"])
            .unwrap();
        assert_eq!(
            p.argv,
            s(&["open", "TextEdit", "--save-script=/w/sessions/a3f/scripts/login.ad"])
        );
        let p = e.plan("close", &["--save-script", "/tmp/x.ad"]).unwrap();
        assert_eq!(p.argv, s(&["close", "--save-script=/w/cmd/x.ad"]));
        assert_eq!(p.expect[0].kind, FileKind::ReplayScript);
        // `--save-script Notes` doesn't consume "Notes" (not a path), exactly like the device engine.
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
        let p = e
            .plan("record", &["contact-sheet", "x.mp4", "--out", "/etc/sheet.png"])
            .unwrap();
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
        let out = parse_result(
            r#"{"success":true,"data":{"path":"/x.png","width":10},"text":"/x.png (10x5)\n"}"#,
            "",
            true,
        );
        assert!(out.ok);
        assert_eq!(out.text.as_deref(), Some("/x.png (10x5)"));
        assert_eq!(out.output["width"], 10);
        let out = parse_result(
            "Replacing daemon\n{\n  \"success\": true,\n  \"data\": {\"message\": \"Opened: TextEdit\"}\n}\n",
            "",
            true,
        );
        assert!(out.ok);
        assert_eq!(out.text.as_deref(), Some("Opened: TextEdit"));
        let out = parse_result(
            r#"{"success":true,"data":{"appName":"TextEdit","surface":"frontmost-app","nested":{}}}"#,
            "",
            true,
        );
        assert_eq!(out.text.as_deref(), Some("appName: TextEdit\nsurface: frontmost-app"));
    }

    #[test]
    fn text_drops_local_details() {
        let mut out = Output::ok(
            serde_json::Value::Null,
            "Opened: TextEdit\nSession state: /x/sessions/extend-a3f",
        );
        tidy_text(&mut out, Path::new("/w/cmd"));
        assert_eq!(out.text.as_deref(), Some("Opened: TextEdit"));
        let mut out = Output::ok(serde_json::Value::Null, "/w/cmd/e2e.png (864x559)");
        tidy_text(&mut out, Path::new("/w/cmd"));
        assert_eq!(out.text.as_deref(), Some("e2e.png (864x559)"));
    }

    #[test]
    fn parses_errors_into_extend_codes() {
        let out = parse_result(
            r#"{"success":false,"error":{"code":"INVALID_ARGS","message":"bad ref","hint":"Run snapshot","details":{"x":1}}}"#,
            "",
            false,
        );
        assert!(!out.ok);
        let e = out.error.unwrap();
        assert_eq!(e.code, "invalid_args");
        assert_eq!(e.message, "bad ref");
        assert_eq!(e.details["engine_code"], "INVALID_ARGS");
        assert_eq!(e.details["hint"], "Run snapshot");
        assert_eq!(e.details["engine_details"]["x"], 1);
        assert_eq!(out.text.as_deref(), Some("bad ref\nHint: Run snapshot"));
        assert_eq!(map_error_code("UNSUPPORTED_OPERATION"), "unsupported_on_device");
        assert_eq!(map_error_code("COMMAND_FAILED"), "command_failed");
        let out = parse_result("", "node: not found", false);
        assert_eq!(out.error.unwrap().code, "command_failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recording_export_keeps_the_manifest_source_and_reports_copy_failure() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let source = state.join("recording.mp4");
        std::fs::write(&source, b"retryable recording").unwrap();
        let response = serde_json::json!({ "success": true, "data": { "recording": "stopped", "outPath": source } });
        std::fs::write(state.join("response.json"), response.to_string()).unwrap();
        let script = dir.path().join("fake-ad");
        std::fs::write(&script, "#!/bin/sh\ncat \"$EXTEND_ENGINE_STATE_DIR/response.json\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = AgentDeviceDriver::new(
            Some(vec![script.display().to_string()]),
            None,
            "linux",
            state.clone(),
            dir.path().join("sessions"),
            std::sync::Arc::new(|_: &ProbeInput<'_>| unreachable!()),
        );
        for (name, blocked) in [("first", false), ("retry", false), ("blocked", true)] {
            let work = dir.path().join(name);
            std::fs::create_dir(&work).unwrap();
            if blocked {
                std::fs::create_dir(work.join("recording.mp4")).unwrap();
            }
            let args = s(&["stop"]);
            let result = driver
                .run(Invocation {
                    id: uuid::Uuid::new_v4(),
                    session_id: "abc",
                    command: "record",
                    args: &args,
                    attachments: &[],
                    workdir: &work,
                    timeout: Duration::from_secs(10),
                    cancel: CancelToken::new(),
                })
                .await;
            if blocked {
                assert!(!result.ok);
                assert_eq!(result.error.unwrap().code, "recording_export_failed");
            } else {
                assert!(result.ok, "{result:?}");
                assert_eq!(result.files.len(), 1);
                assert_eq!(std::fs::read(&result.files[0].path).unwrap(), b"retryable recording");
            }
            assert_eq!(std::fs::read(&source).unwrap(), b"retryable recording");
        }
    }

    /// A stand-in the device engine for session lifecycle: every call is logged to `calls` (first two
    /// arguments) and `argv` (all of them); `close` answers as the `close` file says (ok, gone,
    /// incomplete-once, fail); `daemon stop` fails while `daemon-stop-fails` exists and otherwise
    /// ends every session (close then says gone). The platform check finds the screen, apps and
    /// takeover usable, and screen recording missing.
    #[cfg(unix)]
    fn lifecycle_driver(dir: &Path, platform: &'static str) -> (AgentDeviceDriver, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let script = dir.join("fake-ad");
        std::fs::write(&script, r#"#!/bin/sh
S="$EXTEND_ENGINE_STATE_DIR"
echo "$1 $2" >> "$S/calls"
echo "$*" >> "$S/argv"
fail() { echo "{\"success\":false,\"error\":{\"code\":\"$1\",\"message\":\"$2\",\"details\":{\"reason\":\"$3\"}}}"; exit 1; }
case "$1" in
  close)
    mode=$(cat "$S/close" 2>/dev/null || echo ok)
    case "$mode" in
      ok) echo '{"success":true,"data":{}}' ;;
      gone) fail SESSION_NOT_FOUND "No active session" "" ;;
      incomplete-once) echo ok > "$S/close"; fail COMMAND_FAILED "Session cleanup incomplete" session_cleanup_incomplete ;;
      *) fail COMMAND_FAILED "Recording export failed" session_cleanup_incomplete ;;
    esac ;;
  daemon)
    [ ! -f "$S/daemon-stop-fails" ] || fail COMMAND_FAILED "The daemon didn't stop" "" 
    echo gone > "$S/close"
    echo '{"success":true,"data":{"stopped":true}}' ;;
  device) echo '{"success":true,"data":{"released":[{"status":"released"}],"retained":[],"refused":[],"changed":[]}}' ;;
  *) echo '{"success":true,"data":{}}' ;;
esac
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = AgentDeviceDriver::new(
            Some(vec![script.display().to_string()]),
            None,
            platform,
            state.clone(),
            dir.join("sessions"),
            std::sync::Arc::new(|_: &ProbeInput<'_>| Probe {
                os: extend_protocol::DeviceOs::Linux,
                os_version: None,
                model: None,
                capabilities: vec![
                    Capability::ScreenRead,
                    Capability::AppsLaunch,
                    Capability::Links,
                    Capability::Takeover,
                ],
                missing: vec![MissingCapability {
                    capability: Capability::ScreenRecord,
                    reason: "ffmpeg isn't installed".into(),
                }],
                setup: Setup::complete(),
                engine_version: None,
                online: true,
                awake: None,
                sleep_state: None,
                hardware_id: None,
            }),
        );
        (driver, state)
    }

    #[cfg(unix)]
    fn agent_driver(dir: &Path) -> (AgentDeviceDriver, PathBuf) {
        let (driver, state) = lifecycle_driver(dir, "linux");
        (driver.for_the_agent(), state)
    }

    #[cfg(unix)]
    fn calls(state: &Path) -> Vec<String> {
        std::fs::read_to_string(state.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_owned())
            .collect()
    }

    #[cfg(unix)]
    async fn touch(driver: &AgentDeviceDriver, session: &str, dir: &Path) {
        let work = dir.join(format!("work-{session}"));
        std::fs::create_dir_all(&work).unwrap();
        let args = s(&["-i"]);
        let out = driver
            .run(Invocation {
                id: uuid::Uuid::new_v4(),
                session_id: session,
                command: "snapshot",
                args: &args,
                attachments: &[],
                workdir: &work,
                timeout: Duration::from_secs(10),
                cancel: CancelToken::new(),
            })
            .await;
        assert!(out.ok, "{out:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_agent_device_no_longer_has_counts_as_closed() {
        // The Silicon ran `extend close` itself (or the daemon was replaced): close answers
        // SESSION_NOT_FOUND, and the session's state and recordings still go.
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        driver.session_started("a3f").await;
        touch(&driver, "a3f", dir.path()).await;
        let recordings = driver.session_dir("a3f").join("recordings");
        std::fs::write(recordings.join("capture.mp4"), "kept for a retried export").unwrap();
        std::fs::write(state.join("close"), "gone").unwrap();
        driver.session_ended("a3f").await;
        assert_eq!(calls(&state), ["snapshot -i", "close --platform"]);
        assert!(!driver.session_dir("a3f").exists());
        assert!(!driver.sessions.lock().unwrap().contains_key("a3f"));
        assert!(driver.cleanups.lock().unwrap().pending.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_terminal_only_session_never_calls_agent_device() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        driver.session_started("a3f").await;
        driver.session_ended("a3f").await;
        assert!(calls(&state).is_empty(), "{:?}", calls(&state));
        assert!(!driver.sessions.lock().unwrap().contains_key("a3f"));
        // An unknown session (the agent restarted since it began) is closed to be safe.
        driver.session_ended("b40").await;
        assert_eq!(calls(&state), ["close --platform"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn incomplete_cleanup_is_confirmed_by_a_second_close() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        touch(&driver, "a3f", dir.path()).await;
        std::fs::write(state.join("close"), "incomplete-once").unwrap();
        driver.session_ended("a3f").await;
        assert_eq!(calls(&state), ["snapshot -i", "close --platform", "close --platform"]);
        assert!(!driver.session_dir("a3f").exists());
        assert!(driver.cleanups.lock().unwrap().pending.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_close_that_keeps_failing_is_forced_so_the_next_silicon_is_not_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = agent_driver(dir.path());
        touch(&driver, "a3f", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        driver.session_ended("a3f").await;
        assert_eq!(
            calls(&state),
            [
                "snapshot -i",
                "close --platform",
                "close --platform",
                "daemon stop",
                "device release",
            ]
        );
        assert!(!driver.session_dir("a3f").exists());
        assert!(driver.cleanups.lock().unwrap().pending.is_empty());
        let probe = driver.probe().await;
        assert!(probe.setup.steps.is_empty());
        assert_eq!(
            probe.capabilities,
            [
                Capability::ScreenRead,
                Capability::AppsLaunch,
                Capability::Links,
                Capability::Takeover
            ]
        );
    }

    /// What an unreleased computer reports: the device engine's capabilities missing with the reason,
    /// the rest (terminal, takeover) and setup untouched, so the service keeps it ready.
    fn assert_held(probe: &Probe, session: &str) {
        assert_eq!(
            probe.setup.state,
            SetupState::Complete,
            "a held computer must stay ready: {:?}",
            probe.setup
        );
        assert!(probe.setup.steps.is_empty());
        assert_eq!(probe.capabilities, [Capability::Takeover]);
        let held: Vec<Capability> = probe
            .missing
            .iter()
            .filter(|m| m.reason.contains("couldn't release"))
            .map(|m| m.capability)
            .collect();
        assert_eq!(
            held,
            [Capability::ScreenRead, Capability::AppsLaunch, Capability::Links]
        );
        let reason = &probe
            .missing
            .iter()
            .find(|m| m.capability == Capability::AppsLaunch)
            .unwrap()
            .reason;
        assert!(
            reason.contains(&format!("Session {session} ended"))
                && reason.contains("Recording export failed")
                && reason.contains("The daemon didn't stop"),
            "{reason}"
        );
        assert!(
            reason.contains("the terminal still works") && reason.contains("when the next session starts"),
            "{reason}"
        );
        // What was missing already keeps its own reason.
        assert!(
            probe
                .missing
                .iter()
                .any(|m| m.capability == Capability::ScreenRecord && m.reason == "ffmpeg isn't installed")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreleased_session_is_shown_kept_across_restarts_and_retried_before_the_next_session() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = agent_driver(dir.path());
        touch(&driver, "a3f", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        std::fs::write(state.join("daemon-stop-fails"), "").unwrap();
        driver.session_ended("a3f").await;
        let scratch = driver.session_dir("a3f");
        assert!(scratch.join(CLEANUP_PENDING).is_file());
        assert_held(&driver.probe().await, "a3f");
        // With a local driver the terminal is added back, and the service would store "ready".
        let hello = crate::agent::hello_from(&crate::drivers::local::with_terminal(driver.probe().await));
        assert!(hello.capabilities.contains(&Capability::Terminal));
        assert!(hello.setup.state == SetupState::Complete || hello.setup.steps.is_empty());

        // The app restarts: the new driver finds the session on disk.
        drop(driver);
        let (driver, state) = agent_driver(dir.path());
        assert!(driver.cleanups.lock().unwrap().pending.contains_key("a3f"));
        assert_held(&driver.probe().await, "a3f");

        // Before the next session starts, the close is retried and the computer is released.
        std::fs::remove_file(state.join("daemon-stop-fails")).unwrap();
        std::fs::remove_file(state.join("calls")).unwrap();
        driver.session_started("b40").await;
        assert_eq!(
            calls(&state),
            ["close --platform", "close --platform", "daemon stop", "device release"]
        );
        assert!(!scratch.exists());
        assert!(driver.cleanups.lock().unwrap().pending.is_empty());
        let probe = driver.probe().await;
        assert!(probe.setup.steps.is_empty());
        assert_eq!(
            probe.capabilities,
            [
                Capability::ScreenRead,
                Capability::AppsLaunch,
                Capability::Links,
                Capability::Takeover
            ]
        );
    }

    /// Waits until no cleanup is pending (or ~5 s pass).
    #[cfg(unix)]
    async fn until_released(driver: &AgentDeviceDriver) -> bool {
        for _ in 0..100 {
            if driver.cleanups.lock().unwrap().pending.is_empty() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn with_no_session_in_use_the_background_retry_restarts_agent_device() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        let driver = driver
            .for_the_agent()
            .configured(|d| d.first_retry = Duration::from_millis(100));
        touch(&driver, "a3f", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        std::fs::write(state.join("daemon-stop-fails"), "").unwrap();
        driver.session_ended("a3f").await;
        assert!(!driver.cleanups.lock().unwrap().pending.is_empty());
        // The close still fails, but the device engine's daemon can be stopped now.
        std::fs::remove_file(state.join("daemon-stop-fails")).unwrap();
        assert!(until_released(&driver).await, "{:?}", calls(&state));
        assert_eq!(
            calls(&state).iter().filter(|c| *c == "daemon stop").count(),
            2,
            "{:?}",
            calls(&state)
        );
        assert!(!driver.session_dir("a3f").exists());
        assert_eq!(driver.probe().await.capabilities.len(), 4);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn while_a_session_is_in_use_the_background_retry_never_restarts_agent_device() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        let driver = driver
            .for_the_agent()
            .configured(|d| d.first_retry = Duration::from_millis(50));
        touch(&driver, "0ld", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        std::fs::write(state.join("daemon-stop-fails"), "").unwrap();
        driver.session_ended("0ld").await;
        // The next session starts (its forced retry fails too) and is in use.
        driver.session_started("a3f").await;
        touch(&driver, "a3f", dir.path()).await;
        std::fs::remove_file(state.join("daemon-stop-fails")).unwrap();
        std::fs::remove_file(state.join("calls")).unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let during = calls(&state);
        assert!(
            during.iter().any(|c| c == "close --platform"),
            "the close is still retried: {during:?}"
        );
        assert!(
            !during.iter().any(|c| c == "daemon stop"),
            "a live session's daemon was stopped: {during:?}"
        );
        // Once it ends, its own cleanup forces the release, for both.
        driver.session_ended("a3f").await;
        assert!(
            driver.cleanups.lock().unwrap().pending.is_empty(),
            "{:?}",
            calls(&state)
        );
        assert!(!driver.session_dir("0ld").exists() && !driver.session_dir("a3f").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_agent_restarted_mid_session_leaves_that_session_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = agent_driver(dir.path());
        touch(&driver, "0ld", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        std::fs::write(state.join("daemon-stop-fails"), "").unwrap();
        driver.session_ended("0ld").await;
        driver.session_started("a3f").await;
        assert!(driver.session_dir("a3f").join(LIVE).is_file());

        // The app restarts while a3f is in use; the service announces a3f again on reconnect.
        drop(driver);
        std::fs::remove_file(state.join("daemon-stop-fails")).unwrap();
        std::fs::remove_file(state.join("calls")).unwrap();
        let (driver, state) = agent_driver(dir.path());
        assert!(driver.cleanups.lock().unwrap().pending.contains_key("0ld"));
        assert_eq!(driver.live_sessions(), ["a3f"]);
        driver.session_started("a3f").await;
        assert!(
            calls(&state).is_empty(),
            "a live session's daemon was touched: {:?}",
            calls(&state)
        );

        // When a3f ends, its cleanup runs as usual (forcing, for both sessions).
        driver.session_ended("a3f").await;
        assert_eq!(
            calls(&state),
            ["close --platform", "close --platform", "daemon stop", "device release"]
        );
        assert!(driver.cleanups.lock().unwrap().pending.is_empty());
        assert!(!driver.session_dir("a3f").exists() && !driver.session_dir("0ld").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_that_ended_while_the_agent_was_down_is_closed_when_the_next_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = agent_driver(dir.path());
        driver.session_started("a3f").await;
        touch(&driver, "a3f", dir.path()).await;
        // The app stops without a3f's cleanup; a3f ends meanwhile, and b40 starts.
        drop(driver);
        std::fs::remove_file(state.join("calls")).unwrap();
        let (driver, state) = agent_driver(dir.path());
        driver.session_started("b40").await;
        assert_eq!(calls(&state), ["close --platform"]);
        assert!(!driver.session_dir("a3f").exists());
        assert_eq!(driver.live_sessions(), ["b40"]);
        let argv = std::fs::read_to_string(state.join("argv")).unwrap();
        assert!(argv.lines().last().unwrap().contains("--session extend-a3f"), "{argv}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_agent_retries_an_unreleased_session_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        let driver = driver
            .for_the_agent()
            .configured(|d| d.first_retry = Duration::from_millis(100));
        touch(&driver, "a3f", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        std::fs::write(state.join("daemon-stop-fails"), "").unwrap();
        driver.session_ended("a3f").await;
        assert!(driver.cleanups.lock().unwrap().pending.contains_key("a3f"));
        // the device engine recovers on its own; the background retry closes the session (no force).
        std::fs::write(state.join("close"), "ok").unwrap();
        for _ in 0..100 {
            if driver.cleanups.lock().unwrap().pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            driver.cleanups.lock().unwrap().pending.is_empty(),
            "{:?}",
            calls(&state)
        );
        assert!(!driver.session_dir("a3f").exists());
        assert_eq!(
            calls(&state).iter().filter(|c| *c == "daemon stop").count(),
            1,
            "only the forced release at session end"
        );
        assert!(!driver.cleanups.lock().unwrap().retrying || driver.cleanups.lock().unwrap().pending.is_empty());
    }

    /// Every Mac session runs on the native helper, whatever the XCTest runner's state: it starts
    /// on the app in front, and a link opens with the system and is followed there.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_mac_session_follows_the_front_app_and_links_open_with_the_system() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "macos");
        let opener = dir.path().join("open");
        std::fs::write(
            &opener,
            format!("#!/bin/sh\necho \"$@\" >> {}\n", dir.path().join("opened").display()),
        )
        .unwrap();
        std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
        let driver = driver.configured(|d| {
            d.link_opener = opener.clone();
            d.spotlight = false;
        });
        driver.session_started("a3f").await;
        assert_eq!(calls(&state), ["open --surface"]);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let args = s(&["https://example.com/a?b=c"]);
        let out = driver
            .run(Invocation {
                id: uuid::Uuid::new_v4(),
                session_id: "a3f",
                command: "open",
                args: &args,
                attachments: &[],
                workdir: &work,
                timeout: Duration::from_secs(10),
                cancel: CancelToken::new(),
            })
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("opened")).unwrap().trim(),
            "https://example.com/a?b=c"
        );
        assert_eq!(calls(&state), ["open --surface", "open --surface"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_announced_again_after_a_reconnect_keeps_its_state() {
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = agent_driver(dir.path());
        // An earlier session is still unreleased, so a new session would force a release.
        touch(&driver, "0ld", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        std::fs::write(state.join("daemon-stop-fails"), "").unwrap();
        driver.session_ended("0ld").await;
        driver.session_started("a3f").await;
        driver.update_facts("a3f", |f| f.recording = Some(PathBuf::from("/x/recording.mp4")));
        std::fs::remove_file(state.join("calls")).unwrap();
        // The socket drops and the service says session_started again for the live session.
        driver.session_started("a3f").await;
        assert!(calls(&state).is_empty(), "{:?}", calls(&state));
        assert_eq!(driver.facts("a3f").recording, Some(PathBuf::from("/x/recording.mp4")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_one_off_run_never_restarts_the_agents_daemon() {
        // `extend-agent exec --end-session` shares the running agent's daemon, which may hold a
        // Silicon's live session: it records the failure for the agent instead of forcing.
        let dir = tempfile::tempdir().unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "linux");
        touch(&driver, "e2e", dir.path()).await;
        std::fs::write(state.join("close"), "fail").unwrap();
        driver.session_ended("e2e").await;
        assert_eq!(calls(&state), ["snapshot -i", "close --platform", "close --platform"]);
        assert!(driver.session_dir("e2e").join(CLEANUP_PENDING).is_file());
        // The agent picks it up.
        let (agent, _) = agent_driver(dir.path());
        assert!(agent.cleanups.lock().unwrap().pending.contains_key("e2e"));
    }

    #[test]
    fn mac_apps_are_found_by_bundle_file_name_like_open_dash_a() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        for app in [
            "Visual Studio Code.app/Contents/Other.app",
            "Utilities/iTerm.app",
            "\u{200E}WhatsApp.app",
            "TextEdit.app",
            "Deep/A/B/Hidden.app",
            "Suite.localized/Suite Tool.app",
        ] {
            std::fs::create_dir_all(apps.join(app)).unwrap();
        }
        std::fs::write(apps.join("NotADir.app"), "").unwrap();
        let roots = vec![(apps.clone(), 3)];
        let find = |n: &str| find_app_bundle(n, &roots);
        assert_eq!(find("Visual Studio Code"), Some(apps.join("Visual Studio Code.app")));
        assert_eq!(
            find("visual studio code.APP"),
            Some(apps.join("Visual Studio Code.app"))
        );
        assert_eq!(find("iTerm"), Some(apps.join("Utilities/iTerm.app")));
        assert_eq!(find("WhatsApp"), Some(apps.join("\u{200E}WhatsApp.app")));
        assert_eq!(find("TextEdit.app"), Some(apps.join("TextEdit.app")));
        assert_eq!(find("Suite Tool"), Some(apps.join("Suite.localized/Suite Tool.app")));
        // Deeper than the root allows, inside a bundle, or not a bundle at all.
        assert_eq!(find("Hidden"), None);
        assert_eq!(find("Other"), None);
        assert_eq!(find("NotADir"), None);
        assert_eq!(find(""), None);
        assert!(looks_like_bundle_id("com.microsoft.VSCode"));
        assert!(!looks_like_bundle_id("draw.io") && !looks_like_bundle_id("Visual Studio Code"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn named_mac_apps_reach_agent_device_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        std::fs::create_dir_all(apps.join("Visual Studio Code.app")).unwrap();
        std::fs::create_dir_all(apps.join("Utilities/iTerm.app")).unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "macos");
        let driver = driver.configured(|d| {
            d.app_roots = vec![(apps.clone(), 3)];
            d.spotlight = false;
        });
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        for (command, args) in [
            ("open", vec!["Visual Studio Code", "--save-script"]),
            ("close", vec!["iTerm"]),
            ("open", vec!["TextEdit.app"]),
            ("open", vec!["com.example.Editor"]),
            ("open", vec!["Settings"]),
        ] {
            let args = s(&args);
            let out = driver
                .run(Invocation {
                    id: uuid::Uuid::new_v4(),
                    session_id: "a3f",
                    command,
                    args: &args,
                    attachments: &[],
                    workdir: &work,
                    timeout: Duration::from_secs(10),
                    cancel: CancelToken::new(),
                })
                .await;
            assert!(out.ok, "{out:?}");
        }
        let log = std::fs::read_to_string(state.join("calls")).unwrap();
        let firsts: Vec<&str> = log
            .lines()
            .map(|l| l.split_once(' ').map_or("", |(_, rest)| rest).trim())
            .collect();
        let vsc = apps.join("Visual Studio Code.app").display().to_string();
        // The fake logs only its first two arguments: the command and the (resolved) app.
        assert_eq!(
            firsts,
            [
                vsc.as_str(),
                &apps.join("Utilities/iTerm.app").display().to_string(),
                "TextEdit",
                "com.example.Editor",
                "Settings"
            ]
        );
    }

    /// A bare `--save-script` before the app doesn't take the app's resolved path for the script's.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_bare_save_script_before_a_named_app_keeps_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        std::fs::create_dir_all(apps.join("Notes.app")).unwrap();
        std::fs::create_dir_all(apps.join("Utilities/iTerm.app")).unwrap();
        let (driver, state) = lifecycle_driver(dir.path(), "macos");
        let driver = driver.configured(|d| {
            d.app_roots = vec![(apps.clone(), 3)];
            d.spotlight = false;
        });
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        for (command, args) in [
            ("open", vec!["--save-script", "Notes"]),
            ("close", vec!["--save-script", "iTerm"]),
            ("open", vec!["--save-script", "./flows/login.ad", "Notes"]),
            ("close", vec!["--save-script=/tmp/x.ad", "iTerm"]),
        ] {
            let args = s(&args);
            let out = driver
                .run(Invocation {
                    id: uuid::Uuid::new_v4(),
                    session_id: "a3f",
                    command,
                    args: &args,
                    attachments: &[],
                    workdir: &work,
                    timeout: Duration::from_secs(10),
                    cancel: CancelToken::new(),
                })
                .await;
            assert!(out.ok, "{out:?}");
        }
        let notes = apps.join("Notes.app").display().to_string();
        let iterm = apps.join("Utilities/iTerm.app").display().to_string();
        let scripts = driver.session_dir("a3f").join("scripts");
        let tail = "--platform macos --session extend-a3f --json";
        let argv = std::fs::read_to_string(state.join("argv")).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            [
                format!(
                    "open {notes} --save-script={} {tail}",
                    scripts.join("session.ad").display()
                ),
                format!(
                    "close {iterm} --save-script={} {tail}",
                    work.join("session.ad").display()
                ),
                format!(
                    "open {notes} --save-script={} {tail}",
                    scripts.join("login.ad").display()
                ),
                format!("close {iterm} --save-script={} {tail}", work.join("x.ad").display()),
            ]
        );
    }

    /// Typed text reaches the device engine's CLI but not its command line (`ps` shows argv).
    #[cfg(unix)]
    #[tokio::test]
    async fn typed_text_stays_out_of_the_process_list() {
        let Some(node) = crate::config::which("node") else {
            eprintln!("skipped: node isn't on PATH");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("extend-engine.mjs");
        std::fs::write(
            &entry,
            r#"import { execFileSync } from 'node:child_process';
const ps = execFileSync('ps', ['-ww', '-o', 'args=', '-p', String(process.pid)]).toString();
console.log(JSON.stringify({ success: true, data: { argv: process.argv.slice(2), ps } }));
"#,
        )
        .unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let driver = AgentDeviceDriver::new(
            Some(vec![node.display().to_string(), entry.display().to_string()]),
            None,
            "linux",
            state.clone(),
            dir.path().join("sessions"),
            std::sync::Arc::new(|_: &ProbeInput<'_>| unreachable!()),
        );
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let secret = "hunter2 \"quoted\" ünïcode";
        let args = vec!["@e3".to_owned(), secret.to_owned()];
        let out = driver
            .run(Invocation {
                id: uuid::Uuid::new_v4(),
                session_id: "a3f",
                command: "fill",
                args: &args,
                attachments: &[],
                workdir: &work,
                timeout: Duration::from_secs(20),
                cancel: CancelToken::new(),
            })
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(
            out.output["argv"],
            serde_json::json!([
                "fill",
                "@e3",
                secret,
                "--platform",
                "linux",
                "--session",
                "extend-a3f",
                "--json"
            ])
        );
        let ps = out.output["ps"].as_str().unwrap();
        assert!(ps.contains("extend-engine.mjs"), "{ps}");
        assert!(!ps.contains("hunter2"), "the typed text is in the process list: {ps}");
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
            "#!/bin/sh\necho \"$@\" > \"$EXTEND_ENGINE_STATE_DIR/argv\"\nif [ \"$1\" = screenshot ]; then printf png > \"$2\"; fi\n\
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
        assert!(
            argv.ends_with("--platform macos --session extend-a3f --json\n"),
            "{argv}"
        );
    }
}
