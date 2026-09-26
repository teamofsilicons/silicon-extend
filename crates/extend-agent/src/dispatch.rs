//! Runs commands and session lifecycle hooks in order per device. Commands honor their deadlines
//! and cancellation, with produced files uploaded before the `result` goes back.
//!
//! Lifecycle hooks (a driver's session setup and cleanup) have their own time limits, so a stuck
//! cleanup can hold up the next session's commands for a bounded time only. Once a session has
//! ended, a command that still arrives for it is answered `session_ended` without running.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use extend_driver::cancel::CancelToken;
use extend_driver::{Driver, Invocation, Output};
use extend_protocol::capability::{command as command_spec, not_exposed};
use extend_protocol::frames::{CommandFrame, CommandOutcome, DeviceFrame, ProducedFile};
use extend_protocol::model::CommandError;
use extend_protocol::{COMMAND_TIMEOUT_MAX_MS, COMMAND_TIMEOUT_MIN_MS, DeviceId};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::drivers::args::{find_refused_flag, refused_flag_message, safe_file_name};

/// Where outgoing frames go: the live connection, swapped on every reconnect.
#[derive(Clone, Default)]
pub struct Outbox(Arc<Mutex<Option<mpsc::UnboundedSender<DeviceFrame>>>>);

impl Outbox {
    pub fn set(&self, tx: mpsc::UnboundedSender<DeviceFrame>) {
        *self.0.lock().unwrap() = Some(tx);
    }
    pub fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
    /// Sends on the live connection. False when there is none (the frame is dropped: the service
    /// has already answered `device_offline` for anything in flight).
    pub fn send(&self, frame: DeviceFrame) -> bool {
        match self.0.lock().unwrap().as_ref() {
            Some(tx) => tx.send(frame).is_ok(),
            None => false,
        }
    }
}

/// Finds the driver for a command's `target`: `None` is this computer, a device id is a device
/// this computer carries.
pub trait DriverLookup: Send + Sync {
    fn driver_for(&self, target: Option<&DeviceId>) -> Result<Arc<dyn Driver>, String>;
}

/// Uploads one produced file; returns its size in bytes.
#[async_trait]
pub trait Uploader: Send + Sync {
    async fn upload(&self, upload_id: Uuid, path: &Path, name: &str, content_type: &str) -> Result<u64, String>;
}

type QueueKey = Option<DeviceId>;

enum Work {
    Command(Job),
    Session {
        driver: Arc<dyn Driver>,
        session_id: String,
        starting: bool,
        done: Option<oneshot::Sender<()>>,
    },
}

/// A lifecycle hook a device's queue ran recently, so a command whose deadline passed while it
/// waited can say what it waited for.
struct RecentHook {
    session_id: String,
    starting: bool,
    started: Instant,
    finished: Instant,
}

/// Recent hooks kept per device queue.
const RECENT_HOOKS: usize = 4;

struct Pending {
    target: Option<DeviceId>,
    session_id: String,
    cancel: CancelToken,
}

struct Job {
    frame: CommandFrame,
    cancel: CancelToken,
    received: Instant,
}

struct Inner {
    lookup: Arc<dyn DriverLookup>,
    uploader: Arc<dyn Uploader>,
    outbox: Outbox,
    work_root: PathBuf,
    queues: Mutex<HashMap<QueueKey, mpsc::UnboundedSender<Work>>>,
    pending: Mutex<HashMap<Uuid, Pending>>,
    /// Sessions that have ended, newest last (at most [`ENDED_REMEMBERED`]).
    ended: Mutex<VecDeque<(QueueKey, String)>>,
    setup_limit: Duration,
    cleanup_limit: Duration,
}

#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<Inner>,
}

/// Seconds a cancelled or timed-out driver gets to wind down before its result is written off.
const WIND_DOWN: Duration = Duration::from_secs(3);

/// How long a driver's session setup may take before the device moves on. The local agent-device
/// driver needs up to about two minutes when it first has to release an earlier session.
pub const SETUP_LIMIT: Duration = Duration::from_secs(150);
/// How long a driver's session cleanup may take before the device moves on. Stopping a recording
/// and closing the session can take 90 s each on a carried iPhone.
pub const CLEANUP_LIMIT: Duration = Duration::from_secs(180);

/// Ended sessions remembered to refuse late commands. Session ids are never reused.
const ENDED_REMEMBERED: usize = 256;

impl Dispatcher {
    pub fn new(lookup: Arc<dyn DriverLookup>, uploader: Arc<dyn Uploader>, outbox: Outbox, work_root: PathBuf) -> Self {
        Self::with_lifecycle_limits(lookup, uploader, outbox, work_root, SETUP_LIMIT, CLEANUP_LIMIT)
    }

    /// A dispatcher whose session setup and cleanup hooks get other time limits (tests).
    pub fn with_lifecycle_limits(
        lookup: Arc<dyn DriverLookup>,
        uploader: Arc<dyn Uploader>,
        outbox: Outbox,
        work_root: PathBuf,
        setup_limit: Duration,
        cleanup_limit: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                lookup,
                uploader,
                outbox,
                work_root,
                queues: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                ended: Mutex::new(VecDeque::new()),
                setup_limit,
                cleanup_limit,
            }),
        }
    }

    fn has_ended(&self, target: Option<&DeviceId>, session_id: &str) -> bool {
        self.inner
            .ended
            .lock()
            .unwrap()
            .iter()
            .any(|(t, s)| t.as_ref() == target && s == session_id)
    }

    /// Queues a command after setup, earlier commands and previous-session cleanup on its device.
    /// A command for a session that has already ended is answered at once and never runs.
    pub fn submit(&self, frame: CommandFrame) {
        let session_id = frame.session_id.to_string();
        if self.has_ended(frame.target.as_ref(), &session_id) {
            let message = format!(
                "Session {session_id} had already ended when this command reached the device, so it didn't run. Start a new session to keep going."
            );
            if !self
                .inner
                .outbox
                .send(DeviceFrame::Result(failed(frame.id, error("session_ended", message))))
            {
                tracing::warn!(
                    "command {} arrived for ended session {session_id} while Extend was unreachable; its refusal was dropped",
                    frame.id
                );
            }
            return;
        }
        let cancel = CancelToken::new();
        self.inner.pending.lock().unwrap().insert(
            frame.id,
            Pending {
                target: frame.target.clone(),
                session_id: frame.session_id.to_string(),
                cancel: cancel.clone(),
            },
        );
        let key = frame.target.clone();
        let job = Job {
            frame,
            cancel,
            received: Instant::now(),
        };
        self.enqueue(key, Work::Command(job));
    }

    fn enqueue(&self, key: QueueKey, job: Work) {
        let mut queues = self.inner.queues.lock().unwrap();
        let job = match queues.get(&key) {
            Some(tx) => match tx.send(job) {
                Ok(()) => return,
                // The worker is gone; start a fresh one with this job.
                Err(mpsc::error::SendError(job)) => job,
            },
            None => job,
        };
        queues.insert(key.clone(), self.spawn_worker(key, job));
    }

    fn spawn_worker(&self, key: QueueKey, first: Work) -> mpsc::UnboundedSender<Work> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Work>();
        tx.send(first).ok();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut recent: VecDeque<RecentHook> = VecDeque::new();
            while let Some(work) = rx.recv().await {
                match work {
                    Work::Command(job) => {
                        let id = job.frame.id;
                        let outcome = execute(&inner, job, &recent).await;
                        inner.pending.lock().unwrap().remove(&id);
                        if !inner.outbox.send(DeviceFrame::Result(outcome)) {
                            tracing::warn!(
                                "command {id} finished while Extend was unreachable; its result was dropped"
                            );
                        }
                    }
                    Work::Session {
                        driver,
                        session_id,
                        starting,
                        done,
                    } => {
                        let (what, limit) = if starting {
                            ("setup", inner.setup_limit)
                        } else {
                            ("cleanup", inner.cleanup_limit)
                        };
                        let started = Instant::now();
                        let hook = async {
                            if starting {
                                driver.session_started(&session_id).await;
                            } else {
                                driver.session_ended(&session_id).await;
                            }
                        };
                        if tokio::time::timeout(limit, hook).await.is_err() {
                            tracing::warn!(
                                "session {session_id} {what} didn't finish within {} s; it was stopped so the device can run its next work",
                                limit.as_secs()
                            );
                        }
                        recent.push_back(RecentHook {
                            session_id,
                            starting,
                            started,
                            finished: Instant::now(),
                        });
                        while recent.len() > RECENT_HOOKS {
                            recent.pop_front();
                        }
                        if let Some(done) = done {
                            let _ = done.send(());
                        }
                        if !starting {
                            // An idle device can drop its worker only while enqueue is excluded.
                            let mut queues = inner.queues.lock().unwrap();
                            if rx.is_empty() {
                                queues.remove(&key);
                                break;
                            }
                        }
                    }
                }
            }
        });
        tx
    }

    /// Stops a queued or running command.
    pub fn cancel(&self, id: Uuid) {
        if let Some(token) = self.inner.pending.lock().unwrap().get(&id) {
            token.cancel.cancel();
        }
    }

    /// Prepares the driver before this device's first session command can run. The receiver
    /// resolves once that setup has run.
    pub fn session_started(&self, target: Option<&DeviceId>, session_id: &str) -> Option<oneshot::Receiver<()>> {
        self.inner
            .ended
            .lock()
            .unwrap()
            .retain(|(t, s)| !(t.as_ref() == target && s == session_id));
        self.queue_lifecycle(target, session_id, true)
    }

    /// Cancels the session's queued and running commands, refuses any that arrive later, then
    /// cleans up before the next session uses this device. Cleanup is queued once per session;
    /// the receiver (when it was queued now) resolves once it has run.
    pub fn session_closed(&self, target: Option<&DeviceId>, session_id: &str) -> Option<oneshot::Receiver<()>> {
        for pending in self.inner.pending.lock().unwrap().values() {
            if pending.target.as_ref() == target && pending.session_id == session_id {
                pending.cancel.cancel();
            }
        }
        {
            let mut ended = self.inner.ended.lock().unwrap();
            if ended.iter().any(|(t, s)| t.as_ref() == target && s == session_id) {
                return None;
            }
            ended.push_back((target.cloned(), session_id.to_owned()));
            while ended.len() > ENDED_REMEMBERED {
                ended.pop_front();
            }
        }
        self.queue_lifecycle(target, session_id, false)
    }

    /// Ends every session this dispatcher has work for, plus `known` ones (the pair is gone, or
    /// the app is quitting): their commands are cancelled and their cleanup runs in order on each
    /// device's queue. Returns a receiver per cleanup queued.
    pub fn close_all(&self, known: &[(Option<DeviceId>, String)]) -> Vec<oneshot::Receiver<()>> {
        let mut sessions: Vec<(Option<DeviceId>, String)> = known.to_vec();
        for p in self.inner.pending.lock().unwrap().values() {
            sessions.push((p.target.clone(), p.session_id.clone()));
        }
        let mut seen = Vec::new();
        let mut done = Vec::new();
        for (target, session_id) in sessions {
            if seen.contains(&(target.clone(), session_id.clone())) {
                continue;
            }
            if let Some(rx) = self.session_closed(target.as_ref(), &session_id) {
                done.push(rx);
            }
            seen.push((target, session_id));
        }
        done
    }

    fn queue_lifecycle(
        &self,
        target: Option<&DeviceId>,
        session_id: &str,
        starting: bool,
    ) -> Option<oneshot::Receiver<()>> {
        match self.inner.lookup.driver_for(target) {
            Ok(driver) => {
                let (tx, rx) = oneshot::channel();
                self.enqueue(
                    target.cloned(),
                    Work::Session {
                        driver,
                        session_id: session_id.to_owned(),
                        starting,
                        done: Some(tx),
                    },
                );
                Some(rx)
            }
            Err(error) => {
                tracing::warn!("couldn't handle lifecycle for session {session_id}: {error}");
                None
            }
        }
    }

    /// Commands queued or running.
    pub fn in_flight(&self) -> usize {
        self.inner.pending.lock().unwrap().len()
    }
}

/// Checks the command before anything runs. The service checks too; this is the device's own guard.
pub fn validate(frame: &CommandFrame) -> Result<(), CommandError> {
    if let Some(replacement) = not_exposed(&frame.command) {
        let message = match replacement {
            Some(r) => format!("`{}` isn't available through Extend. Use `{r}` instead.", frame.command),
            None => format!("`{}` isn't available through Extend.", frame.command),
        };
        return Err(error("unknown_command", message));
    }
    if command_spec(&frame.command).is_none() {
        return Err(error(
            "unknown_command",
            format!(
                "`{}` isn't an Extend command. Run `extend --help` for the list.",
                frame.command
            ),
        ));
    }
    if let Some(flag) = find_refused_flag(&frame.args) {
        return Err(error("invalid_args", refused_flag_message(flag)));
    }
    Ok(())
}

fn error(code: &str, message: impl Into<String>) -> CommandError {
    CommandError {
        code: code.to_owned(),
        message: message.into(),
        details: serde_json::Value::Null,
    }
}

fn failed(id: Uuid, err: CommandError) -> CommandOutcome {
    CommandOutcome {
        id,
        ok: false,
        output: serde_json::Value::Null,
        text: Some(err.message.clone()),
        error: Some(err),
        files: vec![],
    }
}

/// The command's deadline, clamped to Extend's bounds.
pub fn deadline(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms.clamp(COMMAND_TIMEOUT_MIN_MS, COMMAND_TIMEOUT_MAX_MS))
}

/// Part of the deadline kept back for uploading files: a fifth, at most 5 s.
pub fn upload_reserve(total: Duration) -> Duration {
    (total / 5).min(Duration::from_secs(5))
}

/// Why a command's deadline passed before it could start: the lifecycle hook that held it up
/// longest, or the commands before it.
fn queued_too_long(recent: &VecDeque<RecentHook>, received: Instant, cleanup_limit: Duration) -> String {
    let held_up = |h: &&RecentHook| h.finished.saturating_duration_since(h.started.max(received));
    let hook = recent.iter().filter(|h| h.finished > received).max_by_key(held_up);
    match hook {
        Some(h) if h.finished > received && !h.starting => format!(
            "The command's deadline passed while this device was still cleaning up after session {} (stopping a recording, closing that session), so it didn't run. That cleanup can take up to {} minutes; run the command again.",
            h.session_id,
            cleanup_limit.as_secs().div_ceil(60)
        ),
        Some(h) if h.finished > received => format!(
            "The command's deadline passed while session {} was still being prepared on this device, so it didn't run. Run it again.",
            h.session_id
        ),
        _ => "The command's deadline passed while it waited for the command before it, so it didn't run. Run it again, or give it a longer timeout.".into(),
    }
}

async fn execute(inner: &Inner, job: Job, recent: &VecDeque<RecentHook>) -> CommandOutcome {
    let Job {
        frame,
        cancel,
        received,
    } = job;
    let id = frame.id;
    if cancel.is_cancelled() {
        return failed(
            id,
            error("cancelled", "Extend cancelled this command before it started."),
        );
    }
    if let Err(e) = validate(&frame) {
        return failed(id, e);
    }
    let driver = match inner.lookup.driver_for(frame.target.as_ref()) {
        Ok(d) => d,
        Err(why) => return failed(id, error("unsupported_on_device", why)),
    };
    let total = deadline(frame.timeout_ms);
    // Time spent queued behind earlier commands counts against the deadline.
    let remaining = total.saturating_sub(received.elapsed());
    if remaining.is_zero() {
        return failed(
            id,
            error(
                "command_timeout",
                queued_too_long(recent, received, inner.cleanup_limit),
            ),
        );
    }
    let run_budget = remaining
        .saturating_sub(upload_reserve(total))
        .max(Duration::from_millis(500));

    let workdir = inner.work_root.join(id.to_string());
    let outcome = async {
        let saved = write_attachments(&workdir, &frame)
            .await
            .map_err(|e| error("invalid_args", e))?;
        let args = substitute_attachments(&frame.args, &saved).map_err(|e| error("invalid_args", e))?;
        let attachments: Vec<PathBuf> = saved.into_iter().map(|(_, p)| p).collect();
        let session_id = frame.session_id.to_string();
        let inv = Invocation {
            id,
            session_id: &session_id,
            command: &frame.command,
            args: &args,
            attachments: &attachments,
            workdir: &workdir,
            timeout: run_budget,
            cancel: cancel.clone(),
        };
        let output = run_with_deadline(driver.as_ref(), inv, run_budget, &cancel).await;
        Ok::<_, CommandError>(finish(inner, &frame, output).await)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&workdir).await;
    match outcome {
        Ok(o) => o,
        Err(e) => failed(id, e),
    }
}

/// Runs the driver, stopping it at the deadline or on cancel.
pub async fn run_with_deadline(
    driver: &dyn Driver,
    inv: Invocation<'_>,
    budget: Duration,
    cancel: &CancelToken,
) -> Output {
    let mut fut = std::pin::pin!(driver.run(inv));
    enum Stop {
        Deadline,
        Cancel,
    }
    let stop = tokio::select! {
        // A cancel wins over an answer that arrives in the same instant, so a cancelled command
        // always reports `cancelled`.
        biased;
        _ = cancel.cancelled() => Stop::Cancel,
        out = &mut fut => return out,
        _ = tokio::time::sleep(budget) => Stop::Deadline,
    };
    cancel.cancel();
    // Give the driver a moment to stop what it started; its answer no longer counts.
    let _ = tokio::time::timeout(WIND_DOWN, &mut fut).await;
    match stop {
        Stop::Deadline => Output::fail(
            "command_timeout",
            format!(
                "The command didn't finish within {} ms and was stopped.",
                budget.as_millis()
            ),
        ),
        Stop::Cancel => Output::fail("cancelled", "Extend cancelled this command."),
    }
}

/// Prefix of an argument that names an attachment (`display show --image attachment:cat.png`).
pub const ATTACHMENT_PREFIX: &str = "attachment:";

/// Replaces every `attachment:<name>` argument (also `--flag=attachment:<name>`) with the local
/// path the attachment was written to.
pub fn substitute_attachments(args: &[String], saved: &[(String, PathBuf)]) -> Result<Vec<String>, String> {
    let lookup = |name: &str| -> Result<String, String> {
        saved
            .iter()
            .find(|(n, _)| n == name)
            .or_else(|| saved.iter().find(|(n, _)| safe_file_name(n, "") == safe_file_name(name, "")))
            .map(|(_, p)| p.display().to_string())
            .ok_or_else(|| format!("{ATTACHMENT_PREFIX}{name} was named in the command but no attachment called {name:?} was sent with it."))
    };
    args.iter()
        .map(|a| {
            if let Some(name) = a.strip_prefix(ATTACHMENT_PREFIX) {
                lookup(name)
            } else if a.starts_with("--")
                && let Some((flag, value)) = a.split_once('=')
                && let Some(name) = value.strip_prefix(ATTACHMENT_PREFIX)
            {
                Ok(format!("{flag}={}", lookup(name)?))
            } else {
                Ok(a.clone())
            }
        })
        .collect()
}

/// Writes each attachment into `{workdir}/attachments/`; returns (name as sent, local path).
async fn write_attachments(workdir: &Path, frame: &CommandFrame) -> Result<Vec<(String, PathBuf)>, String> {
    tokio::fs::create_dir_all(workdir)
        .await
        .map_err(|e| format!("couldn't create a work directory: {e}"))?;
    if frame.attachments.is_empty() {
        return Ok(vec![]);
    }
    let dir = workdir.join("attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("couldn't create a work directory: {e}"))?;
    let mut saved: Vec<(String, PathBuf)> = Vec::with_capacity(frame.attachments.len());
    for (i, a) in frame.attachments.iter().enumerate() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(a.content_base64.trim())
            .map_err(|e| format!("attachment {:?} isn't valid base64: {e}", a.name))?;
        let mut name = safe_file_name(&a.name, &format!("attachment-{}", i + 1));
        if saved
            .iter()
            .any(|(_, p)| p.file_name().and_then(|n| n.to_str()) == Some(name.as_str()))
        {
            name = format!("{}-{name}", i + 1);
        }
        let path = dir.join(name);
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| format!("couldn't save attachment {:?}: {e}", a.name))?;
        saved.push((a.name.clone(), path));
    }
    Ok(saved)
}

/// Uploads the driver's files with the command's upload ids, in order, then builds the result.
async fn finish(inner: &Inner, frame: &CommandFrame, output: Output) -> CommandOutcome {
    let Output {
        mut ok,
        output,
        mut text,
        mut error,
        files,
    } = output;
    let mut produced = Vec::new();
    let mut ids = frame.upload_ids.iter();
    let mut skipped = Vec::new();
    for file in files {
        let Some(upload_id) = ids.next() else {
            skipped.push(file.name.clone());
            continue;
        };
        match inner
            .uploader
            .upload(*upload_id, &file.path, &file.name, &file.content_type)
            .await
        {
            Ok(size) => produced.push(ProducedFile {
                upload_id: *upload_id,
                name: file.name,
                content_type: file.content_type,
                kind: file.kind,
                size_bytes: i64::try_from(size).unwrap_or(i64::MAX),
            }),
            Err(why) => {
                tracing::warn!("upload of {} failed: {why}", file.name);
                ok = false;
                if error.is_none() {
                    error = Some(CommandError {
                        code: "upload_failed".into(),
                        message: format!("{} was made but couldn't be uploaded to Extend: {why}", file.name),
                        details: serde_json::json!({ "file": file.name }),
                    });
                }
            }
        }
    }
    if !skipped.is_empty() {
        tracing::warn!(
            "command {} produced {} more files than it had upload ids",
            frame.id,
            skipped.len()
        );
        let note = format!("Not uploaded (no upload id left): {}", skipped.join(", "));
        text = Some(match text {
            Some(t) if !t.is_empty() => format!("{t}\n{note}"),
            _ => note,
        });
    }
    CommandOutcome {
        id: frame.id,
        ok,
        output,
        text,
        error,
        files: produced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extend_driver::{LocalFile, Probe};
    use extend_protocol::model::{Attachment, FileKind, Setup};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeDriver {
        log: Mutex<Vec<String>>,
        running: AtomicUsize,
        max_running: AtomicUsize,
        /// Cleanup never finishes (a stuck recording export, say).
        hang_cleanup: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl Driver for FakeDriver {
        async fn probe(&self) -> Probe {
            Probe {
                os: extend_protocol::DeviceOs::Macos,
                os_version: None,
                model: None,
                capabilities: vec![],
                missing: vec![],
                setup: Setup::complete(),
                agent_device_version: None,
                online: true,
            }
        }
        async fn run(&self, inv: Invocation<'_>) -> Output {
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_running.fetch_max(now, Ordering::SeqCst);
            self.log
                .lock()
                .unwrap()
                .push(format!("start {} {}", inv.session_id, inv.args.join(" ")));
            let out = match inv.args.first().map(String::as_str) {
                Some("sleep") => {
                    let ms: u64 = inv.args[1].parse().unwrap();
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Output::ok(serde_json::json!({"slept": ms}), "slept")
                }
                Some("stubborn") => {
                    // Ignores cancel entirely.
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                Some("polite") => {
                    inv.cancel.cancelled().await;
                    Output::fail("cancelled_inside", "stopped")
                }
                Some("files") => {
                    let n: usize = inv.args[1].parse().unwrap();
                    let mut out = Output::ok(serde_json::json!({}), "made files");
                    for i in 0..n {
                        let p = inv.workdir.join(format!("f{i}.png"));
                        std::fs::write(&p, vec![b'x'; i + 1]).unwrap();
                        out = out.with_file(LocalFile {
                            path: p,
                            name: format!("f{i}.png"),
                            content_type: "image/png".into(),
                            kind: FileKind::Screenshot,
                        });
                    }
                    out
                }
                Some("attachments-args") => Output::ok(serde_json::json!(inv.args), "args"),
                Some("attachments") => {
                    let names: Vec<String> = inv
                        .attachments
                        .iter()
                        .map(|p| {
                            format!(
                                "{}={}",
                                p.file_name().unwrap().to_str().unwrap(),
                                std::fs::read_to_string(p).unwrap()
                            )
                        })
                        .collect();
                    Output::ok(serde_json::json!(names), names.join(","))
                }
                _ => Output::ok(serde_json::json!({"ran": inv.command}), "ok"),
            };
            self.log
                .lock()
                .unwrap()
                .push(format!("end {} {}", inv.session_id, inv.args.join(" ")));
            self.running.fetch_sub(1, Ordering::SeqCst);
            out
        }
        async fn session_ended(&self, session_id: &str) {
            self.log.lock().unwrap().push(format!("cleanup {session_id}"));
            if self.hang_cleanup.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            tokio::task::yield_now().await;
            self.log.lock().unwrap().push(format!("cleaned {session_id}"));
        }
        async fn session_started(&self, session_id: &str) {
            self.log.lock().unwrap().push(format!("setup {session_id}"));
            tokio::task::yield_now().await;
            self.log.lock().unwrap().push(format!("ready {session_id}"));
        }
    }

    struct Lookup(Arc<FakeDriver>);
    impl DriverLookup for Lookup {
        fn driver_for(&self, target: Option<&DeviceId>) -> Result<Arc<dyn Driver>, String> {
            match target {
                None => Ok(self.0.clone()),
                Some(id) if id.as_str() == "00000001" => Ok(self.0.clone()),
                Some(id) => Err(format!("device {id} isn't attached to this computer")),
            }
        }
    }

    #[derive(Default)]
    struct FakeUploader {
        calls: Mutex<Vec<(Uuid, String, u64)>>,
        fail: bool,
    }
    #[async_trait]
    impl Uploader for FakeUploader {
        async fn upload(&self, upload_id: Uuid, path: &Path, name: &str, _ct: &str) -> Result<u64, String> {
            if self.fail {
                return Err("HTTP 404".into());
            }
            let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
            self.calls.lock().unwrap().push((upload_id, name.to_owned(), size));
            Ok(size)
        }
    }

    struct Harness {
        dispatcher: Dispatcher,
        driver: Arc<FakeDriver>,
        uploader: Arc<FakeUploader>,
        rx: mpsc::UnboundedReceiver<DeviceFrame>,
        _dir: tempfile::TempDir,
        work: PathBuf,
    }

    fn harness(fail_uploads: bool) -> Harness {
        harness_with_limits(fail_uploads, SETUP_LIMIT, CLEANUP_LIMIT)
    }

    fn harness_with_limits(fail_uploads: bool, setup: Duration, cleanup: Duration) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(FakeDriver::default());
        let uploader = Arc::new(FakeUploader {
            fail: fail_uploads,
            ..Default::default()
        });
        let outbox = Outbox::default();
        let (tx, rx) = mpsc::unbounded_channel();
        outbox.set(tx);
        let work = dir.path().join("work");
        let dispatcher = Dispatcher::with_lifecycle_limits(
            Arc::new(Lookup(driver.clone())),
            uploader.clone(),
            outbox,
            work.clone(),
            setup,
            cleanup,
        );
        Harness {
            dispatcher,
            driver,
            uploader,
            rx,
            _dir: dir,
            work,
        }
    }

    async fn until_logged(driver: &FakeDriver, line: &str) {
        for _ in 0..500 {
            if driver.log.lock().unwrap().iter().any(|l| l == line) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{line:?} never logged: {:?}", driver.log.lock().unwrap());
    }

    fn cmd(session: &str, command: &str, args: &[&str], timeout_ms: u64, uploads: usize) -> CommandFrame {
        CommandFrame {
            id: Uuid::new_v4(),
            session_id: session.parse().unwrap(),
            target: None,
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            attachments: vec![],
            timeout_ms,
            upload_ids: (0..uploads).map(|_| Uuid::new_v4()).collect(),
        }
    }

    async fn next_result(rx: &mut mpsc::UnboundedReceiver<DeviceFrame>) -> CommandOutcome {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("result in time")
            .unwrap()
        {
            DeviceFrame::Result(r) => r,
            other => panic!("expected result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_session_runs_in_order_one_at_a_time() {
        let mut h = harness(false);
        let a = cmd("a3f", "click", &["sleep", "300"], 30_000, 0);
        let b = cmd("a3f", "click", &["sleep", "10"], 30_000, 0);
        let c = cmd("a3f", "snapshot", &[], 30_000, 0);
        let ids = [a.id, b.id, c.id];
        h.dispatcher.submit(a);
        h.dispatcher.submit(b);
        h.dispatcher.submit(c);
        for id in ids {
            assert_eq!(next_result(&mut h.rx).await.id, id);
        }
        assert_eq!(h.driver.max_running.load(Ordering::SeqCst), 1);
        let log = h.driver.log.lock().unwrap().clone();
        assert_eq!(log[0], "start a3f sleep 300");
        assert_eq!(log[1], "end a3f sleep 300");
        assert_eq!(log[2], "start a3f sleep 10");
        assert_eq!(h.dispatcher.in_flight(), 0);
    }

    #[tokio::test]
    async fn sessions_on_the_same_device_run_in_order() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "click", &["sleep", "300"], 30_000, 0));
        h.dispatcher.submit(cmd("b40", "click", &["sleep", "300"], 30_000, 0));
        next_result(&mut h.rx).await;
        next_result(&mut h.rx).await;
        assert_eq!(h.driver.max_running.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn new_session_waits_for_previous_session_cleanup() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "snapshot", &[], 30_000, 0));
        assert!(next_result(&mut h.rx).await.ok);
        h.dispatcher.session_closed(None, "a3f");
        h.dispatcher.session_started(None, "b40");
        h.dispatcher.submit(cmd("b40", "snapshot", &[], 30_000, 0));
        assert!(next_result(&mut h.rx).await.ok);
        assert_eq!(
            *h.driver.log.lock().unwrap(),
            [
                "start a3f ",
                "end a3f ",
                "cleanup a3f",
                "cleaned a3f",
                "setup b40",
                "ready b40",
                "start b40 ",
                "end b40 "
            ]
        );
    }

    #[tokio::test]
    async fn separate_devices_still_run_side_by_side() {
        let mut h = harness(false);
        let mut attached = cmd("b40", "click", &["sleep", "100"], 30_000, 0);
        attached.target = Some("00000001".parse().unwrap());
        h.dispatcher.submit(cmd("a3f", "click", &["sleep", "100"], 30_000, 0));
        h.dispatcher.submit(attached);
        assert!(next_result(&mut h.rx).await.ok);
        assert!(next_result(&mut h.rx).await.ok);
        assert_eq!(h.driver.max_running.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn session_end_cancels_queued_work_before_cleanup() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "click", &["stubborn"], 30_000, 0));
        h.dispatcher.session_closed(None, "a3f");
        h.dispatcher.submit(cmd("b40", "snapshot", &[], 30_000, 0));
        assert_eq!(next_result(&mut h.rx).await.error.unwrap().code, "cancelled");
        assert!(next_result(&mut h.rx).await.ok);
        assert_eq!(
            *h.driver.log.lock().unwrap(),
            ["cleanup a3f", "cleaned a3f", "start b40 ", "end b40 "]
        );
    }

    #[tokio::test]
    async fn deadline_stops_a_stubborn_driver() {
        let mut h = harness(false);
        let started = Instant::now();
        h.dispatcher.submit(cmd("a3f", "click", &["stubborn"], 1_000, 0));
        let r = next_result(&mut h.rx).await;
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, "command_timeout");
        // 1 s deadline minus the upload reserve, plus the wind-down.
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn cancel_stops_a_running_command() {
        let mut h = harness(false);
        let c = cmd("a3f", "click", &["polite"], 60_000, 0);
        let id = c.id;
        h.dispatcher.submit(c);
        tokio::time::sleep(Duration::from_millis(100)).await;
        h.dispatcher.cancel(id);
        let r = next_result(&mut h.rx).await;
        assert_eq!(r.id, id);
        assert_eq!(r.error.unwrap().code, "cancelled");
    }

    #[tokio::test]
    async fn cancel_before_start_never_runs() {
        let mut h = harness(false);
        let first = cmd("a3f", "click", &["sleep", "200"], 30_000, 0);
        let second = cmd("a3f", "click", &["never"], 30_000, 0);
        let second_id = second.id;
        h.dispatcher.submit(first);
        h.dispatcher.submit(second);
        h.dispatcher.cancel(second_id);
        next_result(&mut h.rx).await;
        let r = next_result(&mut h.rx).await;
        assert_eq!(r.error.unwrap().code, "cancelled");
        assert!(!h.driver.log.lock().unwrap().iter().any(|l| l.contains("never")));
    }

    #[tokio::test]
    async fn refused_flags_never_reach_the_driver() {
        let mut h = harness(false);
        h.dispatcher
            .submit(cmd("a3f", "snapshot", &["-i", "--platform", "ios"], 30_000, 0));
        let r = next_result(&mut h.rx).await;
        let e = r.error.unwrap();
        assert_eq!(e.code, "invalid_args");
        assert!(e.message.contains("--platform"));
        assert!(h.driver.log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_and_hidden_commands() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "devices", &[], 30_000, 0));
        let e = next_result(&mut h.rx).await.error.unwrap();
        assert_eq!(e.code, "unknown_command");
        assert!(e.message.contains("extend device ls"));
        h.dispatcher.submit(cmd("a3f", "frobnicate", &[], 30_000, 0));
        assert_eq!(next_result(&mut h.rx).await.error.unwrap().code, "unknown_command");
    }

    #[tokio::test]
    async fn unknown_target_is_unsupported() {
        let mut h = harness(false);
        let mut c = cmd("a3f", "snapshot", &[], 30_000, 0);
        c.target = Some("7c1e09ab".parse().unwrap());
        h.dispatcher.submit(c);
        let e = next_result(&mut h.rx).await.error.unwrap();
        assert_eq!(e.code, "unsupported_on_device");
        assert!(e.message.contains("7c1e09ab"));
    }

    #[tokio::test]
    async fn files_upload_in_order_with_their_ids() {
        let mut h = harness(false);
        let c = cmd("a3f", "screenshot", &["files", "2"], 30_000, 3);
        let ids = c.upload_ids.clone();
        let id = c.id;
        h.dispatcher.submit(c);
        let r = next_result(&mut h.rx).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.files.len(), 2);
        assert_eq!(r.files[0].upload_id, ids[0]);
        assert_eq!(r.files[1].upload_id, ids[1]);
        assert_eq!(r.files[0].size_bytes, 1);
        assert_eq!(r.files[1].size_bytes, 2);
        assert_eq!(r.files[0].kind, FileKind::Screenshot);
        let calls = h.uploader.calls.lock().unwrap().clone();
        assert_eq!(calls.iter().map(|c| c.0).collect::<Vec<_>>(), ids[..2].to_vec());
        // The work directory is gone once the result is out.
        assert!(!h.work.join(id.to_string()).exists());
    }

    #[tokio::test]
    async fn more_files_than_upload_ids() {
        let mut h = harness(false);
        h.dispatcher
            .submit(cmd("a3f", "screenshot", &["files", "3"], 30_000, 1));
        let r = next_result(&mut h.rx).await;
        assert!(r.ok);
        assert_eq!(r.files.len(), 1);
        assert!(r.text.unwrap().contains("f1.png, f2.png"));
    }

    #[tokio::test]
    async fn failed_upload_fails_the_command() {
        let mut h = harness(true);
        h.dispatcher
            .submit(cmd("a3f", "screenshot", &["files", "1"], 30_000, 1));
        let r = next_result(&mut h.rx).await;
        assert!(!r.ok);
        assert!(r.files.is_empty());
        assert_eq!(r.error.unwrap().code, "upload_failed");
    }

    #[tokio::test]
    async fn attachments_are_written_for_the_driver() {
        let mut h = harness(false);
        let mut c = cmd("a3f", "replay", &["attachments"], 30_000, 0);
        c.attachments = vec![
            Attachment {
                name: "../../evil/flow.ad".into(),
                content_type: "text/plain".into(),
                content_base64: base64::engine::general_purpose::STANDARD.encode("open Notes"),
            },
            Attachment {
                name: "flow.ad".into(),
                content_type: "text/plain".into(),
                content_base64: base64::engine::general_purpose::STANDARD.encode("close"),
            },
        ];
        h.dispatcher.submit(c);
        let r = next_result(&mut h.rx).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.text.as_deref(), Some("flow.ad=open Notes,2-flow.ad=close"));
    }

    #[tokio::test]
    async fn bad_base64_is_invalid_args() {
        let mut h = harness(false);
        let mut c = cmd("a3f", "replay", &["x.ad"], 30_000, 0);
        c.attachments = vec![Attachment {
            name: "x.ad".into(),
            content_type: "text/plain".into(),
            content_base64: "!!!".into(),
        }];
        h.dispatcher.submit(c);
        assert_eq!(next_result(&mut h.rx).await.error.unwrap().code, "invalid_args");
    }

    #[tokio::test]
    async fn attachment_arguments_become_local_paths() {
        let mut h = harness(false);
        let mut c = cmd(
            "a3f",
            "replay",
            &[
                "attachments-args",
                "attachment:flow.ad",
                "--steps-file=attachment:steps.json",
                "plain",
            ],
            30_000,
            0,
        );
        c.attachments = vec![
            Attachment {
                name: "flow.ad".into(),
                content_type: "text/plain".into(),
                content_base64: base64::engine::general_purpose::STANDARD.encode("x"),
            },
            Attachment {
                name: "steps.json".into(),
                content_type: "application/json".into(),
                content_base64: base64::engine::general_purpose::STANDARD.encode("[]"),
            },
        ];
        h.dispatcher.submit(c);
        let r = next_result(&mut h.rx).await;
        let args: Vec<String> = serde_json::from_value(r.output).unwrap();
        assert!(args[1].ends_with("/attachments/flow.ad"), "{args:?}");
        assert!(
            args[2].starts_with("--steps-file=") && args[2].ends_with("/attachments/steps.json"),
            "{args:?}"
        );
        assert_eq!(args[3], "plain");
        // Naming an attachment that wasn't sent is refused before anything runs.
        h.dispatcher
            .submit(cmd("a3f", "replay", &["attachment:missing.ad"], 30_000, 0));
        let e = next_result(&mut h.rx).await.error.unwrap();
        assert_eq!(e.code, "invalid_args");
        assert!(e.message.contains("missing.ad"));
    }

    #[tokio::test]
    async fn session_end_interrupts_a_running_command_before_cleanup() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "click", &["polite"], 60_000, 0));
        // The command is running (not merely queued) when the session ends: the Stop path.
        until_logged(&h.driver, "start a3f polite").await;
        let stopped = Instant::now();
        let cleaned = h.dispatcher.session_closed(None, "a3f").expect("cleanup queued");
        let r = next_result(&mut h.rx).await;
        assert_eq!(r.error.unwrap().code, "cancelled");
        assert!(stopped.elapsed() < WIND_DOWN, "{:?}", stopped.elapsed());
        tokio::time::timeout(Duration::from_secs(5), cleaned)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            *h.driver.log.lock().unwrap(),
            ["start a3f polite", "end a3f polite", "cleanup a3f", "cleaned a3f"]
        );
    }

    #[tokio::test]
    async fn an_idle_device_drops_its_worker_and_the_next_work_starts_a_new_one() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "snapshot", &[], 30_000, 0));
        assert!(next_result(&mut h.rx).await.ok);
        let cleaned = h.dispatcher.session_closed(None, "a3f").expect("cleanup queued");
        tokio::time::timeout(Duration::from_secs(5), cleaned)
            .await
            .unwrap()
            .unwrap();
        for _ in 0..500 {
            if h.dispatcher.inner.queues.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            h.dispatcher.inner.queues.lock().unwrap().is_empty(),
            "the idle worker is gone"
        );
        h.dispatcher.session_started(None, "b40");
        h.dispatcher.submit(cmd("b40", "snapshot", &[], 30_000, 0));
        assert!(next_result(&mut h.rx).await.ok);
        assert_eq!(
            *h.driver.log.lock().unwrap(),
            [
                "start a3f ",
                "end a3f ",
                "cleanup a3f",
                "cleaned a3f",
                "setup b40",
                "ready b40",
                "start b40 ",
                "end b40 "
            ]
        );
    }

    #[tokio::test]
    async fn commands_for_an_ended_session_are_refused_and_cleanup_runs_once() {
        let mut h = harness(false);
        h.dispatcher.session_started(None, "a3f");
        h.dispatcher.submit(cmd("a3f", "snapshot", &[], 30_000, 0));
        assert!(next_result(&mut h.rx).await.ok);
        let cleaned = h.dispatcher.session_closed(None, "a3f").expect("cleanup queued");
        // The service sent this before it saw the session end (it was waiting on its session lock).
        let late = cmd("a3f", "open", &["Notes"], 30_000, 0);
        let late_id = late.id;
        h.dispatcher.submit(late);
        let r = next_result(&mut h.rx).await;
        assert_eq!(r.id, late_id);
        let e = r.error.unwrap();
        assert_eq!(e.code, "session_ended");
        assert!(
            e.message.contains("a3f") && e.message.contains("didn't run"),
            "{}",
            e.message
        );
        // A repeated end (revoke after Stop) doesn't clean up twice.
        assert!(h.dispatcher.session_closed(None, "a3f").is_none());
        tokio::time::timeout(Duration::from_secs(5), cleaned)
            .await
            .unwrap()
            .unwrap();
        let log = h.driver.log.lock().unwrap().clone();
        assert!(!log.iter().any(|l| l.contains("Notes")), "{log:?}");
        assert_eq!(log.iter().filter(|l| *l == "cleanup a3f").count(), 1, "{log:?}");
        // The same session id on another device is a different session.
        let mut other = cmd("a3f", "snapshot", &[], 30_000, 0);
        other.target = Some("00000001".parse().unwrap());
        h.dispatcher.submit(other);
        assert!(next_result(&mut h.rx).await.ok);
    }

    #[tokio::test]
    async fn a_stuck_cleanup_is_cut_off_and_the_waiting_command_says_why() {
        let mut h = harness_with_limits(false, Duration::from_millis(500), Duration::from_millis(1_500));
        h.driver.hang_cleanup.store(true, Ordering::SeqCst);
        h.dispatcher.submit(cmd("a3f", "snapshot", &[], 30_000, 0));
        assert!(next_result(&mut h.rx).await.ok);
        let cleaned = h.dispatcher.session_closed(None, "a3f").expect("cleanup queued");
        h.dispatcher.session_started(None, "b40");
        // 1 s deadline, stuck behind 1.5 s of cleanup.
        h.dispatcher.submit(cmd("b40", "snapshot", &[], 1_000, 0));
        h.dispatcher.submit(cmd("b40", "snapshot", &["second"], 30_000, 0));
        let r = next_result(&mut h.rx).await;
        let e = r.error.unwrap();
        assert_eq!(e.code, "command_timeout");
        assert!(e.message.contains("cleaning up after session a3f"), "{}", e.message);
        // The device moved on: cleanup was cut off and the next command runs.
        tokio::time::timeout(Duration::from_secs(5), cleaned)
            .await
            .unwrap()
            .unwrap();
        assert!(next_result(&mut h.rx).await.ok);
        let log = h.driver.log.lock().unwrap().clone();
        assert!(!log.contains(&"cleaned a3f".to_string()), "{log:?}");
        assert!(log.contains(&"start b40 second".to_string()), "{log:?}");
    }

    #[tokio::test]
    async fn close_all_cancels_every_session_and_cleans_up_after_its_commands() {
        let mut h = harness(false);
        h.dispatcher.submit(cmd("a3f", "click", &["polite"], 60_000, 0));
        let mut carried = cmd("b40", "click", &["polite"], 60_000, 0);
        carried.target = Some("00000001".parse().unwrap());
        h.dispatcher.submit(carried);
        until_logged(&h.driver, "start a3f polite").await;
        until_logged(&h.driver, "start b40 polite").await;
        // The local session is known to the app; the carried one only through its command.
        let cleaned = h.dispatcher.close_all(&[(None, "a3f".into())]);
        assert_eq!(cleaned.len(), 2);
        for _ in 0..2 {
            assert_eq!(next_result(&mut h.rx).await.error.unwrap().code, "cancelled");
        }
        for done in cleaned {
            tokio::time::timeout(Duration::from_secs(5), done)
                .await
                .unwrap()
                .unwrap();
        }
        let log = h.driver.log.lock().unwrap().clone();
        let at = |l: &str| {
            log.iter()
                .position(|x| x == l)
                .unwrap_or_else(|| panic!("{l:?} missing from {log:?}"))
        };
        assert!(at("end a3f polite") < at("cleanup a3f"));
        assert!(at("end b40 polite") < at("cleanup b40"));
        assert_eq!(h.dispatcher.in_flight(), 0);
    }

    #[test]
    fn deadlines_are_clamped() {
        assert_eq!(deadline(10), Duration::from_millis(1_000));
        assert_eq!(deadline(10_000_000), Duration::from_millis(300_000));
        assert_eq!(upload_reserve(Duration::from_secs(30)), Duration::from_secs(5));
        assert_eq!(upload_reserve(Duration::from_secs(5)), Duration::from_secs(1));
    }
}
