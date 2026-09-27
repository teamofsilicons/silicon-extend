//! The interface between the Extend device agent and whatever actually operates a device.
//!
//! The agent owns the connection to the service, sessions, uploads and the indicator. A driver
//! only answers two questions: what can this device do right now (`probe`), and run this command
//! (`run`). Drivers exist for the device engine (Mac, Linux, and iPhone/iPad through a Mac),
//! Windows, and the TVs a computer carries (Apple TV, Samsung, LG).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use extend_protocol::model::{CommandError, FileKind, MissingCapability, Setup, SleepState};
use extend_protocol::{Capability, DeviceOs};
use tokio_util_cancel::CancelToken;

pub use tokio_util_cancel as cancel;

/// What a device can do at this moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub os: DeviceOs,
    pub os_version: Option<String>,
    pub model: Option<String>,
    pub capabilities: Vec<Capability>,
    /// Capabilities this kind of device has when fully set up but this one lacks, with why.
    pub missing: Vec<MissingCapability>,
    pub setup: Setup,
    /// The device engine's version when the driver uses it.
    pub engine_version: Option<String>,
    /// False when the device can't be reached right now (a TV that's off).
    pub online: bool,
    /// Whether the device is awake: its screen on and unlocked, or a TV out of standby. `None`
    /// when the driver can't tell (an iPhone, a TV that doesn't say). Information only: nothing
    /// is withheld for it, and no driver ever wakes a device to find out.
    pub awake: Option<bool>,
    /// Why it isn't awake, when `awake` is `Some(false)`.
    pub sleep_state: Option<SleepState>,
    /// A stable id of the physical device that survives restarts and re-pairing (an iPhone's UDID,
    /// a TV's DUID or MAC), for a carried device. The agent never sends it: it sends an HMAC of it
    /// keyed with the world's salt (`hardware_key`), so one TV carried for two Carbons is one device.
    pub hardware_id: Option<String>,
}

/// One command to run. `args` are device-engine command-line tokens after the command name.
#[derive(Debug, Clone)]
pub struct Invocation<'a> {
    pub id: uuid::Uuid,
    pub session_id: &'a str,
    pub command: &'a str,
    pub args: &'a [String],
    /// Files the caller sent with the command, already written to disk.
    pub attachments: &'a [PathBuf],
    /// A private scratch directory for this command; files to return go here.
    pub workdir: &'a Path,
    pub timeout: std::time::Duration,
    pub cancel: CancelToken,
}

/// A file the command produced, on local disk, for the agent to upload.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalFile {
    pub path: PathBuf,
    pub name: String,
    pub content_type: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub ok: bool,
    /// Structured result (the device engine's `--json` data, or the driver's own).
    pub output: serde_json::Value,
    /// What the CLI prints without `--json`.
    pub text: Option<String>,
    pub error: Option<CommandError>,
    pub files: Vec<LocalFile>,
}

impl Output {
    pub fn ok(output: serde_json::Value, text: impl Into<String>) -> Self {
        Self {
            ok: true,
            output,
            text: Some(text.into()),
            error: None,
            files: vec![],
        }
    }
    pub fn fail(code: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            ok: false,
            output: serde_json::Value::Null,
            text: Some(message.clone()),
            error: Some(CommandError {
                code: code.to_owned(),
                message,
                details: serde_json::Value::Null,
            }),
            files: vec![],
        }
    }
    pub fn with_file(mut self, file: LocalFile) -> Self {
        self.files.push(file);
        self
    }
}

#[async_trait]
pub trait Driver: Send + Sync {
    /// Checks permissions, helpers and reachability. Called at connect and every 30 s.
    async fn probe(&self) -> Probe;

    /// Runs one command. Must return within `inv.timeout` and stop promptly when `inv.cancel` fires.
    /// A command the device doesn't support returns `Output::fail("unsupported_on_device", ...)`.
    async fn run(&self, inv: Invocation<'_>) -> Output;

    /// Called when a session starts and ends, so the driver can prepare or clean up (close its
    /// device-engine session, stop a recording that was left running).
    async fn session_started(&self, _session_id: &str) {}
    async fn session_ended(&self, _session_id: &str) {}
    /// The session is still live though no command runs: a takeover started or ended.
    async fn session_active(&self, _session_id: &str) {}

    /// A code the Carbon entered on the website during setup (Apple TV).
    async fn setup_code(&self, _code: &str) -> Result<(), String> {
        Err("this device has no setup code".into())
    }

    /// The Carbon asked to retry a failed setup step now (`setup_retry`): `step` names it, `None`
    /// means every failed step. A driver that retries on a timer or with backoff runs the attempt
    /// at once instead of waiting; the caller then probes and reports the result as usual. The
    /// default does nothing beyond that probe, which is all a step that is only checked needs.
    async fn retry_setup(&self, _step: Option<&str>) {}
}

/// Tiny cancellation token so drivers don't need tokio-util.
pub mod tokio_util_cancel {
    use std::sync::Arc;
    use tokio::sync::watch;

    #[derive(Debug, Clone)]
    pub struct CancelToken(Arc<watch::Sender<bool>>, watch::Receiver<bool>);

    impl Default for CancelToken {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CancelToken {
        pub fn new() -> Self {
            let (tx, rx) = watch::channel(false);
            Self(Arc::new(tx), rx)
        }
        pub fn cancel(&self) {
            let _ = self.0.send(true);
        }
        pub fn is_cancelled(&self) -> bool {
            *self.1.borrow()
        }
        pub async fn cancelled(&self) {
            let mut rx = self.1.clone();
            while !*rx.borrow() {
                if rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}
