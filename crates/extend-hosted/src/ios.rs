//! iPhone and iPad, through the device engine on the Mac they are paired with.
//!
//! Every command runs as `<device-engine argv> <command> <args…> --platform ios --udid <udid> --json
//! --session extend-<session id>`. Files a command writes (screenshots, recordings, diffs, replay
//! scripts) are given explicit paths in the command's workdir so they come back as `LocalFile`s.
//!
//! Setup follows UNDERSTANDING.md: the Carbon plugs the iPhone in and taps Trust, turns on
//! Developer Mode, and Extend then puts the device engine's XCTest runner (the "helper") on it with
//! `prepare ios-runner`. Readiness comes from `xcrun devicectl`.
//!
//! While the runner runs, iOS shows "Automation Running" on the device, so the runner runs only
//! while a Silicon is working on the device: setup closes the session `prepare` ran in, every session
//! end closes the session's device-engine session and makes sure the runner is gone, and a runner no
//! command has used for a minute is stopped, inside a live session too (see "The runner" below).
//!
//! Development only: with `EXTEND_HOSTED_ALLOW_SIMULATOR=1`, a Simulator UDID is accepted in place
//! of a physical device, so the command path can be exercised without an iPhone.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use extend_driver::{Driver, Invocation, LocalFile, Output, Probe};
use extend_protocol::capability::{RESERVED_FLAGS, not_exposed};
use extend_protocol::model::{CommandError, FileKind, MissingCapability, Setup, StepStatus};
use extend_protocol::{DeviceOs, ErrorCode, OFFLINE_AFTER_S, SESSION_IDLE_S, SESSION_OFFLINE_GRACE_S, TAKEOVER_MAX_S};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::common::{
    content_type_for, invalid, load_json, resolve_attachment, save_json, step, step_failure, step_help,
};
use crate::{Found, HostedDevice};

const STATE_FILE: &str = "ios.json";
pub(crate) const SIMULATOR_ENV: &str = "EXTEND_HOSTED_ALLOW_SIMULATOR";
/// The helper's bundle id (its test runner is `<this>.uitests.xctrunner`), unless
/// `EXTEND_ENGINE_IOS_BUNDLE_ID` says otherwise.
const DEFAULT_RUNNER_BUNDLE: &str = "com.teamofsilicons.extend.helper";
/// The helper's bundle id before 1.1: removed from a device once the new helper is on it.
const OLD_RUNNER_BUNDLE: &str = "com.callstack.agentdevice.runner";
const DEVICECTL_TIMEOUT: Duration = Duration::from_secs(20);
const PREPARE_TIMEOUT_MS: u64 = 600_000;
/// The device-engine session the helper is installed in.
const SETUP_SESSION: &str = "extend-setup";
/// Bounds Extend's own `close` (the device engine stops a runner within 25 s).
const CLOSE_LIMIT: Duration = Duration::from_secs(60);
/// How long the device engine's close gets to stop an iPhone's runner before Extend stops it.
const RUNNER_EXIT_WAIT: Duration = Duration::from_secs(10);
/// How long a runner gets to exit after SIGTERM before SIGKILL.
const RUNNER_TERM_GRACE: Duration = Duration::from_secs(5);
/// A runner no command has used for this long is stopped, inside a live session too, so
/// "Automation Running" leaves the device soon after a Silicon's last action. The next command that
/// needs it starts it again (a few seconds on a Simulator, about 15 on an iPhone); the device engine keeps
/// the session's app and refs.
const RUNNER_IDLE_STOP: Duration = Duration::from_secs(60);
/// How often each driver looks after its device's runner and sessions, connected or not.
const TEND_EVERY: Duration = Duration::from_secs(20);
/// A runner process younger than this may still be starting: the device engine may be about to connect
/// to it (a runner stopped then makes the device engine build and start another, with no session left
/// to close it).
const RUNNER_SETTLE: Duration = Duration::from_secs(30);
/// A runner process older than this is past any start (the device engine waits 45 s for a runner).
const RUNNER_STARTUP_MAX: Duration = Duration::from_secs(120);
/// No live session goes this long without a command, a takeover starting or a takeover ending: the
/// service ends a session after `SESSION_IDLE_S` without a command, a takeover pauses that for up to
/// `TAKEOVER_MAX_S` (its start and end are noted like a command), and a Mac out of touch keeps it
/// for `OFFLINE_AFTER_S` + `SESSION_OFFLINE_GRACE_S` more. Five minutes spare.
const STALE_SESSION_AFTER: Duration =
    Duration::from_secs((SESSION_IDLE_S + TAKEOVER_MAX_S + SESSION_OFFLINE_GRACE_S) as u64 + OFFLINE_AFTER_S + 300);
/// A session's last use is written down at most this often.
const SESSION_NOTE_EVERY: Duration = Duration::from_secs(60);
/// How long a sighting of the helper on the device counts before `devicectl` is asked again.
const HELPER_RECHECK: Duration = Duration::from_secs(600);
/// How long a failed helper install is shown before it is tried again.
const PREPARE_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Given to the device engine, for a daemon it starts: a runner kept warm after a Simulator session's
/// close stops after 30 s (not 5 minutes), and a daemon that exits stops its runners instead of
/// handing them to the next daemon (a handed-over runner keeps running, and the banner showing,
/// for up to a day). An iPhone's runner stops at its session's close either way. A value already in
/// the environment wins.
pub(crate) const DAEMON_ENV_DEFAULTS: [(&str, &str); 2] = [
    ("EXTEND_ENGINE_IOS_RUNNER_IDLE_STOP_MS", "30000"),
    ("EXTEND_ENGINE_IOS_RUNNER_DETACH", "0"),
];

/// An engine setting from the environment: `EXTEND_ENGINE_<name>`, or 1.0's `AGENT_DEVICE_<name>`.
fn engine_setting(name: &str) -> Option<String> {
    [format!("EXTEND_ENGINE_{name}"), format!("AGENT_DEVICE_{name}")]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

pub(crate) fn driver(device: HostedDevice) -> Result<Box<dyn Driver>, String> {
    if !cfg!(target_os = "macos") {
        return Err("iPhone and iPad are carried by a Mac; this computer isn't a Mac".into());
    }
    if device.agent_device.is_empty() {
        return Err(ENGINE_MISSING.into());
    }
    Ok(Box::new(IosDriver::new(device)))
}

/// Said when the device engine can't be found or run.
const ENGINE_MISSING: &str = "Silicon Extend's device engine is missing on this Mac, so it can't use an iPhone or iPad. Reinstall Silicon Extend for Mac.";

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
                product_type: s("/hardwareProperties/productType").or_else(|| s("/properties/hardware/productType")),
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

/// Whether `device info apps` lists the helper's runner.
pub(crate) fn runner_installed(v: &Value, bundle_prefix: &str) -> bool {
    v.pointer("/result/apps").and_then(Value::as_array).is_some_and(|apps| {
        apps.iter().any(|a| {
            a.get("bundleIdentifier")
                .and_then(Value::as_str)
                .is_some_and(|b| b.starts_with(bundle_prefix))
        })
    })
}

/// The bundle ids of the helper 1.0 put on the device (`com.callstack.agentdevice.runner*`).
pub(crate) fn old_helpers(v: &Value) -> Vec<String> {
    v.pointer("/result/apps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.get("bundleIdentifier").and_then(Value::as_str))
        .filter(|b| b.starts_with(OLD_RUNNER_BUNDLE))
        .map(str::to_owned)
        .collect()
}

/// What a failed setup check or helper install means for the Carbon: one or two sentences
/// saying what is wrong and what to do. `detail` is what devicectl, Xcode or the device engine
/// said; it goes to the log, never to the Carbon.
pub(crate) fn plain_setup_error(kind: &str, detail: &str) -> String {
    let d = detail.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| d.contains(n));
    if has(&["developer mode"]) {
        format!(
            "Developer Mode is off on the {kind}. Turn it on in Settings › Privacy & Security › Developer Mode, let the {kind} restart, then tap Retry."
        )
    } else if has(&[
        "untrusted",
        "not been explicitly trusted",
        "invalid code signature",
        "could not be verified",
        "verify the app",
    ]) {
        format!(
            "The {kind} doesn't trust the helper's developer yet. On the {kind} open Settings › General › VPN & Device Management, trust the developer shown there, then tap Retry."
        )
    } else if has(&[
        "no account",
        "sign in",
        "signing certificate",
        "development team",
        "no team",
        "team id",
        "provisioning profile",
        "no profiles for",
        "code signing",
        "codesign",
    ]) {
        "Xcode on this Mac can't sign the helper: it isn't signed in with an Apple ID, or that account has no team. Open Xcode › Settings › Accounts, sign in, then tap Retry.".into()
    } else if has(&["locked", "unlock", "passcode"]) {
        format!("The {kind} is locked or not connected by cable. Unlock it and keep it plugged in.")
    } else if has(&["not paired", "pairing", "trust"]) {
        format!("The {kind} doesn't trust this Mac yet. Unlock it, tap Trust when it asks, and enter its passcode.")
    } else if has(&["xcrun", "xcode-select", "is xcode installed", "command line tools"])
        && !has(&["timed out", "timeout", "didn't answer"])
    {
        "Xcode isn't ready on this Mac. Install Xcode from the App Store, open it once to finish setting it up, then tap Retry.".into()
    } else if has(&[
        "not connected",
        "not found",
        "unable to locate",
        "no device",
        "disconnected",
        "unavailable",
        "offline",
        "timed out",
        "timeout",
        "didn't answer",
        "in time",
    ]) {
        format!(
            "The {kind} isn't reachable from this Mac. Connect it with a cable (or put it on the same Wi-Fi as this Mac) and keep it unlocked."
        )
    } else {
        format!(
            "Extend couldn't set up its helper on the {kind}. Keep the {kind} unlocked and plugged into this Mac, then tap Retry."
        )
    }
}

/// The most specific readable message in a devicectl error (it nests underlying errors), with the
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
    let out = std::env::temp_dir().join(format!("extend-devicectl-{}.json", uuid::Uuid::new_v4().simple()));
    let mut cmd = Command::new("xcrun");
    cmd.arg("devicectl").args(args).arg("--json-output").arg(&out);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    let result = tokio::time::timeout(DEVICECTL_TIMEOUT + Duration::from_secs(5), cmd.output()).await;
    let json = std::fs::read(&out)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let _ = std::fs::remove_file(&out);
    match (result, json) {
        (Err(_), _) => Err("devicectl didn't answer in time".into()),
        (Ok(Err(e)), _) => Err(format!("couldn't run xcrun devicectl: {e} (is Xcode installed?)")),
        (Ok(Ok(o)), Some(v)) if o.status.success() => Ok(v),
        (Ok(Ok(_)), Some(v)) => Err(devicectl_error(&v).unwrap_or_else(|| "devicectl failed".into())),
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
        .and_then(|r| Path::new(r).file_name().map(|n| n.to_string_lossy().into_owned()))
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

/// the device engine's name for a recording quality Extend's CLI offers (`normal` or `high`).
fn recording_quality(requested: &str) -> Result<&'static str, String> {
    match requested.trim().to_ascii_lowercase().as_str() {
        "normal" | "medium" => Ok("medium"),
        "high" => Ok("high"),
        _ => Err(format!(
            "--quality {requested:?} isn't a recording quality. Use --quality normal (the default) or --quality high, or leave it out."
        )),
    }
}

/// The command, its arguments and the flags Extend adds, with those flags ahead of a `--`: after
/// it the device engine reads every token as text, so flags appended there would be typed.
fn with_extend_flags(command: &str, args: &[String], flags: &[&str]) -> Vec<String> {
    let mut full = vec![command.to_owned()];
    let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
    full.extend_from_slice(&args[..at]);
    full.extend(flags.iter().map(|f| (*f).to_owned()));
    full.extend_from_slice(&args[at..]);
    full
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
            return Err(format!("{f} is chosen by Extend for this device; leave it out"));
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
                // `cli.yaml` offers normal (the default) and high; device-engine calls normal "medium".
                for i in 0..args.len() {
                    let (value_at, value) = if args[i] == "--quality" {
                        (i + 1, args.get(i + 1).cloned())
                    } else if let Some(v) = args[i].strip_prefix("--quality=") {
                        (i, Some(v.to_owned()))
                    } else {
                        continue;
                    };
                    let value = value.ok_or("--quality needs a value: normal or high")?;
                    let mapped = recording_quality(&value)?;
                    args[value_at] = if value_at == i {
                        format!("--quality={mapped}")
                    } else {
                        mapped.to_owned()
                    };
                }
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
                args.push(workdir.join("test-artifacts").to_string_lossy().into_owned());
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

/// Who holds a device the device engine refused with `DEVICE_IN_USE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleOwner {
    pub session: String,
    pub state_dir: Option<String>,
    pub workspace: Option<String>,
}

/// A `DEVICE_IN_USE` refusal whose owner is another Extend session (never our own, never a
/// session someone started outside Extend). That includes the setup session a helper install left
/// open when Extend stopped in the middle of it: a command holds the device's guard, so no install
/// is running when it sees the refusal.
pub(crate) fn stale_owner(out: &Output, ours: &str) -> Option<StaleOwner> {
    let details = &out.error.as_ref()?.details;
    if details.get("engine_code").and_then(Value::as_str) != Some("DEVICE_IN_USE") {
        return None;
    }
    let refusal = details.pointer("/error/details")?;
    // Another daemon's claim names its `owner`; a session of the same daemon is named directly.
    let owner = refusal.get("owner");
    let session = owner.unwrap_or(refusal).get("session").and_then(Value::as_str)?;
    if !session.starts_with("extend-") || session == ours {
        return None;
    }
    let owner_says = |k: &str| owner.and_then(|o| o.get(k)).and_then(Value::as_str).map(str::to_owned);
    Some(StaleOwner {
        session: session.to_owned(),
        state_dir: owner_says("stateDir"),
        workspace: owner_says("workspace"),
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

/// Maps the device engine's error codes onto Extend's.
pub(crate) fn map_error_code(code: &str) -> String {
    match code {
        "INVALID_ARGS" | "INVALID_ARGUMENT" | "USAGE" => crate::common::INVALID_ARGS.to_owned(),
        "UNSUPPORTED_OPERATION" | "UNSUPPORTED_PLATFORM" | "NOT_SUPPORTED" => ErrorCode::UnsupportedOnDevice.as_str(),
        "DEVICE_NOT_FOUND" | "DEVICE_OFFLINE" | "DEVICE_UNAVAILABLE" => ErrorCode::DeviceOffline.as_str(),
        "TIMEOUT" | "COMMAND_TIMEOUT" => ErrorCode::CommandTimeout.as_str(),
        "UNKNOWN_COMMAND" => ErrorCode::UnknownCommand.as_str(),
        _ => ErrorCode::CommandFailed.as_str(),
    }
}

/// The JSON document the device engine printed (the last top-level object in stdout).
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
    let t = t.strip_suffix("View").filter(|s| !s.is_empty()).unwrap_or(t);
    let mut out = String::new();
    for (i, c) in t.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// A readable rendering of a snapshot, close to the device engine's own CLI text
/// (`@e2 [button] "Continue"`).
pub(crate) fn render_snapshot(data: &Value) -> String {
    let nodes = data.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
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
    if let Some(reasons) = data.pointer("/visibility/reasons").and_then(Value::as_array)
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
            let ad_code = err.get("code").and_then(Value::as_str).unwrap_or("COMMAND_FAILED");
            let mut message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The device engine failed")
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
                    details: json!({"engine_code": ad_code, "reason": err.pointer("/details/reason"), "error": err}),
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
                "The device engine failed without output".into()
            } else {
                tail
            })
        }
    }
}

// ───────────── The runner, and "Automation Running" ─────────────
//
// While the device engine's XCTest runner runs on an iPhone or iPad, iOS shows "Automation Running" on
// it. Apple draws that for every XCTest UI automation, and nothing may hide it, so Extend keeps
// the runner to the time a Silicon is working on the device: setup stops the runner `prepare`
// starts, every session end closes the session's device-engine session (which stops the runner) and
// makes sure the runner is gone, and every driver looks after its device every 20 s, connected or
// not: a runner no command has used for a minute is stopped (inside a live session too), as is any
// runner with no session live. A runner the device engine is still starting is never stopped: the device engine
// would take that for a failed start and build and start another, with no session to close it.

/// How a command uses the device engine's XCTest runner on an iPhone or iPad.
///
/// From the fork's physical-device (CoreDevice) paths: the app list, app state, installs, logs
/// and plain screenshots go through `devicectl` (`device info apps`, `device process`,
/// `device install app`, the device console, `device capture screenshot`); `open <app>` launches
/// through `devicectl` and the device engine starts the runner in the background, since the next
/// command nearly always reads the screen; reading the screen's elements and touching it go through
/// the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunnerUse {
    /// Served without the runner.
    Free,
    /// Served without the runner, which the device engine then starts in the background.
    Warms,
    /// Needs the runner.
    Needs,
}

pub(crate) fn runner_use(command: &str, args: &[String]) -> RunnerUse {
    let flags = || args.iter().take_while(|a| *a != "--").map(|a| flag_name(a));
    let flagged = |f: &str| flags().any(|a| a == f);
    match command {
        "apps" | "appstate" | "install" | "reinstall" | "uninstall" | "logs" | "close" => RunnerUse::Free,
        // `--overlay-refs` and `--crop-on` read the screen's elements for the picture.
        "screenshot" if !flagged("--overlay-refs") && !flagged("--crop-on") => RunnerUse::Free,
        "diff" if args.first().map(String::as_str) == Some("screenshot") && !flagged("--overlay-refs") => {
            RunnerUse::Free
        }
        // A bare `open` only binds the session to the device.
        "open" if args.iter().all(|a| a.starts_with('-')) && !flagged("--relaunch") => RunnerUse::Free,
        "open" => RunnerUse::Warms,
        _ => RunnerUse::Needs,
    }
}

/// One process, from `ps -ww -Ao pid=,ppid=,etime=,command=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// How long it has run (`etime`), when `ps` said.
    pub age: Option<Duration>,
    pub command: String,
}

/// `ps`'s `etime`: `[[dd-]hh:]mm:ss`.
pub(crate) fn parse_etime(s: &str) -> Option<Duration> {
    let (days, clock) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };
    let mut secs = 0u64;
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in &parts {
        secs = secs * 60 + part.parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(days * 86_400 + secs))
}

pub(crate) fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (etime, command) = rest.trim_start().split_once(char::is_whitespace)?;
            Some(Proc {
                pid: pid.parse().ok()?,
                ppid: ppid.parse().ok()?,
                age: parse_etime(etime),
                command: command.trim().to_owned(),
            })
        })
        .collect()
}

/// Whether `command` names this device by `id=<udid>`, the UDID matched whole (so another device,
/// or one whose UDID only starts like this one's, never does).
fn names_device(command: &str, udid: &str) -> bool {
    if udid.is_empty() {
        return false;
    }
    let needle = format!("id={udid}");
    command.match_indices(&needle).any(|(i, _)| {
        let before = command[..i].chars().next_back();
        let after = command[i + needle.len()..].chars().next();
        matches!(before, Some(',' | ' ' | '{')) && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// The runner's names in a process's command line: the helper as 1.1 builds it
/// (`SiliconExtendHelper.xcodeproj`, `SiliconExtendHelper….xctestrun`, `SiliconExtend-Runner.app`),
/// and 1.0's (`AgentDeviceRunner…`), for a runner a 1.0 engine started before the update.
fn names_runner(command: &str) -> bool {
    ["SiliconExtendHelper", "SiliconExtend-Runner", "AgentDeviceRunner"]
        .iter()
        .any(|n| command.contains(n))
}

/// Whether a command line is the device engine's runner for this device:
/// `xcodebuild test-without-building … -xctestrun …/SiliconExtendHelper….xctestrun … -destination
/// platform=iOS[ Simulator],id=<udid>`.
pub(crate) fn is_runner_for(command: &str, udid: &str) -> bool {
    command.contains("xcodebuild")
        && command.contains("test-without-building")
        && names_runner(command)
        && names_device(command, udid)
}

/// Whether a command line is the device engine building its runner for this Simulator (the first step of
/// a start; an iPhone's build names no device).
pub(crate) fn is_runner_build_for(command: &str, udid: &str) -> bool {
    command.contains("xcodebuild")
        && command.contains("build-for-testing")
        && names_runner(command)
        && names_device(command, udid)
}

/// Whether a command line is the runner app XCTest launched inside this Simulator
/// (`…/CoreSimulator/Devices/<udid>/data/Containers/Bundle/Application/…/SiliconExtend-Runner.app/…`).
/// It runs under the Simulator's launchd, not under the runner's `xcodebuild`, so stopping only
/// `xcodebuild` would leave it up for most of a minute.
pub(crate) fn is_simulator_runner_app_for(command: &str, udid: &str) -> bool {
    !udid.is_empty()
        && !command.contains("xcodebuild")
        && command.contains(&format!("/Devices/{udid}/"))
        && names_runner(command)
}

/// What the device engine's runner lease for a device (`apple-runner/leases/<udid>.json`) says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunnerLease {
    pub runner_pid: Option<u32>,
    /// The device-engine session that started the runner (its log is `sessions/<name>/runner.log`).
    pub session: Option<String>,
    /// The runner's log.
    pub log: Option<PathBuf>,
    /// The port this start of the runner listens on.
    pub port: Option<u16>,
}

pub(crate) fn parse_lease(v: &Value) -> RunnerLease {
    let log = v.get("runnerLogPath").and_then(Value::as_str).map(PathBuf::from);
    RunnerLease {
        runner_pid: v
            .get("runnerPid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok()),
        session: log
            .as_deref()
            .and_then(|p| p.parent()?.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        log,
        port: v
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok()),
    }
}

/// What a runner's log says about its start on `port`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RunnerLog {
    /// The runner said it listens (`AGENT_DEVICE_RUNNER_PORT=<port>`, the helper's own log line, which
    /// kept its upstream name).
    pub listening: bool,
    /// And it has answered a command since, so the device engine has connected to it.
    pub answered: bool,
}

/// Reads [`RunnerLog`] from a runner's log (every start of a session's runner writes to the same
/// log; the last one on `port` counts).
pub(crate) fn runner_log_state(log: &str, port: u16) -> RunnerLog {
    let needle = format!("AGENT_DEVICE_RUNNER_PORT={port}");
    let at = log
        .match_indices(&needle)
        .filter(|(i, _)| !log[i + needle.len()..].starts_with(|c: char| c.is_ascii_digit()))
        .map(|(i, _)| i)
        .last();
    match at {
        None => RunnerLog::default(),
        Some(i) => RunnerLog {
            listening: true,
            answered: log[i..].contains("AGENT_DEVICE_RUNNER_COMMAND_COMPLETED"),
        },
    }
}

/// The runner processes for this device that Extend may stop: each runner `xcodebuild`, what runs
/// under it, and (on a Simulator) the runner app. A runner the lease says a session outside Extend
/// started is left alone, and so is its app.
pub(crate) fn runner_processes(procs: &[Proc], udid: &str, lease: Option<&RunnerLease>) -> Vec<u32> {
    let foreign = |pid: u32| {
        lease.is_some_and(|l| {
            l.runner_pid == Some(pid) && l.session.as_deref().is_some_and(|s| !s.starts_with("extend-"))
        })
    };
    let runners: Vec<&Proc> = procs.iter().filter(|p| is_runner_for(&p.command, udid)).collect();
    let someone_elses = runners.iter().any(|p| foreign(p.pid));
    let mut out: Vec<u32> = runners.iter().filter(|p| !foreign(p.pid)).map(|p| p.pid).collect();
    if !someone_elses {
        out.extend(
            procs
                .iter()
                .filter(|p| is_simulator_runner_app_for(&p.command, udid))
                .map(|p| p.pid),
        );
    }
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        for p in procs {
            if p.ppid == parent && !out.contains(&p.pid) {
                out.push(p.pid);
            }
        }
        i += 1;
    }
    out
}

/// A device's runner, right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerNow {
    /// None runs (none Extend may stop).
    Gone,
    /// the device engine is still starting it (building it, launching it, or about to connect to it).
    Starting,
    /// Up and past its start (or left behind): these processes may be stopped.
    Up(Vec<u32>),
}

/// Whether this device's runner may be stopped now. `log` is what the lease's runner log says, when
/// the lease names a runner that runs.
pub(crate) fn classify_runner(
    procs: &[Proc],
    udid: &str,
    lease: Option<&RunnerLease>,
    log: Option<RunnerLog>,
) -> RunnerNow {
    if procs.iter().any(|p| is_runner_build_for(&p.command, udid)) {
        return RunnerNow::Starting;
    }
    let pids = runner_processes(procs, udid, lease);
    if pids.is_empty() {
        return RunnerNow::Gone;
    }
    for p in procs
        .iter()
        .filter(|p| pids.contains(&p.pid) && is_runner_for(&p.command, udid))
    {
        let leased = lease.is_some_and(|l| l.runner_pid == Some(p.pid));
        let log = if leased { log } else { None };
        let settled = match (p.age, log) {
            (Some(age), _) if age >= RUNNER_STARTUP_MAX => true,
            // the device engine has connected to it.
            (_, Some(RunnerLog { answered: true, .. })) => true,
            (Some(age), Some(RunnerLog { listening: true, .. })) => age >= RUNNER_SETTLE,
            (None, Some(RunnerLog { listening, .. })) => listening,
            // It hasn't said it listens yet.
            (_, Some(_)) => false,
            // No lease names it: left behind by an earlier daemon or start.
            (Some(age), None) => age >= RUNNER_SETTLE,
            (None, None) => true,
        };
        if !settled {
            return RunnerNow::Starting;
        }
    }
    RunnerNow::Up(pids)
}

/// The device engine's runner leases: `EXTEND_ENGINE_IOS_RUNNER_LEASE_DIR`, else
/// `~/.silicon-extend/engine/apple-runner/leases` (shared by every device-engine daemon on this Mac).
fn lease_dir() -> Option<PathBuf> {
    match engine_setting("IOS_RUNNER_LEASE_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".silicon-extend/engine/apple-runner/leases")),
    }
}

fn read_lease(udid: &str) -> Option<RunnerLease> {
    let bytes = std::fs::read(lease_dir()?.join(format!("{udid}.json"))).ok()?;
    Some(parse_lease(&serde_json::from_slice(&bytes).ok()?))
}

/// The end of a runner's log (the part that can hold the current start's lines).
fn read_log_tail(path: &Path) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    const TAIL: u64 = 8 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

async fn processes() -> Option<Vec<Proc>> {
    let out = Command::new("ps")
        .args(["-ww", "-Ao", "pid=,ppid=,etime=,command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

/// This device's runner right now (see [`classify_runner`]).
pub(crate) async fn runner_now(udid: &str) -> RunnerNow {
    let Some(procs) = processes().await else {
        return RunnerNow::Gone;
    };
    let lease = read_lease(udid);
    let log = lease.as_ref().and_then(|l| {
        let pid = l.runner_pid?;
        if !procs.iter().any(|p| p.pid == pid) {
            return None;
        }
        Some(runner_log_state(&read_log_tail(l.log.as_deref()?)?, l.port?))
    });
    classify_runner(&procs, udid, lease.as_ref(), log)
}

/// The runner processes for this device running now, starting or not (see [`runner_processes`]).
pub(crate) async fn runner_pids(udid: &str) -> Vec<u32> {
    match processes().await {
        Some(procs) => runner_processes(&procs, udid, read_lease(udid).as_ref()),
        None => vec![],
    }
}

async fn signal(pids: &[u32], signal: &str) {
    if pids.is_empty() {
        return;
    }
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .args(pids.iter().map(u32::to_string))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// What [`stop_runner`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerStop {
    /// No runner was up, or it went by itself.
    Gone,
    /// the device engine is still starting one; it was left alone (the next look stops it once started).
    LeftStarting,
    /// These processes were stopped.
    Stopped(Vec<u32>),
}

/// Makes sure the device engine's runner for this device is gone: waits up to `wait` for the device engine's
/// own close to stop it, then stops exactly this device's runner (SIGTERM, which ends the XCTest
/// run the way the device engine does, then SIGKILL). A runner the device engine is still starting is left
/// alone.
pub(crate) async fn stop_runner(udid: &str, wait: Duration) -> RunnerStop {
    let deadline = Instant::now() + wait;
    let mut now = runner_now(udid).await;
    while now != RunnerNow::Gone && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
        now = runner_now(udid).await;
    }
    let pids = match now {
        RunnerNow::Gone => return RunnerStop::Gone,
        RunnerNow::Starting => {
            tracing::debug!(
                udid,
                "the device engine is still starting the runner; it is stopped later"
            );
            return RunnerStop::LeftStarting;
        }
        RunnerNow::Up(pids) => pids,
    };
    tracing::info!(
        udid,
        ?pids,
        "stopping the device engine's runner, which nothing is using"
    );
    signal(&pids, "TERM").await;
    let deadline = Instant::now() + RUNNER_TERM_GRACE;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let left: Vec<u32> = runner_pids(udid)
            .await
            .into_iter()
            .filter(|p| pids.contains(p))
            .collect();
        if left.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            signal(&left, "KILL").await;
            break;
        }
    }
    RunnerStop::Stopped(pids)
}

/// Simulator UDIDs are UUIDs; an iPhone's or iPad's never is.
fn is_simulator_udid(udid: &str) -> bool {
    uuid::Uuid::parse_str(udid).is_ok()
}

/// What every driver built for one device shares (the service attaches a device again on every
/// reconnect, and each attach builds a new driver).
#[derive(Default)]
struct DeviceShared {
    /// A command holds it while it runs, and a session's end, the helper's install and the
    /// driver's look after the device hold it while they close sessions or stop runners, so a
    /// runner is never stopped under a running command.
    guard: Arc<tokio::sync::Mutex<()>>,
    /// When the last command on the device finished (none yet in this run of Extend: `None`).
    last_command: Mutex<Option<Instant>>,
    /// Recording path per session, from `record start` to `record stop`. The runner records, so
    /// it keeps running while a recording does.
    recordings: Mutex<HashMap<String, PathBuf>>,
}

impl DeviceShared {
    fn used_now(&self) {
        *self.last_command.lock().unwrap() = Some(Instant::now());
    }

    /// How long no command has used the device (`None`: none has in this run of Extend).
    fn unused_for(&self) -> Option<Duration> {
        self.last_command.lock().unwrap().map(|t| t.elapsed())
    }

    fn recording(&self) -> bool {
        !self.recordings.lock().unwrap().is_empty()
    }
}

fn shared(udid: &str) -> Arc<DeviceShared> {
    static DEVICES: LazyLock<Mutex<HashMap<String, Arc<DeviceShared>>>> = LazyLock::new(Default::default);
    DEVICES.lock().unwrap().entry(udid.to_owned()).or_default().clone()
}

/// Whether the driver's look after the device stops its runner (once started): with no session
/// live any runner is left over; within one, a runner no command has used for
/// [`RUNNER_IDLE_STOP`] goes, unless it is recording.
pub(crate) fn runner_should_stop(live: bool, unused_for: Option<Duration>, recording: bool) -> bool {
    !recording && (!live || unused_for.is_none_or(|d| d >= RUNNER_IDLE_STOP))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The device-engine calls that put the helper on the device: `prepare ios-runner` inside a setup
/// session of its own, closed right after. `prepare` starts the runner to prove the install; the
/// close stops it (the device engine never keeps an iPhone's or iPad's runner past its session's close).
pub(crate) fn helper_install_calls() -> Vec<(&'static str, Vec<String>)> {
    vec![
        // A setup session a crash left open would refuse the `open` below.
        ("close", vec![]),
        // Binds the setup session to the device; on an iPhone this launches nothing and starts
        // no runner.
        ("open", vec![]),
        (
            "prepare",
            vec!["ios-runner".into(), "--timeout".into(), PREPARE_TIMEOUT_MS.to_string()],
        ),
        ("close", vec![]),
    ]
}

/// What the `helper` setup step does after looking for the helper on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HelperNext {
    /// It's there (or was, and the device didn't answer this time).
    Done,
    /// `prepare` is running.
    Installing,
    /// The last install failed a moment ago: show why, then try again.
    ShowFailure(String),
    /// Missing while a Silicon's session may still be live: installed once it ends.
    AfterSession,
    /// The device answered without it: install it. The only way setup starts the runner.
    Install,
    /// The device didn't answer and the helper was never seen: show why, install nothing.
    Unchecked(String),
}

/// `listed`: whether `devicectl device info apps` lists the helper, or why it couldn't tell.
/// `verified`: the helper was seen before. `live`: a Silicon's session may still be live.
pub(crate) fn helper_next(listed: &Result<bool, String>, prepare: &Prepare, verified: bool, live: bool) -> HelperNext {
    match (listed, prepare) {
        (Ok(true), _) => HelperNext::Done,
        (_, Prepare::Running) => HelperNext::Installing,
        (Ok(false), Prepare::Failed(e, at)) if at.elapsed() < PREPARE_RETRY_AFTER => HelperNext::ShowFailure(e.clone()),
        (Ok(false), _) if live => HelperNext::AfterSession,
        (Ok(false), _) => HelperNext::Install,
        (Err(_), _) if verified => HelperNext::Done,
        (Err(e), _) => HelperNext::Unchecked(e.clone()),
    }
}

// ───────────── Driver ─────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    udid: Option<String>,
    /// The helper was seen on the device, or `prepare` put it there. A check that fails later (a
    /// locked device) never installs it again; only a device that answers without it does.
    #[serde(default)]
    helper_verified: bool,
    /// device-engine sessions this driver has run commands in and not closed yet (an entry goes only
    /// once the device engine has answered its `close`).
    #[serde(default)]
    sessions: BTreeMap<String, OpenSession>,
    /// The helper install opened its setup session and hasn't closed it yet. Set while the install
    /// runs; still set afterwards only if Extend stopped in the middle of it, and then the driver's
    /// look after the device closes the session (it claims the device and keeps the daemon up).
    #[serde(default)]
    setup_open: bool,
    /// 1.0's helper (`com.callstack.agentdevice.runner*`) is gone from the device.
    #[serde(default)]
    old_helper_gone: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct OpenSession {
    /// When a command last ran in it (ms since the epoch).
    last_used_ms: u64,
    /// A command in it used (or warmed) the runner.
    #[serde(default)]
    runner: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum Prepare {
    Idle,
    Running,
    Done,
    Failed(String, Instant),
}

/// ios.json is changed by read, change, write; drivers built for the same device (and the
/// background tasks they start) take turns.
static STATE_LOCK: Mutex<()> = Mutex::new(());

struct Inner {
    device: HostedDevice,
    saved: Mutex<Saved>,
    prepare: Mutex<Prepare>,
    version: Mutex<Option<String>>,
    runner_seen: Mutex<Option<Instant>>,
}

pub struct IosDriver {
    inner: Arc<Inner>,
}

impl IosDriver {
    /// Built inside a tokio runtime (as the agent builds it), the driver also looks after its
    /// device every [`TEND_EVERY`] until it is dropped, whether or not the Mac is connected to
    /// Extend (see [`Inner::tend`]).
    pub(crate) fn new(device: HostedDevice) -> Self {
        let saved = load_json(&device.state_dir, STATE_FILE).unwrap_or_default();
        let inner = Arc::new(Inner {
            device,
            saved: Mutex::new(saved),
            prepare: Mutex::new(Prepare::Idle),
            version: Mutex::new(None),
            runner_seen: Mutex::new(None),
        });
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let me = Arc::downgrade(&inner);
            rt.spawn(async move {
                loop {
                    tokio::time::sleep(TEND_EVERY).await;
                    let Some(me) = me.upgrade() else { return };
                    me.tend().await;
                }
            });
        }
        Self { inner }
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
        self.device.state_dir.join("recordings").join(session_name(session_id))
    }

    /// Changes ios.json starting from what is on disk, since another driver for this device may
    /// have written it, and saves it.
    fn update_saved<R>(&self, change: impl FnOnce(&mut Saved) -> R) -> R {
        let _turn = STATE_LOCK.lock().unwrap();
        let mut saved =
            load_json(&self.device.state_dir, STATE_FILE).unwrap_or_else(|| self.saved.lock().unwrap().clone());
        let r = change(&mut saved);
        if let Err(e) = save_json(&self.device.state_dir, STATE_FILE, &saved) {
            tracing::warn!(error = %e, "couldn't save the iPhone's state");
        }
        *self.saved.lock().unwrap() = saved;
        r
    }

    fn read_saved(&self) -> Saved {
        let _turn = STATE_LOCK.lock().unwrap();
        load_json(&self.device.state_dir, STATE_FILE).unwrap_or_else(|| self.saved.lock().unwrap().clone())
    }

    /// Notes that a command is about to run in `session`, so the session is closed when it ends
    /// (or, if its end never reaches this Mac, once it has certainly ended).
    fn note_session(&self, session: &str, runner: bool) {
        let now = now_ms();
        let fresh = self.saved.lock().unwrap().sessions.get(session).is_some_and(|s| {
            (s.runner || !runner) && now.saturating_sub(s.last_used_ms) < SESSION_NOTE_EVERY.as_millis() as u64
        });
        if fresh {
            return;
        }
        self.update_saved(|s| {
            let open = s.sessions.entry(session.to_owned()).or_default();
            open.last_used_ms = now;
            open.runner |= runner;
        });
    }

    /// Notes that `session` is still live though no command ran (a takeover started or ended), so
    /// the backstop in [`Self::tend`] never closes it under a run of takeovers. A session with no
    /// device-engine session yet has nothing to keep.
    fn keep_session(&self, session: &str) {
        if !self.read_saved().sessions.contains_key(session) {
            return;
        }
        let now = now_ms();
        self.update_saved(|s| {
            if let Some(open) = s.sessions.get_mut(session) {
                open.last_used_ms = open.last_used_ms.max(now);
            }
        });
    }

    /// Whether a Silicon's session may still be using the device.
    fn has_live_sessions(&self) -> bool {
        let now = now_ms();
        self.read_saved()
            .sessions
            .values()
            .any(|s| now.saturating_sub(s.last_used_ms) <= STALE_SESSION_AFTER.as_millis() as u64)
    }

    fn base_command(&self) -> Command {
        let mut cmd = Command::new(&self.device.agent_device[0]);
        cmd.args(&self.device.agent_device[1..]);
        // Read by a daemon this command starts (a running daemon keeps its own). A value set
        // under either name wins.
        for (name, value) in DAEMON_ENV_DEFAULTS {
            if engine_setting(name.trim_start_matches("EXTEND_ENGINE_")).is_none() {
                cmd.env(name, value);
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    async fn engine_version(&self) -> Option<String> {
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

    /// Runs the device engine against this device. Returns (stdout, stderr, success) or why it couldn't run.
    async fn agent_device(
        &self,
        udid: &str,
        session: &str,
        command: &str,
        args: &[String],
        inv: Option<&Invocation<'_>>,
    ) -> Result<(String, String, bool), String> {
        let mut cmd = self.base_command();
        cmd.args(with_extend_flags(
            command,
            args,
            &["--platform", "ios", "--udid", udid, "--json", "--session", session],
        ));
        let child = cmd.spawn().map_err(|e| {
            format!(
                "couldn't start the device engine ({}): {e}",
                self.device.agent_device[0]
            )
        })?;
        let wait = child.wait_with_output();
        let out = match inv {
            Some(inv) => {
                tokio::select! {
                    r = wait => r,
                    _ = inv.cancel.cancelled() => return Err("cancelled".into()),
                    _ = tokio::time::sleep(inv.timeout) => return Err(format!("the device engine didn't finish within {} ms", inv.timeout.as_millis())),
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

    /// [`Self::agent_device`] for Extend's own calls (setup, session ends), bounded by `limit`.
    async fn agent_device_within(
        &self,
        udid: &str,
        session: &str,
        command: &str,
        args: &[String],
        limit: Duration,
    ) -> Result<(String, String, bool), String> {
        match tokio::time::timeout(limit, self.agent_device(udid, session, command, args, None)).await {
            Ok(r) => r,
            Err(_) => Err(format!(
                "the device engine didn't finish `{command}` within {} s",
                limit.as_secs()
            )),
        }
    }

    /// Closes an abandoned device-engine session that still claims this device.
    async fn close_stale(&self, udid: &str, owner: &StaleOwner) {
        tracing::info!(session = %owner.session, "closing an abandoned device-engine session on this device");
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
            cmd.env("EXTEND_ENGINE_STATE_DIR", dir);
        }
        if let Some(ws) = owner.workspace.as_ref().filter(|w| Path::new(w).is_dir()) {
            cmd.current_dir(ws);
        }
        let _ = tokio::time::timeout(Duration::from_secs(60), cmd.output()).await;
    }

    /// Closes one device-engine session of this device and, once the device engine has answered, forgets
    /// it in ios.json (a close Extend couldn't finish, as when the app quits in the middle of it,
    /// is tried again later: at the next reconnect, or by the backstop in [`Self::tend`]).
    async fn close_session(&self, udid: &str, name: &str) -> bool {
        let answered = match self.agent_device_within(udid, name, "close", &[], CLOSE_LIMIT).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(session = %name, "couldn't close the device-engine session: {e}");
                false
            }
        };
        let _ = std::fs::remove_dir_all(self.device.state_dir.join("recordings").join(name));
        if answered {
            self.update_saved(|s| {
                s.sessions.remove(name);
            });
        }
        answered
    }

    /// Ends Extend sessions on this device: `session_id`'s, or, when it is empty, every one this
    /// driver has open (the device is no longer carried, or the service has no session on it).
    /// Closes their device-engine sessions (which finishes a running recording first and, on an
    /// iPhone or iPad, stops the runner) and makes sure the runner is gone. The caller holds the
    /// device's guard.
    async fn end_sessions(&self, udid: &str, session_id: &str) {
        let saved = self.read_saved();
        let names: Vec<String> = if session_id.is_empty() {
            saved.sessions.keys().cloned().collect()
        } else {
            vec![session_name(session_id)]
        };
        let used_runner = names.iter().any(|n| saved.sessions.get(n).is_some_and(|o| o.runner));
        {
            let dev = shared(udid);
            let mut recordings = dev.recordings.lock().unwrap();
            if session_id.is_empty() {
                recordings.clear();
            } else {
                recordings.remove(session_id);
            }
        }
        for name in &names {
            self.close_session(udid, name).await;
        }
        // The device is no longer carried (or has no session): a setup session an interrupted
        // install left open goes too, since no later look after the device may come.
        if session_id.is_empty()
            && self.read_saved().setup_open
            && self
                .agent_device_within(udid, SETUP_SESSION, "close", &[], CLOSE_LIMIT)
                .await
                .is_ok()
        {
            self.update_saved(|s| s.setup_open = false);
        }
        // the device engine's close already stopped an iPhone's runner; a Simulator's it keeps warm on
        // purpose, and here it goes too, as on the device, unless it is still starting (then the
        // next look after the device stops it).
        let wait = if used_runner && !is_simulator_udid(udid) {
            RUNNER_EXIT_WAIT
        } else {
            Duration::ZERO
        };
        stop_runner(udid, wait).await;
    }

    /// Looks after the device, every [`TEND_EVERY`] whether or not the Mac is connected, and only
    /// while nothing else uses the device (no command, session end or helper install):
    ///
    /// - a setup session a helper install left open when Extend stopped in the middle of it is
    ///   closed (it claims the device, so every Silicon command would be refused);
    /// - backstop for a session whose end never reached this Mac: once no command, takeover start
    ///   or takeover end has come for longer than any live session can go without one, it is
    ///   closed;
    /// - the runner (once the device engine has finished starting it) is stopped when no session is
    ///   live, or when no command has used it for [`RUNNER_IDLE_STOP`] and it isn't recording.
    async fn tend(&self) {
        let Some(udid) = self.udid() else { return };
        let dev = shared(&udid);
        let Ok(_held) = dev.guard.try_lock() else { return };
        let saved = self.read_saved();
        if saved.setup_open {
            tracing::info!("closing the setup session an interrupted helper install left open");
            if self
                .agent_device_within(&udid, SETUP_SESSION, "close", &[], CLOSE_LIMIT)
                .await
                .is_ok()
            {
                self.update_saved(|s| s.setup_open = false);
            }
        }
        let now = now_ms();
        for (name, _) in saved
            .sessions
            .iter()
            .filter(|(_, s)| now.saturating_sub(s.last_used_ms) > STALE_SESSION_AFTER.as_millis() as u64)
        {
            tracing::info!(session = %name, "closing a device-engine session no command has used for a long time");
            self.close_session(&udid, name).await;
        }
        let live = !self.read_saved().sessions.is_empty();
        if runner_should_stop(live, dev.unused_for(), dev.recording()) {
            stop_runner(&udid, Duration::ZERO).await;
        }
    }

    /// Puts the helper on the device (see [`helper_install_calls`]) and makes sure the runner that
    /// proved it is gone afterwards.
    async fn install_helper(&self, udid: &str) -> Result<(), String> {
        let dev = shared(udid);
        let _held = dev.guard.lock().await;
        // Another driver for this device (the service attached it again meanwhile) may have
        // installed it while this one waited; the helper step clears this before asking again.
        if self.read_saved().helper_verified {
            return Ok(());
        }
        let mut result = Err("prepare didn't run".to_owned());
        let mut closed = false;
        for (command, args) in helper_install_calls() {
            if command == "open" {
                self.update_saved(|s| s.setup_open = true);
            }
            let limit = if command == "prepare" {
                Duration::from_millis(PREPARE_TIMEOUT_MS) + CLOSE_LIMIT
            } else {
                CLOSE_LIMIT
            };
            let out = self
                .agent_device_within(udid, SETUP_SESSION, command, &args, limit)
                .await;
            match command {
                "prepare" => {
                    result = out.and_then(|(stdout, stderr, ok)| {
                        let out = to_output("prepare", &stdout, &stderr, ok);
                        if out.ok {
                            Ok(())
                        } else {
                            Err(out.error.map(|e| e.message).unwrap_or_else(|| "prepare failed".into()))
                        }
                    })
                }
                "close" => closed = out.is_ok(),
                _ => {}
            }
        }
        if closed {
            self.update_saved(|s| s.setup_open = false);
        }
        let wait = if is_simulator_udid(udid) {
            Duration::ZERO
        } else {
            RUNNER_EXIT_WAIT
        };
        stop_runner(udid, wait).await;
        result
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
            let next = match me.install_helper(&udid).await {
                Ok(()) => {
                    *me.runner_seen.lock().unwrap() = Some(Instant::now());
                    me.update_saved(|s| s.helper_verified = true);
                    Prepare::Done
                }
                Err(e) => Prepare::Failed(e, Instant::now()),
            };
            *me.prepare.lock().unwrap() = next;
        });
    }

    /// The `helper` setup step. Only a device that answers without the helper gets it (again);
    /// a check that fails (a locked device) reports the step instead.
    async fn helper_step(self: &Arc<Self>, udid: &str, title: &str) -> extend_protocol::model::SetupStep {
        let kind = self.kind();
        let recently_seen = self
            .runner_seen
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < HELPER_RECHECK);
        let listed: Result<bool, String> = if recently_seen {
            Ok(true)
        } else {
            let prefix = engine_setting("IOS_BUNDLE_ID").unwrap_or_else(|| DEFAULT_RUNNER_BUNDLE.into());
            devicectl(&["device", "info", "apps", "--device", udid, "--timeout", "15"])
                .await
                .map(|v| {
                    let installed = runner_installed(&v, &prefix);
                    if installed {
                        self.remove_old_helpers(udid, old_helpers(&v));
                    }
                    installed
                })
        };
        let prepare = self.prepare.lock().unwrap().clone();
        let verified = self.saved.lock().unwrap().helper_verified;
        let live = listed == Ok(false) && self.has_live_sessions();
        let installing = format!(
            "Extend is putting its helper on the {kind}. Keep it unlocked and connected; the first time takes a few minutes."
        );
        match helper_next(&listed, &prepare, verified, live) {
            HelperNext::Done => {
                if listed == Ok(true) {
                    *self.runner_seen.lock().unwrap() = Some(Instant::now());
                    if !verified {
                        self.update_saved(|s| s.helper_verified = true);
                    }
                }
                step("helper", title, StepStatus::Done)
            }
            HelperNext::Installing => step_help("helper", title, StepStatus::InProgress, &installing),
            HelperNext::ShowFailure(e) => {
                step_failure("helper", title, StepStatus::Failed, &plain_setup_error(kind, &e), &e)
            }
            HelperNext::AfterSession => step_help(
                "helper",
                title,
                StepStatus::Todo,
                &format!("Extend puts its helper back on the {kind} once the Silicon's session there ends."),
            ),
            HelperNext::Install => {
                self.update_saved(|s| s.helper_verified = false);
                self.start_prepare(udid.to_owned());
                step_help("helper", title, StepStatus::InProgress, &installing)
            }
            HelperNext::Unchecked(e) => {
                step_failure("helper", title, StepStatus::Todo, &plain_setup_error(kind, &e), &e)
            }
        }
    }

    /// Removes 1.0's helper from the device once the new one is there (best effort, once).
    fn remove_old_helpers(self: &Arc<Self>, udid: &str, old: Vec<String>) {
        if is_simulator_udid(udid) || self.saved.lock().unwrap().old_helper_gone {
            return;
        }
        if old.is_empty() {
            self.update_saved(|s| s.old_helper_gone = true);
            return;
        }
        let me = Arc::clone(self);
        let udid = udid.to_owned();
        tokio::spawn(async move {
            let mut all_gone = true;
            for bundle in old {
                match devicectl(&[
                    "device",
                    "uninstall",
                    "app",
                    "--device",
                    &udid,
                    &bundle,
                    "--timeout",
                    "30",
                ])
                .await
                {
                    Ok(_) => tracing::info!(bundle, "removed Silicon Extend 1.0's helper from the device"),
                    Err(e) => {
                        all_gone = false;
                        tracing::info!(bundle, "couldn't remove Silicon Extend 1.0's helper yet: {e}");
                    }
                }
            }
            if all_gone {
                me.update_saved(|s| s.old_helper_gone = true);
            }
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
                        step_failure(
                            "connect",
                            &connect_title,
                            StepStatus::Failed,
                            "Xcode isn't ready on this Mac. Install Xcode from the App Store, open it once to finish setting it up, then tap Retry.",
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
            Some(u) => mine.iter().find(|d| d.udid == u || d.identifier == u).copied(),
            None => {
                // Adopt the device the Carbon just plugged in: the one with this name, or the only one.
                let pick = mine
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(&self.device.name))
                    .or(if mine.len() == 1 { mine.first() } else { None })
                    .copied();
                if let Some(d) = pick {
                    self.update_saved(|s| s.udid = Some(d.udid.clone()));
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
                        &format!("Use a cable. Unlock the {kind}, tap Trust when it asks, and enter its passcode."),
                    ),
                    step("developer_mode", devmode_title, StepStatus::Todo),
                    step("helper", helper_title, StepStatus::Todo),
                ],
            );
        };
        let udid = dev.udid.clone();
        let details = devicectl(&["device", "info", "details", "--device", &udid, "--timeout", "15"]).await;
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
            (None, Err(e)) => step_failure(
                "developer_mode",
                devmode_title,
                StepStatus::Todo,
                &plain_setup_error(kind, e),
                e,
            ),
            (None, Ok(_)) => step("developer_mode", devmode_title, StepStatus::Todo),
        };
        let helper = if devmode != Some(true) {
            step("helper", helper_title, StepStatus::Todo)
        } else {
            self.helper_step(&udid, helper_title).await
        };
        (online, os_version, model, vec![connect, devmode_step, helper])
    }
}

#[async_trait]
impl Driver for IosDriver {
    async fn probe(&self) -> Probe {
        let me = &self.inner;
        let full = me.device.os.full_capabilities();
        let version = me.engine_version().await;
        let udid = me.udid();
        let sims = if simulators_allowed() {
            simulators().await
        } else {
            vec![]
        };
        let sim = udid.as_ref().and_then(|u| sims.iter().find(|s| &s.udid == u)).cloned();
        let (online, os_version, model, mut steps) = match sim {
            Some(s) => (
                true,
                Some(s.runtime.clone()),
                Some(format!("{} (Simulator)", s.name)),
                vec![
                    step("connect", "Simulator selected (development only)", StepStatus::Done),
                    step(
                        "developer_mode",
                        "Developer Mode (not needed on a Simulator)",
                        StepStatus::Done,
                    ),
                    step(
                        "helper",
                        "Extend helper installed (the device engine installs it on first use)",
                        StepStatus::Done,
                    ),
                ],
            ),
            None => me.probe_physical().await,
        };
        if version.is_none() {
            steps.push(step_failure(
                "engine",
                "Silicon Extend's device engine is ready on this Mac",
                StepStatus::Failed,
                ENGINE_MISSING,
                format!("`{} --version` didn't run", me.device.agent_device.join(" ")),
            ));
        }
        let setup = Setup::from_steps(steps);
        let ready = online && setup.state == extend_protocol::model::SetupState::Complete;
        let reason = setup
            .steps
            .iter()
            .find(|s| s.status != StepStatus::Done)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| format!("The {} isn't reachable; keep it unlocked and near this Mac", me.kind()));
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
            engine_version: version,
            online,
            // Whether an iPhone or iPad is locked isn't read here yet: without a real device's
            // answer to check against, it stays "can't tell" and the Carbon says when it's awake.
            awake: None,
            sleep_state: None,
            // The UDID names the physical device whichever Mac or Carbon adds it.
            hardware_id: udid.filter(|u| !is_simulator_udid(u)),
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
        let plan = match plan(inv.command, inv.args, inv.attachments, inv.workdir, &record_dir) {
            Ok(p) => p,
            Err(e) if e.contains("isn't available through Extend") => {
                return Output::fail(ErrorCode::UnknownCommand.as_str().as_str(), e);
            }
            Err(e) => return invalid(e),
        };
        let dev = shared(&udid);
        let _held = tokio::select! {
            held = dev.guard.clone().lock_owned() => held,
            _ = inv.cancel.cancelled() => {
                return crate::common::failed(format!("`{}` was cancelled", inv.command));
            }
            _ = tokio::time::sleep(inv.timeout) => {
                return Output::fail(
                    ErrorCode::CommandTimeout.as_str().as_str(),
                    format!("The {} was still busy (Extend was setting it up, ending a session or stopping its idle runner) after {} ms", me.kind(), inv.timeout.as_millis()),
                );
            }
        };
        // The device counts as used until the command is over, however it ends (the runner's idle
        // stop counts from there).
        struct Using(Arc<DeviceShared>);
        impl Drop for Using {
            fn drop(&mut self) {
                self.0.used_now();
            }
        }
        dev.used_now();
        let _using = Using(Arc::clone(&dev));
        if plan.record_to.is_some() || inv.args.iter().any(|a| flag_name(a) == "--save-script") {
            let _ = std::fs::create_dir_all(&record_dir);
        }
        let _ = std::fs::create_dir_all(inv.workdir);
        let before = list_files(inv.workdir);
        let session = session_name(inv.session_id);
        me.note_session(&session, runner_use(inv.command, inv.args) != RunnerUse::Free);
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
            // Extend lets one session use a device at a time, so another `extend-…` the device engine
            // session still holding it was left behind (a crash, a lost session end): close it.
            match stale_owner(&out, &session) {
                Some(owner) if attempt == 0 => me.close_stale(&udid, &owner).await,
                _ => break,
            }
        }

        if out.ok
            && let Some(path) = &plan.record_to
        {
            dev.recordings
                .lock()
                .unwrap()
                .insert(inv.session_id.to_owned(), path.clone());
        }
        if plan.record_stop
            && let Some(src) = dev.recordings.lock().unwrap().remove(inv.session_id)
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

    /// A session ended (an empty id: every session on the device, which is no longer carried or
    /// has no session live). Its device-engine session is closed and the runner is made sure to be
    /// gone, so "Automation Running" leaves the device with the session.
    async fn session_ended(&self, session_id: &str) {
        let me = &self.inner;
        let Some(udid) = me.udid() else { return };
        let dev = shared(&udid);
        let _held = dev.guard.lock().await;
        me.end_sessions(&udid, session_id).await;
    }

    /// A takeover started or ended: the session is live though no command runs, so the backstop
    /// never closes its device-engine session (the runner still stops once idle, so "Automation
    /// Running" isn't up while the Carbon uses the device).
    async fn session_active(&self, session_id: &str) {
        self.inner.keep_session(&session_name(session_id));
    }

    /// Retry: a failed helper install runs again at the probe that follows at once (not a minute
    /// later), and the device is asked afresh whether the helper is there.
    async fn retry_setup(&self, _step: Option<&str>) {
        let me = &self.inner;
        {
            let mut p = me.prepare.lock().unwrap();
            if matches!(*p, Prepare::Failed(..)) {
                *p = Prepare::Idle;
            }
        }
        *me.runner_seen.lock().unwrap() = None;
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
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&stem)) {
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
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
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

        assert_eq!(parse_developer_mode(&fixture("devicectl-details.json")), Some(true));
        let locked = fixture("devicectl-apps-locked.json");
        assert!(!runner_installed(&locked, DEFAULT_RUNNER_BUNDLE));
        let msg = devicectl_error(&locked).unwrap();
        assert!(msg.contains("locked") && msg.contains("Unlock the device"), "{msg}");
        // 1.0's helper alone isn't the helper; once the new one is there the old one goes.
        let old_only = json!({"result": {"apps": [{"bundleIdentifier": "com.apple.mobilesafari"}, {"bundleIdentifier": "com.callstack.agentdevice.runner.uitests.xctrunner"}]}});
        assert!(!runner_installed(&old_only, DEFAULT_RUNNER_BUNDLE));
        let both = json!({"result": {"apps": [
            {"bundleIdentifier": "com.callstack.agentdevice.runner"},
            {"bundleIdentifier": "com.callstack.agentdevice.runner.uitests.xctrunner"},
            {"bundleIdentifier": "com.teamofsilicons.extend.helper"},
            {"bundleIdentifier": "com.teamofsilicons.extend.helper.uitests.xctrunner"}]}});
        assert!(runner_installed(&both, DEFAULT_RUNNER_BUNDLE));
        assert_eq!(
            old_helpers(&both),
            vec![
                "com.callstack.agentdevice.runner".to_owned(),
                "com.callstack.agentdevice.runner.uitests.xctrunner".to_owned()
            ]
        );
        assert!(old_helpers(&locked).is_empty());
    }

    #[test]
    fn setup_errors_are_said_plainly() {
        let cases = [
            (
                "The device is locked. (com.apple.dt.CoreDeviceError error 4000.) Unlock the device and try again.",
                "The iPhone is locked or not connected by cable. Unlock it and keep it plugged in.",
            ),
            ("Developer Mode is disabled", "Developer Mode is off on the iPhone"),
            (
                "Unable to launch com.teamofsilicons.extend.helper because it has an invalid code signature, inadequate entitlements or its profile has not been explicitly trusted by the user.",
                "doesn't trust the helper's developer yet",
            ),
            (
                "xcodebuild: error: Signing for \"SiliconExtendHelper\" requires a development team. Select a development team in the Signing & Capabilities editor.",
                "Xcode on this Mac can't sign the helper",
            ),
            (
                "No Account for Team \"ABCDE12345\"",
                "Xcode on this Mac can't sign the helper",
            ),
            ("The device is not paired with this Mac", "doesn't trust this Mac yet"),
            (
                "couldn't run xcrun devicectl: No such file or directory (is Xcode installed?)",
                "Xcode isn't ready on this Mac",
            ),
            ("devicectl didn't answer in time", "isn't reachable from this Mac"),
            ("exit status 65", "Extend couldn't set up its helper on the iPhone"),
        ];
        for (detail, said) in cases {
            let plain = plain_setup_error("iPhone", detail);
            assert!(plain.contains(said), "{detail:?} → {plain:?}");
            // One or two sentences, and nothing technical from the detail.
            assert!(plain.matches(". ").count() <= 2, "{plain}");
            for technical in ["xcodebuild", "CoreDeviceError", "exit status", "com.apple", "0x"] {
                assert!(!plain.contains(technical), "{plain}");
            }
        }
        assert!(plain_setup_error("iPad", "Developer Mode is disabled").contains("on the iPad"));
    }

    #[test]
    fn runners_are_found_by_their_1_1_and_1_0_names() {
        let udid = "00008110-0000FA4E00000001";
        let dest = "platform=iOS";
        let new = format!(
            "/usr/bin/xcodebuild test-without-building -only-testing SiliconExtendHelperUITests/RunnerTests/testCommand -xctestrun /x/SiliconExtendHelper.env.session-{udid}.xctestrun -destination {dest},id={udid}"
        );
        let old = new.replace("SiliconExtendHelper", "AgentDeviceRunner");
        assert!(is_runner_for(&new, udid) && is_runner_for(&old, udid));
        assert!(!is_runner_for(&new.replace("SiliconExtendHelper", "Other"), udid));
        let build = format!(
            "/usr/bin/xcodebuild build-for-testing -project /x/SiliconExtendHelper.xcodeproj -scheme SiliconExtendHelper -destination platform=iOS Simulator,id={udid}"
        );
        assert!(is_runner_build_for(&build, udid));
        let app = format!(
            "/Users/c/Library/Developer/CoreSimulator/Devices/{udid}/data/Containers/Bundle/Application/B9/SiliconExtend-Runner.app/SiliconExtend-Runner"
        );
        assert!(is_simulator_runner_app_for(&app, udid));
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
        let p = plan("screenshot", &s(&["../../etc/home", "--overlay-refs"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["/w/home.png", "--overlay-refs"]));

        let base = vec![PathBuf::from("/att/base.png")];
        let p = plan("diff", &s(&["screenshot", "--baseline", "base.png"]), &base, w, r).unwrap();
        assert_eq!(
            p.args,
            s(&["screenshot", "--baseline", "/att/base.png", "--out", "/w/diff.png"])
        );
        assert!(plan("diff", &s(&["screenshot", "--baseline", "nope.png"]), &[], w, r).is_err());

        let p = plan("record", &s(&["start", "--fps", "30"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["start", "/r/recording.mp4", "--fps", "30"]));
        assert_eq!(p.record_to, Some(PathBuf::from("/r/recording.mp4")));
        // `cli.yaml`'s normal is the device engine's medium; high passes as it is.
        let p = plan("record", &s(&["start", "--quality", "normal"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["start", "/r/recording.mp4", "--quality", "medium"]));
        let p = plan("record", &s(&["start", "clip", "--quality=high"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["start", "/r/clip.mp4", "--quality=high"]));
        let why = plan("record", &s(&["start", "--quality", "ultra"]), &[], w, r).unwrap_err();
        assert!(why.contains("--quality normal") && why.contains("high"), "{why}");
        assert!(plan("record", &s(&["start", "--quality"]), &[], w, r).is_err());
        // Extend's own flags stay ahead of a `--`, where the device engine would type them.
        assert_eq!(
            with_extend_flags("type", &s(&["--", "--json"]), &["--platform", "ios"]),
            s(&["type", "--platform", "ios", "--", "--json"])
        );
        assert_eq!(
            with_extend_flags("snapshot", &s(&["-i"]), &["--platform", "ios"]),
            s(&["snapshot", "-i", "--platform", "ios"])
        );
        assert!(plan("record", &s(&["stop"]), &[], w, r).unwrap().record_stop);

        let p = plan("close", &s(&["--save-script"]), &[], w, r).unwrap();
        assert_eq!(p.args, s(&["--save-script", "/w/session.ad"]));
        let p = plan("open", &s(&["Settings", "--save-script", "flow.ad"]), &[], w, r).unwrap();
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
        // A setup session an interrupted helper install left open is recovered too.
        assert_eq!(
            stale_owner(&busy(SETUP_SESSION), "extend-new").map(|o| o.session),
            Some(SETUP_SESSION.to_owned())
        );
        // A session of the same daemon is named in the refusal itself (as the device engine prints it).
        let same_daemon = |session: &str| {
            let body = json!({"success": false, "error": {"code": "DEVICE_IN_USE",
                "message": format!("Device is already in use by session \"{session}\"."),
                "details": {"session": session, "deviceId": SIM, "deviceName": "iPhone 16e"}}});
            to_output("open", &body.to_string(), "", false)
        };
        assert_eq!(
            stale_owner(&same_daemon("extend-rl44515"), "extend-new"),
            Some(StaleOwner {
                session: "extend-rl44515".into(),
                state_dir: None,
                workspace: None
            })
        );
        assert_eq!(stale_owner(&same_daemon("default"), "extend-new"), None);
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
        assert_eq!(render_text("screenshot", &data), "Screenshot general.png (390x844)");
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

    /// Drives a real iOS Simulator through the device engine: open Settings, snapshot, click, screenshot.
    /// Run with:
    /// `EXTEND_HOSTED_ALLOW_SIMULATOR=1 EXTEND_HOSTED_SIM_UDID=<udid> cargo test -p extend-hosted -- --ignored --nocapture simulator_end_to_end`
    /// (the device engine must be built in `vendor/extend-engine`; `EXTEND_HOSTED_ENGINE` overrides
    /// the command, space-separated).
    #[tokio::test]
    #[ignore = "needs an iOS Simulator and a built device engine"]
    async fn simulator_end_to_end() {
        use extend_driver::cancel::CancelToken;
        let udid = std::env::var("EXTEND_HOSTED_SIM_UDID").expect("set EXTEND_HOSTED_SIM_UDID");
        assert!(simulators_allowed(), "set {SIMULATOR_ENV}=1");
        let agent_device: Vec<String> =
            match std::env::var("EXTEND_HOSTED_ENGINE").or_else(|_| std::env::var("EXTEND_HOSTED_AGENT_DEVICE")) {
                Ok(v) => v.split_whitespace().map(str::to_owned).collect(),
                Err(_) => vec![
                    "node".into(),
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../vendor/extend-engine/bin/extend-engine.mjs")
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
            "probe: online={} model={:?} os={:?} engine={:?} setup={:?}",
            p.online, p.model, p.os_version, p.engine_version, p.setup.state
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
        let out = run("open", s(&["com.apple.Preferences", "--relaunch"]), work.join("1")).await;
        assert!(out.ok, "{out:?}");
        let mut out = run("snapshot", s(&["-i"]), work.join("2")).await;
        assert!(out.ok, "{out:?}");
        let general = |out: &Output, kind: &str| {
            out.output["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["label"] == "General" && n["type"] == kind)
                .and_then(|n| n["ref"].as_str())
                .map(|r| format!("@{r}"))
        };
        if general(&out, "Cell").is_none() {
            // Settings reopened on the page an earlier run left it on.
            assert!(run("back", s(&[]), work.join("2b")).await.ok);
            out = run("snapshot", s(&["-i"]), work.join("2c")).await;
            assert!(out.ok, "{out:?}");
        }
        // the device engine refuses the row's own ref (its child controls cover its touch point), and on
        // some models the row and its button both carry the label: the button's ref, else the label.
        let target = general(&out, "Button").unwrap_or_else(|| "label=\"General\"".into());
        let out = run("click", vec![target], work.join("3")).await;
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
        let out = run("record", s(&["start", "clip", "--scope", "device"]), work.join("7")).await;
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
    fn knows_which_commands_need_the_runner() {
        use RunnerUse::{Free, Needs, Warms};
        // Served over CoreDevice on an iPhone: no runner, no "Automation Running".
        for (command, args) in [
            ("apps", vec![]),
            ("appstate", vec![]),
            ("install", vec!["attachment:app.ipa"]),
            ("reinstall", vec!["com.example", "attachment:app.ipa"]),
            ("logs", vec!["path"]),
            ("close", vec![]),
            ("close", vec!["Settings"]),
            ("screenshot", vec![]),
            ("screenshot", vec!["home", "--fullscreen"]),
            ("diff", vec!["screenshot", "--baseline", "base.png"]),
            ("open", vec![]),
        ] {
            assert_eq!(runner_use(command, &s(&args)), Free, "{command} {args:?}");
        }
        // Launched over CoreDevice; the device engine warms the runner for the next read.
        assert_eq!(runner_use("open", &s(&["Settings"])), Warms);
        assert_eq!(runner_use("open", &s(&["https://example.com"])), Warms);
        assert_eq!(runner_use("open", &s(&["--relaunch"])), Warms);
        // Reading the screen's elements or touching it needs the runner.
        for (command, args) in [
            ("snapshot", vec!["-i"]),
            ("screenshot", vec!["--overlay-refs"]),
            ("screenshot", vec!["--crop-on", "label=\"General\""]),
            ("diff", vec!["snapshot"]),
            ("diff", vec!["screenshot", "--baseline", "b.png", "--overlay-refs"]),
            ("click", vec!["@e9"]),
            ("fill", vec!["@e3", "hello"]),
            ("type", vec!["--", "--overlay-refs"]),
            ("press", vec!["@e2"]),
            ("longpress", vec!["100", "200"]),
            ("scroll", vec!["down"]),
            ("swipe", vec!["up"]),
            ("back", vec![]),
            ("home", vec![]),
            ("get", vec!["text", "@e1"]),
            ("find", vec!["Continue", "click"]),
            ("is", vec!["visible", "@e1"]),
            ("wait", vec!["text", "Done"]),
            ("alert", vec!["accept"]),
            ("keyboard", vec!["dismiss"]),
            ("record", vec!["start"]),
            ("replay", vec!["flow.ad"]),
            ("batch", vec!["--steps", "[]"]),
        ] {
            assert_eq!(runner_use(command, &s(&args)), Needs, "{command} {args:?}");
        }
    }

    const PHONE: &str = "00008110-001E0DAC02C2401E";
    const PAD: &str = "00008132-000A18A22E9A401C";
    const SIM: &str = "50F5AD4C-4D26-445D-A2CA-9D04D9FFD768";

    fn runner_line(pid: u32, etime: &str, dest: &str, udid: &str) -> String {
        format!(
            "{pid:>6}     1 {etime:>11} /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild test-without-building -only-testing AgentDeviceRunnerUITests/RunnerTests/testCommand -parallel-testing-enabled NO -xctestrun /Users/c/.agent-device/apple-runner/derived/ios-device/cache-1/Build/Products/AgentDeviceRunner.env.session-{udid}-owner-5807-f552a997-61133.xctestrun -destination {dest},id={udid} -derivedDataPath /Users/c/.agent-device/apple-runner/derived/ios-device/cache-1"
        )
    }

    /// The Simulator's runner app, as `ps` shows it (under the Simulator's launchd, pid 900).
    fn sim_app_line(pid: u32, udid: &str) -> String {
        format!(
            "{pid:>6}   900       00:40 /Users/c/Library/Developer/CoreSimulator/Devices/{udid}/data/Containers/Bundle/Application/B9F78B83-10BB-4540-B86D-FE735B35BA5C/AgentDeviceRunnerUITests-Runner.app/AgentDeviceRunnerUITests-Runner"
        )
    }

    fn ps_listing() -> String {
        [
            runner_line(101, "01:10", "platform=iOS", PHONE),
            format!("  102   101       01:09 /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild-helper --for {PHONE}"),
            runner_line(201, "1-02:03:04", "platform=iOS", PAD),
            runner_line(301, "00:41", "platform=iOS Simulator", SIM),
            sim_app_line(302, SIM),
            format!("   900     1    01:00:00 launchd_sim /Users/c/Library/Developer/CoreSimulator/Devices/{SIM}/data/var/run/launchd_bootstrap.plist"),
            // Someone's own tests on the iPhone, and a UDID that only starts like the iPhone's.
            format!("  401     1       00:05 /usr/bin/xcodebuild test-without-building -scheme MyApp -destination platform=iOS,id={PHONE}"),
            runner_line(501, "00:05", "platform=iOS", &format!("{PHONE}0")),
            format!("  601     1       00:05 /usr/bin/vim notes-id={PHONE}.txt"),
            "garbage line".to_owned(),
        ]
        .join("\n")
    }

    fn lease(pid: u32, session: &str) -> RunnerLease {
        RunnerLease {
            runner_pid: Some(pid),
            session: Some(session.into()),
            log: Some(PathBuf::from(format!(
                "/Users/c/.agent-device/sessions/{session}/runner.log"
            ))),
            port: Some(61133),
        }
    }

    #[test]
    fn finds_exactly_this_devices_runner() {
        let procs = parse_ps(&ps_listing());
        assert_eq!(procs.len(), 9);
        assert_eq!((procs[1].pid, procs[1].ppid), (102, 101));
        assert_eq!(procs[0].age, Some(Duration::from_secs(70)));
        assert_eq!(procs[2].age, Some(Duration::from_secs(86_400 + 2 * 3600 + 3 * 60 + 4)));
        assert!(procs[0].command.starts_with("/Applications/Xcode.app"));
        // The runner's xcodebuild and what runs under it (and a Simulator's runner app, which runs
        // under the Simulator's launchd); never another device's runner or a project's own tests.
        assert_eq!(runner_processes(&procs, PHONE, None), vec![101, 102]);
        assert_eq!(runner_processes(&procs, PAD, None), vec![201]);
        assert_eq!(runner_processes(&procs, SIM, None), vec![301, 302]);
        assert!(runner_processes(&procs, "00008110-000000000000FA4E", None).is_empty());
        assert!(runner_processes(&procs, "", None).is_empty());
        // the device engine's lease says who started the runner: an Extend session's is Extend's to
        // stop, one started outside Extend is left alone (its app too), and one the lease no
        // longer names is an orphan.
        assert_eq!(
            runner_processes(&procs, PHONE, Some(&lease(101, "extend-setup"))),
            vec![101, 102]
        );
        assert!(runner_processes(&procs, PHONE, Some(&lease(101, "default"))).is_empty());
        assert!(runner_processes(&procs, SIM, Some(&lease(301, "default"))).is_empty());
        assert_eq!(
            runner_processes(&procs, PHONE, Some(&lease(999, "default"))),
            vec![101, 102]
        );
        // A runner app whose xcodebuild is gone is still this Simulator's runner.
        let orphan = parse_ps(&sim_app_line(302, SIM));
        assert_eq!(runner_processes(&orphan, SIM, None), vec![302]);
        assert_eq!(
            parse_lease(&json!({"runnerPid": 13206, "port": 61133,
                "runnerLogPath": "/Users/c/.agent-device/sessions/extend-setup/runner.log"})),
            RunnerLease {
                runner_pid: Some(13206),
                ..lease(0, "extend-setup")
            }
        );
        assert_eq!(parse_lease(&json!({})), RunnerLease::default());
        assert!(is_simulator_udid(SIM) && !is_simulator_udid(PHONE) && !is_simulator_udid(PAD));
        assert_eq!(parse_etime("05"), Some(Duration::from_secs(5)));
        assert_eq!(parse_etime("12:34:56"), Some(Duration::from_secs(45_296)));
        assert_eq!(parse_etime("x:1"), None);
    }

    #[test]
    fn never_stops_a_runner_agent_device_is_still_starting() {
        use RunnerNow::{Gone, Starting, Up};
        let at = |etime: &str| parse_ps(&runner_line(101, etime, "platform=iOS", PHONE));
        let ours = lease(101, "extend-a1");
        let log = |listening, answered| Some(RunnerLog { listening, answered });
        // Just launched: the device engine may be about to connect to it.
        assert_eq!(
            classify_runner(&at("00:05"), PHONE, Some(&ours), log(false, false)),
            Starting
        );
        assert_eq!(
            classify_runner(&at("00:05"), PHONE, Some(&ours), log(true, false)),
            Starting
        );
        assert_eq!(classify_runner(&at("00:05"), PHONE, None, None), Starting);
        // It answered a command, so the device engine has connected to it.
        assert_eq!(
            classify_runner(&at("00:05"), PHONE, Some(&ours), log(true, true)),
            Up(vec![101])
        );
        // It has listened for a while (a start whose runner no command has asked yet).
        assert_eq!(
            classify_runner(&at("00:45"), PHONE, Some(&ours), log(true, false)),
            Up(vec![101])
        );
        // Not listening yet, but still within the device engine's wait for it.
        assert_eq!(
            classify_runner(&at("00:45"), PHONE, Some(&ours), log(false, false)),
            Starting
        );
        // Long past any start, or left behind with no lease naming it.
        assert_eq!(
            classify_runner(&at("03:00"), PHONE, Some(&ours), log(false, false)),
            Up(vec![101])
        );
        assert_eq!(classify_runner(&at("00:45"), PHONE, None, None), Up(vec![101]));
        // A lease for another runner says nothing about this one.
        assert_eq!(
            classify_runner(&at("00:45"), PHONE, Some(&lease(999, "extend-a1")), log(false, false)),
            Up(vec![101])
        );
        assert_eq!(classify_runner(&[], PHONE, Some(&ours), None), Gone);
        // the device engine building a Simulator's runner is a start under way, whatever else runs.
        let mut building = parse_ps(&sim_app_line(302, SIM));
        building.extend(parse_ps(&format!(
            "  700  1  00:20 /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild build-for-testing -project /x/AgentDeviceRunner.xcodeproj -scheme AgentDeviceRunner -destination platform=iOS Simulator,id={SIM} -derivedDataPath /x"
        )));
        assert_eq!(classify_runner(&building, SIM, None, None), Starting);
        assert_eq!(classify_runner(&building[..1], SIM, None, None), Up(vec![302]));

        // The runner's log: the last start on the lease's port, matched whole.
        let text = "AGENT_DEVICE_RUNNER_PORT=6113\nAGENT_DEVICE_RUNNER_COMMAND_COMPLETED command=tap\n** BUILD INTERRUPTED **\nAGENT_DEVICE_RUNNER_PORT=61133\n";
        assert_eq!(
            runner_log_state(text, 61133),
            RunnerLog {
                listening: true,
                answered: false
            }
        );
        let text = format!("{text}AGENT_DEVICE_RUNNER_COMMAND_COMPLETED command=snapshot ok=1\n");
        assert_eq!(
            runner_log_state(&text, 61133),
            RunnerLog {
                listening: true,
                answered: true
            }
        );
        assert!(runner_log_state(&text, 6113).listening);
        assert_eq!(runner_log_state(&text, 611), RunnerLog::default());
    }

    #[test]
    fn stops_a_runner_nothing_uses() {
        let secs = |s| Some(Duration::from_secs(s));
        // No session live: any runner is left over.
        assert!(runner_should_stop(false, secs(1), false));
        assert!(runner_should_stop(false, None, false));
        // Inside a live session: once no command has used it for a minute.
        assert!(!runner_should_stop(true, secs(10), false));
        assert!(runner_should_stop(true, secs(60), false));
        // Nothing has used it since Extend started (a runner from before).
        assert!(runner_should_stop(true, None, false));
        // Never while it records the screen.
        assert!(!runner_should_stop(true, secs(600), true));
    }

    #[test]
    fn setup_only_installs_on_a_device_that_answered_without_the_helper() {
        let idle = Prepare::Idle;
        let locked: Result<bool, String> = Err("The device is locked.".into());
        // A check that fails (locked, out of reach) never starts `prepare`, which starts the runner.
        assert_eq!(helper_next(&locked, &idle, true, false), HelperNext::Done);
        assert_eq!(
            helper_next(&locked, &idle, false, false),
            HelperNext::Unchecked("The device is locked.".into())
        );
        assert_eq!(helper_next(&Ok(true), &idle, false, false), HelperNext::Done);
        assert_eq!(helper_next(&Ok(false), &idle, true, false), HelperNext::Install);
        assert_eq!(
            helper_next(&Ok(false), &Prepare::Done, true, false),
            HelperNext::Install
        );
        // Never under a Silicon's live session, and never twice at once.
        assert_eq!(helper_next(&Ok(false), &idle, true, true), HelperNext::AfterSession);
        assert_eq!(
            helper_next(&Ok(false), &Prepare::Running, false, false),
            HelperNext::Installing
        );
        assert_eq!(
            helper_next(&locked, &Prepare::Running, false, false),
            HelperNext::Installing
        );
        // A failed install is shown for a while, then tried again.
        let failed = Prepare::Failed("no signing".into(), Instant::now());
        assert_eq!(
            helper_next(&Ok(false), &failed, false, false),
            HelperNext::ShowFailure("no signing".into())
        );
        if let Some(earlier) = Instant::now().checked_sub(PREPARE_RETRY_AFTER + Duration::from_secs(1)) {
            let failed = Prepare::Failed("no signing".into(), earlier);
            assert_eq!(helper_next(&Ok(false), &failed, false, false), HelperNext::Install);
        }
        // `prepare` runs in a setup session of its own that is closed right after it.
        let calls: Vec<&str> = helper_install_calls().iter().map(|(c, _)| *c).collect();
        assert_eq!(calls, ["close", "open", "prepare", "close"]);
        assert_eq!(
            helper_install_calls()[1].1,
            Vec::<String>::new(),
            "a bare open starts no runner"
        );
        assert_eq!(helper_install_calls()[2].1[0], "ios-runner");
    }

    /// A stand-in for the device engine that logs each call (with the runner settings it was started
    /// with) and answers success. While its `held` file exists it plays a daemon whose device the
    /// setup session still claims: everything but a `close` is refused `DEVICE_IN_USE`, as
    /// the device engine refuses a session of the same daemon, until that session is closed.
    #[cfg(unix)]
    struct FakeAgentDevice {
        dir: tempfile::TempDir,
        driver: IosDriver,
        /// Each fake its own device, since the device guard is shared by every driver in the process.
        udid: String,
    }

    #[cfg(unix)]
    impl FakeAgentDevice {
        fn new() -> Self {
            Self::build(false, false)
        }

        fn with_setup_held(held: bool) -> Self {
            Self::build(held, false)
        }

        /// `prepare` never finishes (a first install takes minutes).
        fn with_slow_prepare() -> Self {
            Self::build(false, true)
        }

        fn build(held: bool, slow_prepare: bool) -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
            let udid = format!(
                "00008110-0000FA4E{:08X}",
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("calls.log");
            let held_file = dir.path().join("held");
            if held {
                std::fs::write(&held_file, "").unwrap();
            }
            let refusal = json!({"success": false, "error": {"code": "DEVICE_IN_USE",
                "message": format!("Device is already in use by session \"{SETUP_SESSION}\"."),
                "details": {"session": SETUP_SESSION, "deviceId": udid}}});
            let script = dir.path().join("extend-engine.sh");
            std::fs::write(
                &script,
                format!(
                    r#"#!/bin/sh
printf '%s|%s|%s\n' "$EXTEND_ENGINE_IOS_RUNNER_IDLE_STOP_MS" "$EXTEND_ENGINE_IOS_RUNNER_DETACH" "$*" >> '{log}'
if [ "$1" = prepare ] && [ {slow} = 1 ]; then sleep 60; fi
if [ -f '{held}' ]; then
  case "$*" in
    "close "*"--session {setup}"*) rm -f '{held}' ;;
    "close "*) ;;
    *) echo '{refusal}'; exit 1 ;;
  esac
fi
echo '{{"success":true,"data":{{"message":"ok"}}}}'
"#,
                    log = log.display(),
                    held = held_file.display(),
                    setup = SETUP_SESSION,
                    slow = u8::from(slow_prepare),
                ),
            )
            .unwrap();
            let driver = IosDriver::new(HostedDevice {
                device_id: "dev_phone".into(),
                os: DeviceOs::Ios,
                name: "Test iPhone".into(),
                address: Some(udid.clone()),
                state_dir: dir.path().join("state"),
                agent_device: vec!["/bin/sh".into(), script.to_string_lossy().into_owned()],
            });
            Self { dir, driver, udid }
        }

        /// Each call as `(command, --session value)`, checking the runner settings it carried.
        fn calls(&self) -> Vec<(String, String)> {
            let expect = |name: &str, default: &str| engine_setting(name).unwrap_or_else(|| default.into());
            let settings = format!(
                "{}|{}|",
                expect("IOS_RUNNER_IDLE_STOP_MS", "30000"),
                expect("IOS_RUNNER_DETACH", "0")
            );
            std::fs::read_to_string(self.dir.path().join("calls.log"))
                .unwrap_or_default()
                .lines()
                .map(|line| {
                    let argv = line
                        .strip_prefix(&settings)
                        .unwrap_or_else(|| panic!("{line:?} lacks {settings:?}"));
                    let words: Vec<&str> = argv.split(' ').collect();
                    let session = words
                        .iter()
                        .position(|w| *w == "--session")
                        .map(|i| words[i + 1].to_owned())
                        .unwrap_or_default();
                    assert!(argv.contains(&format!("--platform ios --udid {}", self.udid)), "{argv}");
                    (words[0].to_owned(), session)
                })
                .collect()
        }

        fn open_sessions(&self) -> BTreeMap<String, OpenSession> {
            self.driver.inner.read_saved().sessions
        }

        async fn run(&self, session: &str, command: &'static str, args: &[&str]) -> Output {
            let work = self
                .dir
                .path()
                .join("work")
                .join(uuid::Uuid::new_v4().simple().to_string());
            let args = s(args);
            self.driver
                .run(Invocation {
                    id: uuid::Uuid::new_v4(),
                    session_id: session,
                    command,
                    args: &args,
                    attachments: &[],
                    workdir: &work,
                    timeout: Duration::from_secs(30),
                    cancel: extend_driver::cancel::CancelToken::new(),
                })
                .await
        }
    }

    #[cfg(unix)]
    fn call(command: &str, session: &str) -> (String, String) {
        (command.to_owned(), session.to_owned())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_end_closes_its_agent_device_session() {
        let fake = FakeAgentDevice::new();
        assert!(fake.run("a1", "open", &["Settings"]).await.ok);
        assert!(fake.run("a1", "snapshot", &["-i"]).await.ok);
        assert!(fake.run("b2", "apps", &[]).await.ok);
        let open = fake.open_sessions();
        assert_eq!(open.keys().collect::<Vec<_>>(), ["extend-a1", "extend-b2"]);
        assert!(open["extend-a1"].runner && !open["extend-b2"].runner);

        fake.driver.session_ended("a1").await;
        assert_eq!(fake.calls().last(), Some(&call("close", "extend-a1")));
        assert_eq!(fake.open_sessions().keys().collect::<Vec<_>>(), ["extend-b2"]);

        // The device is no longer carried (or the service has no session on it): every session
        // this driver has open is closed.
        assert!(fake.run("c3", "screenshot", &[]).await.ok);
        fake.driver.session_ended("").await;
        let calls = fake.calls();
        let closes: Vec<_> = calls[calls.len() - 2..].to_vec();
        assert_eq!(closes, [call("close", "extend-b2"), call("close", "extend-c3")]);
        assert!(fake.open_sessions().is_empty());

        // A session that ran nothing is still closed when it ends.
        fake.driver.session_ended("d4").await;
        assert_eq!(fake.calls().last(), Some(&call("close", "extend-d4")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_closes_the_session_prepare_ran_in() {
        let fake = FakeAgentDevice::new();
        fake.driver.inner.install_helper(&fake.udid).await.unwrap();
        assert_eq!(
            fake.calls(),
            [
                call("close", SETUP_SESSION),
                call("open", SETUP_SESSION),
                call("prepare", SETUP_SESSION),
                call("close", SETUP_SESSION)
            ]
        );
        // Setup leaves no session open, and none a Silicon's would have to wait for.
        assert!(fake.open_sessions().is_empty());
        assert!(!fake.driver.inner.read_saved().setup_open);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_whose_end_never_arrived_is_closed_once_it_has_certainly_ended() {
        let fake = FakeAgentDevice::new();
        assert!(fake.run("old", "snapshot", &[]).await.ok);
        assert!(fake.run("new", "snapshot", &[]).await.ok);
        let long_ago = now_ms() - STALE_SESSION_AFTER.as_millis() as u64 - 1_000;
        fake.driver
            .inner
            .update_saved(|s| s.sessions.get_mut("extend-old").unwrap().last_used_ms = long_ago);
        fake.driver.inner.tend().await;
        assert_eq!(fake.calls().last(), Some(&call("close", "extend-old")));
        assert_eq!(fake.open_sessions().keys().collect::<Vec<_>>(), ["extend-new"]);
        // A live session is never closed by the backstop.
        let before = fake.calls().len();
        fake.driver.inner.tend().await;
        assert_eq!(fake.calls().len(), before);
        // A takeover starting or ending keeps a session no command has used from the backstop.
        fake.driver
            .inner
            .update_saved(|s| s.sessions.get_mut("extend-new").unwrap().last_used_ms = long_ago);
        fake.driver.session_active("new").await;
        fake.driver.inner.tend().await;
        assert_eq!(fake.calls().len(), before);
        assert_eq!(fake.open_sessions().keys().collect::<Vec<_>>(), ["extend-new"]);
        // …and never makes up a session that ran nothing.
        fake.driver.session_active("never").await;
        assert_eq!(fake.open_sessions().keys().collect::<Vec<_>>(), ["extend-new"]);
    }

    /// Extend stopped between the helper install's `open` and its final `close` (the Carbon quit
    /// during a first install that takes minutes): the setup session still claims the device in
    /// the device engine's daemon, which refuses every other session `DEVICE_IN_USE` and never idles out.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_setup_session_left_open_is_recovered() {
        // A Silicon's command finds the device claimed by it: the setup session is closed and the
        // command runs.
        let fake = FakeAgentDevice::with_setup_held(true);
        let out = fake.run("s1", "open", &["com.apple.Preferences"]).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(
            fake.calls(),
            [
                call("open", "extend-s1"),
                call("close", SETUP_SESSION),
                call("open", "extend-s1")
            ]
        );

        // Extend stops in the middle of an install: the setup session is noted as open.
        let fake = FakeAgentDevice::with_slow_prepare();
        let install = fake.driver.inner.install_helper(&fake.udid);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), install).await.is_err(),
            "prepare still running"
        );
        assert_eq!(
            fake.calls(),
            [
                call("close", SETUP_SESSION),
                call("open", SETUP_SESSION),
                call("prepare", SETUP_SESSION)
            ]
        );
        assert!(fake.driver.inner.read_saved().setup_open);

        // Found before any command (the next launch, say): the look after the device closes it.
        let fake = FakeAgentDevice::with_setup_held(true);
        fake.driver.inner.update_saved(|s| s.setup_open = true);
        fake.driver.inner.tend().await;
        assert_eq!(fake.calls(), [call("close", SETUP_SESSION)]);
        assert!(!fake.driver.inner.read_saved().setup_open);
        assert!(fake.run("s2", "snapshot", &[]).await.ok);
        // Closed once: the next look has nothing to close.
        let before = fake.calls().len();
        fake.driver.inner.tend().await;
        assert_eq!(fake.calls().len(), before);
    }

    /// "Automation Running" leaves the device soon after the Silicon's last action, on a real iOS
    /// Simulator: the runner runs while a session reads and touches the screen; the driver's own
    /// look after the device (in the background, as in the app; no probe) stops it once no command
    /// has used it for a minute, and the session's next command starts it again with the session's
    /// app still bound; a session's end stops it at once; runner-free commands start none; a session
    /// that ends while the device engine is still starting the runner (an `open` and nothing else) leaves
    /// none behind and none comes back; installing the helper leaves none. A monitor prints every
    /// change of this Simulator's runner processes (`build` = the device engine building the runner,
    /// `xcodebuild` = the runner, `app` = the runner app inside the Simulator). Run it with an
    /// device-engine state of its own, so it never shares a daemon with the Extend app:
    /// `EXTEND_HOSTED_ALLOW_SIMULATOR=1 EXTEND_HOSTED_SIM_UDID=<udid> EXTEND_ENGINE_STATE_DIR=<dir>
    /// EXTEND_ENGINE_IOS_RUNNER_LEASE_DIR=<dir>/leases EXTEND_ENGINE_CLAIMS_DIR=<dir>/claims
    /// cargo test -p extend-hosted -- --ignored --nocapture simulator_runner` (about 7 minutes).
    #[tokio::test]
    #[ignore = "needs an iOS Simulator and a built device engine"]
    async fn simulator_runner_lifecycle() {
        use extend_driver::cancel::CancelToken;
        let udid = std::env::var("EXTEND_HOSTED_SIM_UDID").expect("set EXTEND_HOSTED_SIM_UDID");
        assert!(simulators_allowed(), "set {SIMULATOR_ENV}=1");
        assert!(
            engine_setting("STATE_DIR").is_some(),
            "set EXTEND_ENGINE_STATE_DIR so the test has a daemon of its own"
        );
        let agent_device: Vec<String> =
            match std::env::var("EXTEND_HOSTED_ENGINE").or_else(|_| std::env::var("EXTEND_HOSTED_AGENT_DEVICE")) {
                Ok(v) => v.split_whitespace().map(str::to_owned).collect(),
                Err(_) => vec![
                    "node".into(),
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../vendor/extend-engine/bin/extend-engine.mjs")
                        .to_string_lossy()
                        .into_owned(),
                ],
            };
        let t0 = Instant::now();
        let at = move || format!("+{:.1}s", t0.elapsed().as_secs_f64());
        // Every change of this Simulator's runner processes, and when the last one was seen.
        let last_seen: Arc<Mutex<Option<Instant>>> = Arc::default();
        {
            let (udid, last_seen) = (udid.clone(), last_seen.clone());
            tokio::spawn(async move {
                let mut last = String::from("?");
                loop {
                    let procs = processes().await.unwrap_or_default();
                    let mut now: Vec<String> = procs
                        .iter()
                        .filter_map(|p| {
                            let kind = if is_runner_build_for(&p.command, &udid) {
                                "build"
                            } else if is_runner_for(&p.command, &udid) {
                                "xcodebuild"
                            } else if is_simulator_runner_app_for(&p.command, &udid) {
                                "app"
                            } else {
                                return None;
                            };
                            Some(format!("{kind} {}", p.pid))
                        })
                        .collect();
                    now.sort();
                    let now = now.join(", ");
                    if !now.is_empty() {
                        *last_seen.lock().unwrap() = Some(Instant::now());
                    }
                    if now != last {
                        println!("[monitor {}] {}", at(), if now.is_empty() { "none" } else { &now });
                        last = now;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        }
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
        assert!(
            p.online && p.setup.state == extend_protocol::model::SetupState::Complete,
            "{p:?}"
        );
        let work = state.path().join("work");
        let run = |session: String, command: &'static str, args: Vec<String>| {
            let d = &d;
            let work = work.join(uuid::Uuid::new_v4().simple().to_string());
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
                    "[{}] $ {command} {}  ({} ms, ok={}) {}",
                    at(),
                    args.join(" "),
                    t.elapsed().as_millis(),
                    out.ok,
                    out.text.clone().unwrap_or_default().lines().next().unwrap_or_default()
                );
                out
            }
        };
        let runners = || async { runner_pids(&udid).await.len() };
        // Waits up to `limit` for no runner; returns how long that took.
        let gone_within = |limit: Duration| {
            let udid = udid.clone();
            async move {
                let t = Instant::now();
                while !runner_pids(&udid).await.is_empty() {
                    if t.elapsed() > limit {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Some(t.elapsed())
            }
        };
        // Whatever an earlier run left goes first (the driver's look after the device).
        assert!(
            gone_within(Duration::from_secs(150)).await.is_some(),
            "a runner an earlier run left is stopped"
        );
        println!("[{}] no runner to start with", at());

        // 1. A session that reads and touches the screen runs the runner.
        let a = format!("rl{}", std::process::id());
        assert!(
            run(a.clone(), "open", s(&["com.apple.Preferences", "--relaunch"]))
                .await
                .ok
        );
        assert!(run(a.clone(), "snapshot", s(&["-i"])).await.ok);
        // A touch (Settings reopens on whatever page it was left on; a scroll works on any).
        assert!(run(a.clone(), "scroll", s(&["down"])).await.ok);
        assert!(run(a.clone(), "snapshot", s(&["-i"])).await.ok);
        println!("[{}] during the session: {} runner process(es)", at(), runners().await);
        assert!(runners().await > 0, "the session's runner runs");

        // 2. No command for a minute: the runner goes though the session is live, and the session's
        // next command starts it again, with its app still bound.
        let idle = gone_within(RUNNER_IDLE_STOP + TEND_EVERY + Duration::from_secs(15)).await;
        println!("[{}] runner gone {idle:?} after the last command", at());
        let idle = idle.expect("the runner stops once no command has used it for a minute");
        assert!(idle >= RUNNER_IDLE_STOP - Duration::from_secs(1), "{idle:?}");
        assert!(d.inner.read_saved().sessions.contains_key(&session_name(&a)));
        let out = run(a.clone(), "scroll", s(&["up"])).await;
        assert!(out.ok, "{out:?}");
        let out = run(a.clone(), "snapshot", s(&["-i"])).await;
        assert!(out.ok, "{out:?}");
        assert!(
            out.text.unwrap_or_default().contains("App: com.apple.Preferences"),
            "the session kept its app"
        );

        // 3. Its end closes the device engine session, and the runner goes with it.
        let t = Instant::now();
        d.session_ended(&a).await;
        println!("[{}] session_ended took {} ms", at(), t.elapsed().as_millis());
        assert_eq!(runners().await, 0, "no runner once the session ended");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(runners().await, 0, "and none comes back");

        // 4. Runner-free commands start none. (`screenshot` needs a device-engine session; a bare
        // `open` makes one without launching anything.)
        let b = format!("rf{}", std::process::id());
        for (command, args) in [("apps", s(&[])), ("open", s(&[])), ("screenshot", s(&["home"]))] {
            assert_eq!(runner_use(command, &args), RunnerUse::Free);
            assert!(run(b.clone(), command, args).await.ok);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(runners().await, 0, "runner-free commands start no runner");
        d.session_ended(&b).await;

        // 5. A session that opens an app and ends at once, while the device engine is still starting the
        // runner the `open` warms: the start is never cut short (the device engine would build and start
        // another with no session), the runner goes once started, and none comes back.
        let c = format!("qe{}", std::process::id());
        assert!(run(c.clone(), "open", s(&["com.apple.Preferences"])).await.ok);
        let t = Instant::now();
        d.session_ended(&c).await;
        println!(
            "[{}] quick session ended ({} ms): {} runner process(es)",
            at(),
            t.elapsed().as_millis(),
            runners().await
        );
        let ended = Instant::now();
        tokio::time::sleep(Duration::from_secs(150)).await;
        let last = (*last_seen.lock().unwrap()).filter(|t| *t > ended);
        let last = last.map(|t| t.duration_since(ended));
        println!(
            "[{}] after the quick session: runner last seen {last:?} after its end",
            at()
        );
        assert_eq!(runners().await, 0, "no runner is left behind");
        assert!(
            last.is_none_or(|l| l < Duration::from_secs(90)),
            "the runner goes within 90 s of the end, and none comes back: {last:?}"
        );

        // 6. Installing the helper proves it with the runner, then leaves none.
        let t = Instant::now();
        let installed = d.inner.install_helper(&udid).await;
        println!(
            "[{}] helper install: {installed:?} ({} ms)",
            at(),
            t.elapsed().as_millis()
        );
        assert!(installed.is_ok(), "{installed:?}");
        let gone = gone_within(Duration::from_secs(60)).await;
        println!("[{}] runner gone {gone:?} after the helper install", at());
        assert!(gone.is_some(), "the helper install leaves no runner");
        assert!(!d.inner.read_saved().setup_open);
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe_name(None, "screenshot", "png"), "screenshot.png");
        assert_eq!(safe_name(Some("a b.PNG"), "x", "png"), "a b.PNG");
        assert_eq!(safe_name(Some("../../.hidden"), "x", "png"), "hidden.png");
        assert_eq!(safe_name(Some("x/y/rec"), "r", "mp4"), "rec.mp4");
    }
}
