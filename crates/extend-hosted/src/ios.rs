//! iPhone and iPad, through agent-device on the Mac they are paired with.
//!
//! Every command runs as `<agent-device argv> <command> <args…> --platform ios --udid <udid> --json
//! --session extend-<session id>`. Files a command writes (screenshots, recordings, diffs, replay
//! scripts) are given explicit paths in the command's workdir so they come back as `LocalFile`s.
//!
//! Setup follows UNDERSTANDING.md: the Carbon plugs the iPhone in and taps Trust, turns on
//! Developer Mode, and Extend then puts agent-device's XCTest runner (the "helper") on it with
//! `prepare ios-runner`. Readiness comes from `xcrun devicectl`.
//!
//! Development only: with `EXTEND_HOSTED_ALLOW_SIMULATOR=1`, a Simulator UDID is accepted in place
//! of a physical device, so the command path can be exercised without an iPhone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use extend_driver::{Driver, Invocation, LocalFile, Output, Probe};
use extend_protocol::capability::{RESERVED_FLAGS, not_exposed};
use extend_protocol::model::{CommandError, FileKind, MissingCapability, Setup, StepStatus};
use extend_protocol::{DeviceOs, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::common::{
    content_type_for, invalid, load_json, resolve_attachment, save_json, step, step_error,
    step_help,
};
use crate::{Found, HostedDevice};

const STATE_FILE: &str = "ios.json";
pub(crate) const SIMULATOR_ENV: &str = "EXTEND_HOSTED_ALLOW_SIMULATOR";
const DEFAULT_RUNNER_BUNDLE: &str = "com.callstack.agentdevice.runner";
const DEVICECTL_TIMEOUT: Duration = Duration::from_secs(20);
const PREPARE_TIMEOUT_MS: u64 = 600_000;

pub(crate) fn driver(device: HostedDevice) -> Result<Box<dyn Driver>, String> {
    if !cfg!(target_os = "macos") {
        return Err("iPhone and iPad are carried by a Mac; this computer isn't a Mac".into());
    }
    if device.agent_device.is_empty() {
        return Err("agent-device isn't bundled with this Extend app, so it can't operate an iPhone; reinstall the Extend app for Mac".into());
    }
    Ok(Box::new(IosDriver::new(device)))
}

pub(crate) fn simulators_allowed() -> bool {
    std::env::var(SIMULATOR_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

// ───────────── devicectl / simctl ─────────────

/// One device CoreDevice knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreDevice {
    pub udid: String,
    pub identifier: String,
    pub name: String,
    /// `iPhone`, `iPad`, `appleTV`, …
    pub device_type: String,
    pub product_type: Option<String>,
    pub os_version: Option<String>,
    pub paired: bool,
    pub simulated: bool,
    /// `wired`, `localNetwork`, …
    pub transport: Option<String>,
}

pub(crate) fn parse_devicectl_list(v: &Value) -> Vec<CoreDevice> {
    let devices = v
        .pointer("/result/devices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    devices
        .iter()
        .filter_map(|d| {
            let s = |p: &str| d.pointer(p).and_then(Value::as_str).map(str::to_owned);
            let udid = s("/hardwareProperties/udid").or_else(|| s("/properties/hardware/udid"))?;
            Some(CoreDevice {
                identifier: s("/identifier").unwrap_or_else(|| udid.clone()),
                name: s("/deviceProperties/name")
                    .or_else(|| s("/properties/state/name"))
                    .unwrap_or_else(|| udid.clone()),
                device_type: s("/hardwareProperties/deviceType")
                    .or_else(|| s("/properties/hardware/deviceType"))
                    .unwrap_or_default(),
                product_type: s("/hardwareProperties/productType")
                    .or_else(|| s("/properties/hardware/productType")),
                os_version: s("/deviceProperties/osVersionNumber"),
                paired: s("/connectionProperties/pairingState")
                    .or_else(|| s("/properties/connection/pairingState"))
                    .as_deref()
                    == Some("paired"),
                simulated: s("/hardwareProperties/reality")
                    .or_else(|| s("/properties/hardware/reality"))
                    .as_deref()
                    == Some("simulated"),
                transport: s("/connectionProperties/transportType")
                    .or_else(|| s("/properties/connection/transportType")),
                udid,
            })
        })
        .collect()
}

/// `Some(true)` when `device info details` says Developer Mode is on.
pub(crate) fn parse_developer_mode(v: &Value) -> Option<bool> {
    match v
        .pointer("/result/deviceProperties/developerModeStatus")
        .and_then(Value::as_str)
    {
        Some("enabled") => Some(true),
        Some(_) => Some(false),
        None => None,
    }
}

/// Whether `device info apps` lists agent-device's runner.
pub(crate) fn runner_installed(v: &Value, bundle_prefix: &str) -> bool {
    v.pointer("/result/apps")
        .and_then(Value::as_array)
        .is_some_and(|apps| {
            apps.iter().any(|a| {
                a.get("bundleIdentifier")
                    .and_then(Value::as_str)
                    .is_some_and(|b| b.starts_with(bundle_prefix))
            })
        })
}

/// The most specific human message in a devicectl error (it nests underlying errors), with the
/// recovery suggestion when there is one ("Unlock the device and try again.").
pub(crate) fn devicectl_error(v: &Value) -> Option<String> {
    let mut cur = v.get("error")?;
    let mut message = None;
    let mut suggestion = None;
    loop {
        let info = cur.get("userInfo");
        if let Some(m) = info
            .and_then(|i| i.pointer("/NSLocalizedDescription/string"))
            .and_then(Value::as_str)
        {
            message = Some(m.to_owned());
        }
        if let Some(s) = info
            .and_then(|i| i.pointer("/NSLocalizedRecoverySuggestion/string"))
            .and_then(Value::as_str)
        {
            suggestion = Some(s.to_owned());
        }
        match info.and_then(|i| i.pointer("/NSUnderlyingError/error")) {
            Some(next) => cur = next,
            None => break,
        }
    }
    match (message, suggestion) {
        (Some(m), Some(s)) => Some(format!("{m} {s}")),
        (m, s) => m.or(s),
    }
}

/// Runs `xcrun devicectl <args> --json-output <tmp>` and returns the JSON (also on failure, when
/// devicectl wrote its error there).
async fn devicectl(args: &[&str]) -> Result<Value, String> {
    let out = std::env::temp_dir().join(format!(
        "extend-devicectl-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let mut cmd = Command::new("xcrun");
    cmd.arg("devicectl")
        .args(args)
        .arg("--json-output")
        .arg(&out);
    cmd.stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let result =
        tokio::time::timeout(DEVICECTL_TIMEOUT + Duration::from_secs(5), cmd.output()).await;
    let json = std::fs::read(&out)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let _ = std::fs::remove_file(&out);
    match (result, json) {
        (Err(_), _) => Err("devicectl didn't answer in time".into()),
        (Ok(Err(e)), _) => Err(format!(
            "couldn't run xcrun devicectl: {e} (is Xcode installed?)"
        )),
        (Ok(Ok(o)), Some(v)) if o.status.success() => Ok(v),
        (Ok(Ok(_)), Some(v)) => {
            Err(devicectl_error(&v).unwrap_or_else(|| "devicectl failed".into()))
        }
        (Ok(Ok(o)), None) => Err(String::from_utf8_lossy(&o.stderr)
            .lines()
            .last()
            .unwrap_or("devicectl failed")
            .to_owned()),
    }
}

async fn list_core_devices() -> Result<Vec<CoreDevice>, String> {
    Ok(parse_devicectl_list(
        &devicectl(&["list", "devices", "--timeout", "10"]).await?,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Simulator {
    pub udid: String,
    pub name: String,
    pub runtime: String,
    pub booted: bool,
}

pub(crate) fn parse_simctl(v: &Value) -> Vec<Simulator> {
    let mut out = Vec::new();
    if let Some(map) = v.get("devices").and_then(Value::as_object) {
        for (runtime, list) in map {
            // com.apple.CoreSimulator.SimRuntime.iOS-18-4 → iOS 18.4
            let pretty = runtime
                .rsplit('.')
                .next()
                .unwrap_or(runtime)
                .replacen('-', " ", 1)
                .replace('-', ".");
            for d in list.as_array().into_iter().flatten() {
                if d.get("isAvailable").and_then(Value::as_bool) == Some(false) {
                    continue;
                }
                let (Some(udid), Some(name)) = (
                    d.get("udid").and_then(Value::as_str),
                    d.get("name").and_then(Value::as_str),
                ) else {
                    continue;
                };
                out.push(Simulator {
                    udid: udid.into(),
                    name: name.into(),
                    runtime: pretty.clone(),
                    booted: d.get("state").and_then(Value::as_str) == Some("Booted"),
                });
            }
        }
    }
    out
}

async fn simulators() -> Vec<Simulator> {
    let out = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available", "-j"])
        .kill_on_drop(true)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout)
            .map(|v| parse_simctl(&v))
            .unwrap_or_default(),
        _ => vec![],
    }
}

fn is_kind(os: DeviceOs, device_type: &str) -> bool {
    match os {
        DeviceOs::Ipados => device_type.eq_ignore_ascii_case("iPad"),
        _ => device_type.eq_ignore_ascii_case("iPhone") || device_type.eq_ignore_ascii_case("iPod"),
    }
}

/// iPhones (or iPads) this Mac knows, for `discover`.
pub(crate) async fn attached_devices(os: DeviceOs) -> Vec<Found> {
    if !cfg!(target_os = "macos") {
        return vec![];
    }
    let mut found: Vec<Found> = list_core_devices()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|d| !d.simulated && is_kind(os, &d.device_type))
        .map(|d| Found {
            name: d.name,
            address: d.udid,
            model: d.product_type,
        })
        .collect();
    if simulators_allowed() {
        let want_ipad = os == DeviceOs::Ipados;
        found.extend(
            simulators()
                .await
                .into_iter()
                .filter(|s| s.name.starts_with("iPad") == want_ipad && s.runtime.starts_with("iOS"))
                .map(|s| Found {
                    name: format!("{} (Simulator, {})", s.name, s.runtime),
                    address: s.udid,
                    model: None,
                }),
        );
    }
    found
}

// ───────────── Arguments and files ─────────────

/// What to run and which files to collect afterwards.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub args: Vec<String>,
    pub expect: Vec<(PathBuf, FileKind)>,
    /// `record start` writes here; `record stop` hands it back.
    pub record_to: Option<PathBuf>,
    pub record_stop: bool,
}

fn flag_name(a: &str) -> &str {
    a.split_once('=').map(|(f, _)| f).unwrap_or(a)
}

/// A file name the Silicon chose, reduced to a safe basename with the given extension.
pub(crate) fn safe_name(requested: Option<&str>, default: &str, ext: &str) -> String {
    let base = requested
        .and_then(|r| {
            Path::new(r)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .map(|n| {
            n.chars()
                .map(|c| {
                    if c.is_alphanumeric() || "-_. ".contains(c) {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        })
        .map(|n| n.trim_start_matches('.').trim().to_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| default.to_owned());
    if Path::new(&base)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// Index of the first positional argument, skipping the values of flags that take one.
fn first_positional(args: &[String], start: usize, value_flags: &[&str]) -> Option<usize> {
    let mut i = start;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with('-') && a.len() > 1 && a.parse::<f64>().is_err() {
            if value_flags.contains(&a.as_str()) {
                i += 1;
            }
        } else {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Rewrites a command's arguments so every file it writes lands in `workdir` (or `record_dir` for
/// recordings, which span two commands), and reads files the Silicon sent from `attachments`.
pub(crate) fn plan(
    command: &str,
    args: &[String],
    attachments: &[PathBuf],
    workdir: &Path,
    record_dir: &Path,
) -> Result<Plan, String> {
    for a in args {
        let f = flag_name(a);
        if RESERVED_FLAGS.contains(&f) {
            return Err(format!(
                "{f} is chosen by Extend for this device; leave it out"
            ));
        }
    }
    if let Some(replacement) = not_exposed(command) {
        return Err(match replacement {
            Some(r) => format!("`{command}` isn't available through Extend; use `{r}`"),
            None => format!("`{command}` isn't available through Extend"),
        });
    }
    let mut args = args.to_vec();
    let mut expect = Vec::new();
    let mut record_to = None;
    let mut record_stop = false;
    let attach = |a: &str| -> Result<String, String> {
        resolve_attachment(a, attachments)
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{a} wasn't sent with the command"))
    };
    let set_flag_path = |args: &mut Vec<String>, flag: &str, path: &Path| {
        let p = path.to_string_lossy().into_owned();
        if let Some(i) = args.iter().position(|a| a == flag) {
            if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                args[i + 1] = p;
            } else {
                args.insert(i + 1, p);
            }
        } else if let Some(i) = args.iter().position(|a| a.starts_with(&format!("{flag}="))) {
            args[i] = format!("{flag}={p}");
        } else {
            args.push(flag.to_owned());
            args.push(p);
        }
    };
    match command {
        "screenshot" => {
            let at = first_positional(&args, 0, &["--scale", "--crop-on"]);
            let name = safe_name(at.map(|i| args[i].as_str()), "screenshot", "png");
            let path = workdir.join(&name);
            match at {
                Some(i) => args[i] = path.to_string_lossy().into_owned(),
                None => args.insert(0, path.to_string_lossy().into_owned()),
            }
            expect.push((path, FileKind::Screenshot));
        }
        "diff" if args.first().map(String::as_str) == Some("screenshot") => {
            if let Some(i) = args.iter().position(|a| a == "--baseline") {
                let v = args.get(i + 1).ok_or("--baseline needs a file")?.clone();
                args[i + 1] = attach(&v)?;
            }
            if let Some(i) = first_positional(&args, 1, &["--baseline", "--out", "--threshold"]) {
                args[i] = attach(&args[i].clone())?;
            }
            let requested = args
                .iter()
                .position(|a| a == "--out")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let out = workdir.join(safe_name(requested.as_deref(), "diff", "png"));
            set_flag_path(&mut args, "--out", &out);
            expect.push((out, FileKind::Diff));
        }
        "record" => match args.first().map(String::as_str) {
            Some("start") => {
                let at = first_positional(&args, 1, &["--scope", "--fps", "--quality"]);
                let name = safe_name(at.map(|i| args[i].as_str()), "recording", "mp4");
                let path = record_dir.join(name);
                match at {
                    Some(i) => args[i] = path.to_string_lossy().into_owned(),
                    None => args.insert(1, path.to_string_lossy().into_owned()),
                }
                record_to = Some(path);
            }
            Some("stop") => record_stop = true,
            _ => {}
        },
        "open" | "close" if args.iter().any(|a| flag_name(a) == "--save-script") => {
            let i = args
                .iter()
                .position(|a| flag_name(a) == "--save-script")
                .expect("checked above");
            let requested = match args[i].split_once('=') {
                Some((_, v)) => Some(v.to_owned()),
                None => args.get(i + 1).filter(|v| v.ends_with(".ad")).cloned(),
            };
            let name = safe_name(requested.as_deref(), "session", "ad");
            let path = if command == "open" {
                record_dir.join(name)
            } else {
                workdir.join(name)
            };
            let p = path.to_string_lossy().into_owned();
            match (args[i].contains('='), requested.is_some()) {
                (true, _) => args[i] = format!("--save-script={p}"),
                (false, true) => args[i + 1] = p,
                (false, false) => args.insert(i + 1, p),
            }
            if command == "close" {
                expect.push((path, FileKind::ReplayScript));
            }
        }
        "replay" => {
            if let Some(i) = first_positional(&args, 0, &["--from", "--plan-digest"]) {
                args[i] = attach(&args[i].clone())?;
            }
        }
        "test" => {
            let mut i = 0;
            while i < args.len() {
                if args[i].starts_with("--") {
                    if matches!(
                        args[i].as_str(),
                        "--retries" | "--timeout" | "--artifacts-dir" | "--reporter" | "--platform"
                    ) {
                        i += 1;
                    }
                } else if let Some(p) = resolve_attachment(&args[i], attachments) {
                    args[i] = p.to_string_lossy().into_owned();
                }
                i += 1;
            }
            // Suite artifacts land in the workdir and come back with the result.
            if !args.iter().any(|a| a == "--artifacts-dir") {
                args.push("--artifacts-dir".into());
                args.push(
                    workdir
                        .join("test-artifacts")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        "batch" => {
            if let Some(i) = args.iter().position(|a| a == "--steps-file") {
                let v = args.get(i + 1).ok_or("--steps-file needs a path")?.clone();
                args[i + 1] = attach(&v)?;
            }
        }
        _ => {}
    }
    Ok(Plan {
        args,
        expect,
        record_to,
        record_stop,
    })
}

/// Who holds a device agent-device refused with `DEVICE_IN_USE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleOwner {
    pub session: String,
    pub state_dir: Option<String>,
    pub workspace: Option<String>,
}

/// A `DEVICE_IN_USE` refusal whose owner is another Extend session (never our own, never a
/// session someone started outside Extend).
pub(crate) fn stale_owner(out: &Output, ours: &str) -> Option<StaleOwner> {
    let details = &out.error.as_ref()?.details;
    if details.get("agent_device_code").and_then(Value::as_str) != Some("DEVICE_IN_USE") {
        return None;
    }
    let owner = details.pointer("/error/details/owner")?;
    let session = owner.get("session").and_then(Value::as_str)?;
    if !session.starts_with("extend-") || session == ours || session == "extend-setup" {
        return None;
    }
    Some(StaleOwner {
        session: session.to_owned(),
        state_dir: owner
            .get("stateDir")
            .and_then(Value::as_str)
            .map(str::to_owned),
        workspace: owner
            .get("workspace")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn session_name(session_id: &str) -> String {
    let clean: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    format!("extend-{clean}")
}

// ───────────── Results ─────────────

/// Maps agent-device's error codes onto Extend's.
pub(crate) fn map_error_code(code: &str) -> String {
    match code {
        "INVALID_ARGS" | "INVALID_ARGUMENT" | "USAGE" => crate::common::INVALID_ARGS.to_owned(),
        "UNSUPPORTED_OPERATION" | "UNSUPPORTED_PLATFORM" | "NOT_SUPPORTED" => {
            ErrorCode::UnsupportedOnDevice.as_str()
        }
        "DEVICE_NOT_FOUND" | "DEVICE_OFFLINE" | "DEVICE_UNAVAILABLE" => {
            ErrorCode::DeviceOffline.as_str()
        }
        "TIMEOUT" | "COMMAND_TIMEOUT" => ErrorCode::CommandTimeout.as_str(),
        "UNKNOWN_COMMAND" => ErrorCode::UnknownCommand.as_str(),
        _ => ErrorCode::CommandFailed.as_str(),
    }
}

/// The JSON document agent-device printed (the last top-level object in stdout).
pub(crate) fn parse_stdout(stdout: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
        return Some(v);
    }
    let mut last = None;
    for (i, _) in stdout.match_indices("\n{") {
        if let Ok(v) = serde_json::from_str::<Value>(stdout[i + 1..].trim()) {
            last = Some(v);
        }
    }
    last.or_else(|| {
        stdout
            .find('{')
            .and_then(|i| serde_json::from_str(stdout[i..].trim()).ok())
    })
}

fn role_name(t: &str) -> String {
    match t {
        "SearchField" => return "search".into(),
        "StaticText" => return "text".into(),
        "Other" => return "other".into(),
        _ => {}
    }
    let t = t
        .strip_suffix("View")
        .filter(|s| !s.is_empty())
        .unwrap_or(t);
    let mut out = String::new();
    for (i, c) in t.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// A readable rendering of a snapshot, close to agent-device's own CLI text
/// (`@e2 [button] "Continue"`).
pub(crate) fn render_snapshot(data: &Value) -> String {
    let nodes = data
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    if let Some(app) = data
        .get("appBundleId")
        .or_else(|| data.get("appName"))
        .and_then(Value::as_str)
    {
        lines.push(format!("App: {app}"));
    }
    let truncated = data.get("truncated").and_then(Value::as_bool) == Some(true);
    lines.push(format!(
        "Snapshot: {} nodes{}",
        nodes.len(),
        if truncated { " (truncated)" } else { "" }
    ));
    for n in &nodes {
        let s = |k: &str| n.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        let mut line = String::new();
        if let Some(r) = s("ref") {
            line.push_str(&format!("@{r} "));
        }
        line.push_str(&format!("[{}]", role_name(s("type").unwrap_or("other"))));
        if let Some(label) = s("label").or(s("value")).or(s("identifier")) {
            line.push_str(&format!(" \"{}\"", label.replace('"', "\\\"")));
        }
        if matches!(
            s("type"),
            Some("TextField" | "SecureTextField" | "SearchField" | "TextView")
        ) {
            line.push_str(" [editable]");
        }
        if n.get("enabled").and_then(Value::as_bool) == Some(false) {
            line.push_str(" [disabled]");
        }
        if n.get("selected").and_then(Value::as_bool) == Some(true) {
            line.push_str(" [selected]");
        }
        lines.push(line);
    }
    if let Some(reasons) = data
        .pointer("/visibility/reasons")
        .and_then(Value::as_array)
        && reasons.iter().any(|r| r == "scroll-hidden-below")
    {
        lines.push("[more content below; scroll down]".into());
    }
    lines.join("\n")
}

fn render_text(command: &str, data: &Value) -> String {
    if command == "snapshot" && data.get("nodes").is_some() {
        return render_snapshot(data);
    }
    if command == "apps"
        && let Some(apps) = data.get("apps").and_then(Value::as_array)
    {
        return apps
            .iter()
            .map(|a| {
                match (
                    a.get("name").and_then(Value::as_str),
                    a.get("bundleId").or(a.get("id")).and_then(Value::as_str),
                ) {
                    (Some(n), Some(id)) => format!("{n}  {id}"),
                    (Some(n), None) => n.to_owned(),
                    (None, Some(id)) => id.to_owned(),
                    _ => a.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if command == "screenshot"
        && let Some(path) = data.get("path").and_then(Value::as_str)
    {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return match (
            data.get("width").and_then(Value::as_u64),
            data.get("height").and_then(Value::as_u64),
        ) {
            (Some(w), Some(h)) => format!("Screenshot {name} ({w}x{h})"),
            _ => format!("Screenshot {name}"),
        };
    }
    for k in ["message", "text", "value"] {
        if let Some(m) = data.get(k).and_then(Value::as_str) {
            return m.to_owned();
        }
    }
    serde_json::to_string(data).unwrap_or_default()
}

pub(crate) fn to_output(command: &str, stdout: &str, stderr: &str, exit_ok: bool) -> Output {
    match parse_stdout(stdout) {
        Some(v) if v.get("success").and_then(Value::as_bool) == Some(true) => {
            let data = v.get("data").cloned().unwrap_or(Value::Null);
            Output {
                ok: true,
                text: Some(render_text(command, &data)),
                output: data,
                error: None,
                files: vec![],
            }
        }
        Some(v) if v.get("success").and_then(Value::as_bool) == Some(false) => {
            let err = v.get("error").cloned().unwrap_or(Value::Null);
            let ad_code = err
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("COMMAND_FAILED");
            let mut message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("agent-device failed")
                .to_owned();
            if let Some(h) = err.get("hint").and_then(Value::as_str) {
                message.push_str(&format!("\nHint: {h}"));
            }
            Output {
                ok: false,
                output: Value::Null,
                text: Some(message.clone()),
                error: Some(CommandError {
                    code: map_error_code(ad_code),
                    message,
                    details: json!({"agent_device_code": ad_code, "reason": err.pointer("/details/reason"), "error": err}),
                }),
                files: vec![],
            }
        }
        _ if exit_ok => Output::ok(json!({"stdout": stdout}), stdout.trim()),
        _ => {
            let tail: String = stderr
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            crate::common::failed(if tail.is_empty() {
                "agent-device failed without output".into()
            } else {
                tail
            })
        }
    }
}

// ───────────── Driver ─────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    udid: Option<String>,
}

#[derive(Debug, Clone)]
enum Prepare {
    Idle,
    Running,
    Done,
    Failed(String),
}

struct Inner {
    device: HostedDevice,
    saved: Mutex<Saved>,
    prepare: Mutex<Prepare>,
    /// Recording path per session, from `record start` to `record stop`.
    recordings: Mutex<HashMap<String, PathBuf>>,
    version: Mutex<Option<String>>,
    runner_seen: Mutex<Option<Instant>>,
}

pub struct IosDriver {
    inner: Arc<Inner>,
}

impl IosDriver {
    pub(crate) fn new(device: HostedDevice) -> Self {
        let saved = load_json(&device.state_dir, STATE_FILE).unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                device,
                saved: Mutex::new(saved),
                prepare: Mutex::new(Prepare::Idle),
                recordings: Mutex::new(HashMap::new()),
                version: Mutex::new(None),
                runner_seen: Mutex::new(None),
            }),
        }
    }
}

impl Inner {
    fn kind(&self) -> &'static str {
        if self.device.os == DeviceOs::Ipados {
            "iPad"
        } else {
            "iPhone"
        }
    }

    fn udid(&self) -> Option<String> {
        self.device
            .address
            .clone()
            .filter(|a| !a.trim().is_empty())
            .or_else(|| self.saved.lock().unwrap().udid.clone())
    }

    fn record_dir(&self, session_id: &str) -> PathBuf {
        self.device
            .state_dir
            .join("recordings")
            .join(session_name(session_id))
    }

    fn base_command(&self) -> Command {
        let mut cmd = Command::new(&self.device.agent_device[0]);
        cmd.args(&self.device.agent_device[1..]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    async fn agent_device_version(&self) -> Option<String> {
        if let Some(v) = self.version.lock().unwrap().clone() {
            return Some(v);
        }
        let mut cmd = self.base_command();
        cmd.arg("--version");
        let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
            .await
            .ok()?
            .ok()?;
        let v = String::from_utf8_lossy(&out.stdout)
            .trim()
            .lines()
            .last()?
            .trim()
            .to_owned();
        (!v.is_empty() && out.status.success()).then(|| {
            *self.version.lock().unwrap() = Some(v.clone());
            v
        })
    }

    /// Runs agent-device against this device. Returns (stdout, stderr, success) or why it couldn't run.
    async fn agent_device(
        &self,
        udid: &str,
        session: &str,
        command: &str,
        args: &[String],
        inv: Option<&Invocation<'_>>,
    ) -> Result<(String, String, bool), String> {
        let mut cmd = self.base_command();
        cmd.arg(command).args(args).args([
            "--platform",
            "ios",
            "--udid",
            udid,
            "--json",
            "--session",
            session,
        ]);
        let child = cmd.spawn().map_err(|e| {
            format!(
                "couldn't start agent-device ({}): {e}",
                self.device.agent_device[0]
            )
        })?;
        let wait = child.wait_with_output();
        let out = match inv {
            Some(inv) => {
                tokio::select! {
                    r = wait => r,
                    _ = inv.cancel.cancelled() => return Err("cancelled".into()),
                    _ = tokio::time::sleep(inv.timeout) => return Err(format!("agent-device didn't finish within {} ms", inv.timeout.as_millis())),
                }
            }
            None => wait.await,
        }
        .map_err(|e| e.to_string())?;
        Ok((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        ))
    }

    /// Closes an abandoned agent-device session that still claims this device.
    async fn close_stale(&self, udid: &str, owner: &StaleOwner) {
        tracing::info!(session = %owner.session, "closing an abandoned agent-device session on this device");
        let mut cmd = self.base_command();
        cmd.args([
            "close",
            "--platform",
            "ios",
            "--udid",
            udid,
            "--json",
            "--session",
            &owner.session,
        ]);
        if let Some(dir) = &owner.state_dir {
            cmd.env("AGENT_DEVICE_STATE_DIR", dir);
        }
        if let Some(ws) = owner.workspace.as_ref().filter(|w| Path::new(w).is_dir()) {
            cmd.current_dir(ws);
        }
        let _ = tokio::time::timeout(Duration::from_secs(60), cmd.output()).await;
    }

    fn start_prepare(self: &Arc<Self>, udid: String) {
        {
            let mut p = self.prepare.lock().unwrap();
            if matches!(*p, Prepare::Running) {
                return;
            }
            *p = Prepare::Running;
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let args = vec![
                "ios-runner".to_owned(),
                "--timeout".to_owned(),
                PREPARE_TIMEOUT_MS.to_string(),
            ];
            let result = me
                .agent_device(&udid, "extend-setup", "prepare", &args, None)
                .await;
            let next = match result {
                Ok((stdout, stderr, ok)) => {
                    let out = to_output("prepare", &stdout, &stderr, ok);
                    if out.ok {
                        *me.runner_seen.lock().unwrap() = Some(Instant::now());
                        Prepare::Done
                    } else {
                        Prepare::Failed(
                            out.error
                                .map(|e| e.message)
                                .unwrap_or_else(|| "prepare failed".into()),
                        )
                    }
                }
                Err(e) => Prepare::Failed(e),
            };
            *me.prepare.lock().unwrap() = next;
        });
    }

    async fn probe_physical(
        self: &Arc<Self>,
    ) -> (
        bool,
        Option<String>,
        Option<String>,
        Vec<extend_protocol::model::SetupStep>,
    ) {
        let kind = self.kind();
        let connect_title = format!("Plug the {kind} into this Mac and tap Trust");
        let devmode_title = "Turn on Developer Mode (Settings › Privacy & Security)";
        let helper_title = "Extend helper installed";
        let devices = list_core_devices().await;
        let devices = match devices {
            Ok(d) => d,
            Err(e) => {
                return (
                    false,
                    None,
                    None,
                    vec![
                        step_error(
                            "connect",
                            &connect_title,
                            StepStatus::Failed,
                            Some("Install Xcode on this Mac and open it once."),
                            e,
                        ),
                        step("developer_mode", devmode_title, StepStatus::Todo),
                        step("helper", helper_title, StepStatus::Todo),
                    ],
                );
            }
        };
        let mine: Vec<&CoreDevice> = devices
            .iter()
            .filter(|d| !d.simulated && is_kind(self.device.os, &d.device_type))
            .collect();
        let found = match self.udid() {
            Some(u) => mine
                .iter()
                .find(|d| d.udid == u || d.identifier == u)
                .copied(),
            None => {
                // Adopt the device the Carbon just plugged in: the one with this name, or the only one.
                let pick = mine
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(&self.device.name))
                    .or(if mine.len() == 1 { mine.first() } else { None })
                    .copied();
                if let Some(d) = pick {
                    let mut s = self.saved.lock().unwrap();
                    s.udid = Some(d.udid.clone());
                    let _ = save_json(&self.device.state_dir, STATE_FILE, &*s);
                }
                pick
            }
        };
        let Some(dev) = found.filter(|d| d.paired) else {
            return (
                false,
                None,
                None,
                vec![
                    step_help(
                        "connect",
                        &connect_title,
                        StepStatus::NeedsCarbon,
                        &format!(
                            "Use a cable. Unlock the {kind}, tap Trust when it asks, and enter its passcode."
                        ),
                    ),
                    step("developer_mode", devmode_title, StepStatus::Todo),
                    step("helper", helper_title, StepStatus::Todo),
                ],
            );
        };
        let udid = dev.udid.clone();
        let details = devicectl(&[
            "device",
            "info",
            "details",
            "--device",
            &udid,
            "--timeout",
            "15",
        ])
        .await;
        let (online, devmode) = match &details {
            Ok(v) => (true, parse_developer_mode(v)),
            Err(_) => (false, None),
        };
        let os_version = details
            .as_ref()
            .ok()
            .and_then(|v| {
                v.pointer("/result/deviceProperties/osVersionNumber")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or(dev.os_version.clone());
        let model = dev.product_type.clone();
        let connect = step("connect", &connect_title, StepStatus::Done);
        let devmode_step = match (devmode, &details) {
            (Some(true), _) => step("developer_mode", devmode_title, StepStatus::Done),
            (Some(false), _) => step_help(
                "developer_mode",
                devmode_title,
                StepStatus::NeedsCarbon,
                &format!(
                    "On the {kind}: Settings › Privacy & Security › Developer Mode, turn it on, and restart when asked."
                ),
            ),
            (None, Err(e)) => step_error(
                "developer_mode",
                devmode_title,
                StepStatus::Todo,
                Some(&format!(
                    "Keep the {kind} unlocked and on the same Wi-Fi as this Mac, or plugged in."
                )),
                e.clone(),
            ),
            (None, Ok(_)) => step("developer_mode", devmode_title, StepStatus::Todo),
        };
        let helper = if devmode != Some(true) {
            step("helper", helper_title, StepStatus::Todo)
        } else {
            let recently_seen = self
                .runner_seen
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() < Duration::from_secs(600));
            let installed = recently_seen || {
                let prefix = std::env::var("AGENT_DEVICE_IOS_BUNDLE_ID")
                    .unwrap_or_else(|_| DEFAULT_RUNNER_BUNDLE.into());
                let apps = devicectl(&[
                    "device",
                    "info",
                    "apps",
                    "--device",
                    &udid,
                    "--timeout",
                    "15",
                ])
                .await;
                let yes = apps.as_ref().is_ok_and(|v| runner_installed(v, &prefix));
                if yes {
                    *self.runner_seen.lock().unwrap() = Some(Instant::now());
                }
                yes
            };
            let state = self.prepare.lock().unwrap().clone();
            match (installed, state) {
                (true, _) => step("helper", helper_title, StepStatus::Done),
                (false, Prepare::Running) => step_help(
                    "helper",
                    helper_title,
                    StepStatus::InProgress,
                    &format!(
                        "Extend is putting its helper on the {kind}. Keep it unlocked and connected; the first time takes a few minutes."
                    ),
                ),
                (false, Prepare::Failed(e)) => {
                    *self.prepare.lock().unwrap() = Prepare::Idle;
                    step_error(
                        "helper",
                        helper_title,
                        StepStatus::Failed,
                        Some(
                            "The helper must be signed: set AGENT_DEVICE_IOS_TEAM_ID (and AGENT_DEVICE_IOS_BUNDLE_ID) for the Extend app, or sign in to Xcode with an Apple ID. Extend tries again shortly.",
                        ),
                        e,
                    )
                }
                (false, Prepare::Idle | Prepare::Done) => {
                    self.start_prepare(udid.clone());
                    step("helper", helper_title, StepStatus::InProgress)
                }
            }
        };
        (
            online,
            os_version,
            model,
            vec![connect, devmode_step, helper],
        )
    }
}

#[async_trait]
impl Driver for IosDriver {
    async fn probe(&self) -> Probe {
        let me = &self.inner;
        let full = me.device.os.full_capabilities();
        let version = me.agent_device_version().await;
        let udid = me.udid();
        let sims = if simulators_allowed() {
            simulators().await
        } else {
            vec![]
        };
        let sim = udid
            .as_ref()
            .and_then(|u| sims.iter().find(|s| &s.udid == u))
            .cloned();
        let (online, os_version, model, mut steps) = match sim {
            Some(s) => (
                true,
                Some(s.runtime.clone()),
                Some(format!("{} (Simulator)", s.name)),
                vec![
                    step(
                        "connect",
                        "Simulator selected (development only)",
                        StepStatus::Done,
                    ),
                    step(
                        "developer_mode",
                        "Developer Mode (not needed on a Simulator)",
                        StepStatus::Done,
                    ),
                    step(
                        "helper",
                        "Extend helper installed (agent-device installs it on first use)",
                        StepStatus::Done,
                    ),
                ],
            ),
            None => me.probe_physical().await,
        };
        if version.is_none() {
            steps.push(step_error(
                "agent_device",
                "agent-device available on this Mac",
                StepStatus::Failed,
                Some("Reinstall the Extend app for Mac; it carries agent-device."),
                format!(
                    "`{} --version` didn't run",
                    me.device.agent_device.join(" ")
                ),
            ));
        }
        let setup = Setup::from_steps(steps);
        let ready = online && setup.state == extend_protocol::model::SetupState::Complete;
        let reason = setup
            .steps
            .iter()
            .find(|s| s.status != StepStatus::Done)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| {
                format!(
                    "The {} isn't reachable; keep it unlocked and near this Mac",
                    me.kind()
                )
            });
        Probe {
            os: me.device.os,
            os_version,
            model,
            capabilities: if ready { full.to_vec() } else { vec![] },
            missing: if ready {
                vec![]
            } else {
                full.iter()
                    .map(|c| MissingCapability {
                        capability: *c,
                        reason: reason.clone(),
                    })
                    .collect()
            },
            setup,
            agent_device_version: version,
            online,
        }
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        let me = &self.inner;
        let Some(udid) = me.udid() else {
            return crate::common::not_ready(format!(
                "No {} is connected yet; plug it into this Mac and tap Trust",
                me.kind()
            ));
        };
        let record_dir = me.record_dir(inv.session_id);
        let plan = match plan(
            inv.command,
            inv.args,
            inv.attachments,
            inv.workdir,
            &record_dir,
        ) {
            Ok(p) => p,
            Err(e) if e.contains("isn't available through Extend") => {
                return Output::fail(ErrorCode::UnknownCommand.as_str().as_str(), e);
            }
            Err(e) => return invalid(e),
        };
        if plan.record_to.is_some() || inv.args.iter().any(|a| flag_name(a) == "--save-script") {
            let _ = std::fs::create_dir_all(&record_dir);
        }
        let _ = std::fs::create_dir_all(inv.workdir);
        let before = list_files(inv.workdir);
        let session = session_name(inv.session_id);
        let mut out = Output::fail(ErrorCode::CommandFailed.as_str().as_str(), "not run");
        for attempt in 0..2 {
            let (stdout, stderr, ok) = match me
                .agent_device(&udid, &session, inv.command, &plan.args, Some(&inv))
                .await
            {
                Ok(r) => r,
                Err(e) if e == "cancelled" => {
                    return crate::common::failed(format!("`{}` was cancelled", inv.command));
                }
                Err(e) if e.contains("didn't finish") => {
                    return Output::fail(ErrorCode::CommandTimeout.as_str().as_str(), e);
                }
                Err(e) => return crate::common::failed(e),
            };
            out = to_output(inv.command, &stdout, &stderr, ok);
            // Extend lets one session use a device at a time, so another `extend-…` agent-device
            // session still holding it was left behind (a crash, a lost session end): close it.
            match stale_owner(&out, &session) {
                Some(owner) if attempt == 0 => me.close_stale(&udid, &owner).await,
                _ => break,
            }
        }

        if out.ok
            && let Some(path) = &plan.record_to
        {
            me.recordings
                .lock()
                .unwrap()
                .insert(inv.session_id.to_owned(), path.clone());
        }
        if plan.record_stop
            && let Some(src) = me.recordings.lock().unwrap().remove(inv.session_id)
        {
            move_recording(&src, inv.workdir);
        }
        let mut listed: Vec<PathBuf> = Vec::new();
        for (path, kind) in &plan.expect {
            if path.is_file() {
                listed.push(path.clone());
                out.files.push(local_file(path, *kind));
            }
        }
        // Anything else the command left in the workdir (recordings, overlays, suite artifacts).
        for path in list_files(inv.workdir) {
            if before.contains(&path) || listed.contains(&path) || inv.attachments.contains(&path) {
                continue;
            }
            let kind = match path.extension().and_then(|e| e.to_str()) {
                Some("mp4" | "mov") => FileKind::Recording,
                Some("ad") => FileKind::ReplayScript,
                Some("log") => FileKind::Log,
                _ => FileKind::Other,
            };
            out.files.push(local_file(&path, kind));
        }
        out
    }

    async fn session_ended(&self, session_id: &str) {
        let me = &self.inner;
        let Some(udid) = me.udid() else { return };
        let session = session_name(session_id);
        if me.recordings.lock().unwrap().remove(session_id).is_some() {
            let _ = me
                .agent_device(&udid, &session, "record", &["stop".to_owned()], None)
                .await;
        }
        let _ = me.agent_device(&udid, &session, "close", &[], None).await;
        let _ = std::fs::remove_dir_all(me.record_dir(session_id));
    }
}

fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

/// Moves a finished recording (and its sidecars, same stem) into the workdir.
fn move_recording(src: &Path, workdir: &Path) {
    let Some(stem) = src.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return;
    };
    let Some(dir) = src.parent() else { return };
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(&stem))
        {
            let dest = workdir.join(p.file_name().unwrap());
            if std::fs::rename(&p, &dest).is_err() {
                let _ = std::fs::copy(&p, &dest).map(|_| std::fs::remove_file(&p));
            }
        }
    }
}

fn local_file(path: &Path, kind: FileKind) -> LocalFile {
    LocalFile {
        path: path.to_path_buf(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        content_type: content_type_for(path).into(),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_devicectl_output() {
        let list = parse_devicectl_list(&fixture("devicectl-list.json"));
        assert_eq!(list.len(), 2);
        let phone = &list[0];
        assert_eq!(phone.udid, "00008110-000A1B2C3D4E5F60");
        assert_eq!(phone.name, "Test iPhone");
        assert_eq!(phone.device_type, "iPhone");
        assert_eq!(phone.product_type.as_deref(), Some("iPhone14,4"));
        assert!(phone.paired && !phone.simulated);
        assert_eq!(phone.transport.as_deref(), Some("localNetwork"));
        assert!(list[1].simulated);

        assert_eq!(
            parse_developer_mode(&fixture("devicectl-details.json")),
            Some(true)
        );
        let locked = fixture("devicectl-apps-locked.json");
        assert!(!runner_installed(&locked, DEFAULT_RUNNER_BUNDLE));
        let msg = devicectl_error(&locked).unwrap();
        assert!(
            msg.contains("locked") && msg.contains("Unlock the device"),
            "{msg}"
        );
        let apps = json!({"result": {"apps": [{"bundleIdentifier": "com.apple.mobilesafari"}, {"bundleIdentifier": "com.callstack.agentdevice.runner.uitests.xctrunner"}]}});
        assert!(runner_installed(&apps, DEFAULT_RUNNER_BUNDLE));
    }

    #[test]
    fn parses_simctl() {
        let v = json!({"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-18-4": [
            {"udid": "50F5AD4C-4D26-445D-A2CA-9D04D9FFD768", "name": "iPhone 16e", "state": "Shutdown", "isAvailable": true},
            {"udid": "X", "name": "Broken", "state": "Shutdown", "isAvailable": false}]}});
        let sims = parse_simctl(&v);
        assert_eq!(
            sims,
            vec![Simulator {
                udid: "50F5AD4C-4D26-445D-A2CA-9D04D9FFD768".into(),
                name: "iPhone 16e".into(),
                runtime: "iOS 18.4".into(),
                booted: false
            }]
        );
    }

    #[test]
    fn plans_file_outputs() {
        let w = Path::new("/w");
        let r = Path::new("/r");
        let p = plan("screenshot", &s(&["--scale", "0.5"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["/w/screenshot.png", "--scale", "0.5"]));
        assert_eq!(
            p.expect,
            vec![(PathBuf::from("/w/screenshot.png"), FileKind::Screenshot)]
        );
        let p = plan(
            "screenshot",
            &s(&["../../etc/home", "--overlay-refs"]),
            &[],
            w,
            r,
        )
        .unwrap();
        assert_eq!(p.args, s(&["/w/home.png", "--overlay-refs"]));

        let base = vec![PathBuf::from("/att/base.png")];
        let p = plan(
            "diff",
            &s(&["screenshot", "--baseline", "base.png"]),
            &base,
            w,
            r,
        )
        .unwrap();
        assert_eq!(
            p.args,
            s(&[
                "screenshot",
                "--baseline",
                "/att/base.png",
                "--out",
                "/w/diff.png"
            ])
        );
        assert!(
            plan(
                "diff",
                &s(&["screenshot", "--baseline", "nope.png"]),
                &[],
                w,
                r
            )
            .is_err()
        );

        let p = plan("record", &s(&["start", "--fps", "30"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["start", "/r/recording.mp4", "--fps", "30"]));
        assert_eq!(p.record_to, Some(PathBuf::from("/r/recording.mp4")));
        assert!(
            plan("record", &s(&["stop"]), &[], w, r)
                .unwrap()
                .record_stop
        );

        let p = plan("close", &s(&["--save-script"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["--save-script", "/w/session.ad"]));
        let p = plan(
            "open",
            &s(&["Settings", "--save-script", "flow.ad"]),
            &[],
            w,
            r,
        )
        .unwrap();
        assert_eq!(p.args, s(&["Settings", "--save-script", "/r/flow.ad"]));

        let scripts = vec![PathBuf::from("/att/flow.ad")];
        let p = plan("replay", &s(&["flow.ad", "--keep-session"]), &scripts, w, r).unwrap();
        assert_eq!(p.args[0], "/att/flow.ad");
    }

    #[test]
    fn refuses_reserved_and_hidden_commands() {
        let w = Path::new("/w");
        assert!(
            plan("snapshot", &s(&["--udid", "x"]), &[], w, w)
                .unwrap_err()
                .contains("--udid")
        );
        assert!(
            plan("open", &s(&["Settings", "--platform=android"]), &[], w, w)
                .unwrap_err()
                .contains("--platform")
        );
        assert!(
            plan("devices", &[], &[], w, w)
                .unwrap_err()
                .contains("extend device ls")
        );
        assert!(plan("boot", &[], &[], w, w).is_err());
    }

    #[test]
    fn maps_results() {
        let ok = r#"{"success": true, "data": {"message": "Opened: com.apple.Preferences", "appBundleId": "com.apple.Preferences"}}"#;
        let o = to_output("open", ok, "", true);
        assert!(o.ok);
        assert_eq!(o.text.as_deref(), Some("Opened: com.apple.Preferences"));
        let err = r#"{"success": false, "error": {"code": "SESSION_NOT_FOUND", "message": "iOS snapshot requires an active app session", "hint": "Run open first", "details": {"reason": "ios_app_session_required"}}}"#;
        let o = to_output("snapshot", err, "", false);
        assert!(!o.ok);
        let e = o.error.unwrap();
        assert_eq!(e.code, "command_failed");
        assert!(e.message.contains("Hint: Run open first"));
        assert_eq!(e.details["reason"], "ios_app_session_required");
        let o = to_output(
            "click",
            r#"{"success":false,"error":{"code":"INVALID_ARGS","message":"bad"}}"#,
            "",
            false,
        );
        assert_eq!(o.error.unwrap().code, "invalid_args");
        let o = to_output("click", "", "boom\n", false);
        assert_eq!(o.text.as_deref(), Some("boom"));
    }

    #[test]
    fn finds_stale_extend_sessions() {
        let busy = |owner: &str| {
            let body = json!({"success": false, "error": {"code": "DEVICE_IN_USE", "message": "owned", "details": {
                "reason": "DEVICE_CLAIM_LIVE_OWNER", "owner": {"session": owner, "stateDir": "/s", "workspace": "/w"}}}});
            to_output("open", &body.to_string(), "", false)
        };
        assert_eq!(
            stale_owner(&busy("extend-old"), "extend-new"),
            Some(StaleOwner {
                session: "extend-old".into(),
                state_dir: Some("/s".into()),
                workspace: Some("/w".into())
            })
        );
        assert_eq!(stale_owner(&busy("extend-new"), "extend-new"), None);
        assert_eq!(stale_owner(&busy("someone-else"), "extend-new"), None);
        assert_eq!(
            stale_owner(
                &to_output("open", r#"{"success":true,"data":{}}"#, "", true),
                "extend-new"
            ),
            None
        );
    }

    #[test]
    fn screenshot_text() {
        let data = json!({"path": "/w/general.png", "width": 390, "height": 844});
        assert_eq!(
            render_text("screenshot", &data),
            "Screenshot general.png (390x844)"
        );
    }

    #[test]
    fn renders_snapshots() {
        let data = json!({"appBundleId": "com.apple.Preferences", "truncated": false, "nodes": [
            {"type": "NavigationBar", "identifier": "Settings", "ref": "e2", "enabled": true},
            {"type": "SearchField", "label": "Search", "ref": "e3", "enabled": true},
            {"type": "Cell", "label": "General", "ref": "e9", "enabled": true},
            {"type": "CollectionView", "ref": "e6"}],
            "visibility": {"reasons": ["scroll-hidden-below"]}});
        assert_eq!(
            render_snapshot(&data),
            "App: com.apple.Preferences\nSnapshot: 4 nodes\n@e2 [navigation-bar] \"Settings\"\n@e3 [search] \"Search\" [editable]\n@e9 [cell] \"General\"\n@e6 [collection]\n[more content below; scroll down]"
        );
    }

    /// Drives a real iOS Simulator through agent-device: open Settings, snapshot, click, screenshot.
    /// Run with:
    /// `EXTEND_HOSTED_ALLOW_SIMULATOR=1 EXTEND_HOSTED_SIM_UDID=<udid> cargo test -p extend-hosted -- --ignored --nocapture simulator`
    /// (agent-device must be built in `vendor/agent-device`; `EXTEND_HOSTED_AGENT_DEVICE` overrides
    /// the command, space-separated).
    #[tokio::test]
    #[ignore = "needs an iOS Simulator and a built agent-device"]
    async fn simulator_end_to_end() {
        use extend_driver::cancel::CancelToken;
        let udid = std::env::var("EXTEND_HOSTED_SIM_UDID").expect("set EXTEND_HOSTED_SIM_UDID");
        assert!(simulators_allowed(), "set {SIMULATOR_ENV}=1");
        let agent_device: Vec<String> = match std::env::var("EXTEND_HOSTED_AGENT_DEVICE") {
            Ok(v) => v.split_whitespace().map(str::to_owned).collect(),
            Err(_) => vec![
                "node".into(),
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../vendor/agent-device/bin/agent-device.mjs")
                    .to_string_lossy()
                    .into_owned(),
            ],
        };
        let state = tempfile::tempdir().unwrap();
        let d = IosDriver::new(HostedDevice {
            device_id: "dev_sim".into(),
            os: DeviceOs::Ios,
            name: "Simulator".into(),
            address: Some(udid.clone()),
            state_dir: state.path().to_path_buf(),
            agent_device,
        });
        let p = d.probe().await;
        println!(
            "probe: online={} model={:?} os={:?} agent-device={:?} setup={:?}",
            p.online, p.model, p.os_version, p.agent_device_version, p.setup.state
        );
        assert!(
            p.online && p.setup.state == extend_protocol::model::SetupState::Complete,
            "{p:?}"
        );
        assert_eq!(p.capabilities, DeviceOs::Ios.full_capabilities().to_vec());

        let session = format!("sim{}", std::process::id());
        let run = |command: &'static str, args: Vec<String>, work: PathBuf| {
            let d = &d;
            let session = session.clone();
            async move {
                std::fs::create_dir_all(&work).unwrap();
                let t = Instant::now();
                let out = d
                    .run(Invocation {
                        id: uuid::Uuid::new_v4(),
                        session_id: &session,
                        command,
                        args: &args,
                        attachments: &[],
                        workdir: &work,
                        timeout: Duration::from_secs(600),
                        cancel: CancelToken::new(),
                    })
                    .await;
                println!(
                    "\n$ {command} {}  ({} ms, ok={})\n{}",
                    args.join(" "),
                    t.elapsed().as_millis(),
                    out.ok,
                    out.text.clone().unwrap_or_default()
                );
                out
            }
        };
        let work = state.path().join("work");
        let out = run(
            "open",
            s(&["com.apple.Preferences", "--relaunch"]),
            work.join("1"),
        )
        .await;
        assert!(out.ok, "{out:?}");
        let out = run("snapshot", s(&["-i"]), work.join("2")).await;
        assert!(out.ok, "{out:?}");
        let general = out.output["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["label"] == "General")
            .and_then(|n| n["ref"].as_str())
            .expect("a General row in Settings")
            .to_owned();
        let out = run("click", vec![format!("@{general}")], work.join("3")).await;
        assert!(out.ok, "{out:?}");
        let out = run("screenshot", s(&["general"]), work.join("4")).await;
        assert!(out.ok, "{out:?}");
        let file = &out.files[0];
        println!(
            "screenshot file: {} ({} bytes, {})",
            file.path.display(),
            std::fs::metadata(&file.path).unwrap().len(),
            file.content_type
        );
        assert_eq!(
            (file.name.as_str(), file.kind, file.content_type.as_str()),
            ("general.png", FileKind::Screenshot, "image/png")
        );
        assert!(std::fs::read(&file.path).unwrap().starts_with(b"\x89PNG"));
        let out = run("snapshot", s(&[]), work.join("5")).await;
        assert!(
            out.ok && out.text.unwrap_or_default().contains("About"),
            "General page should list About"
        );
        let out = run(
            "record",
            s(&["start", "clip", "--scope", "device"]),
            work.join("7"),
        )
        .await;
        assert!(out.ok, "{out:?}");
        tokio::time::sleep(Duration::from_secs(2)).await;
        let out = run("record", s(&["stop"]), work.join("8")).await;
        assert!(out.ok, "{out:?}");
        let video = out
            .files
            .iter()
            .find(|f| f.kind == FileKind::Recording)
            .expect("a recording comes back on stop");
        println!(
            "recording file: {} ({} bytes)",
            video.name,
            std::fs::metadata(&video.path).unwrap().len()
        );
        assert_eq!(video.content_type, "video/mp4");
        let out = run("devices", s(&[]), work.join("6")).await;
        assert_eq!(out.error.unwrap().code, "unknown_command");
        d.session_ended(&session).await;
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe_name(None, "screenshot", "png"), "screenshot.png");
        assert_eq!(safe_name(Some("a b.PNG"), "x", "png"), "a b.PNG");
        assert_eq!(safe_name(Some("../../.hidden"), "x", "png"), "hidden.png");
        assert_eq!(safe_name(Some("x/y/rec"), "r", "mp4"), "rec.mp4");
    }
}
