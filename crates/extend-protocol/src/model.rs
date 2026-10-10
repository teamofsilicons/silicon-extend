//! Resource shapes, matching `understanding/api.yaml` components.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::capability::{Capability, DeviceKind, DeviceOs};
use crate::ids::{DeviceId, SessionId};

pub type Timestamp = OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Carbon,
    Silicon,
}

/// A Carbon or Silicon as the wire shows it: `id` is the account's current public id (`c:ada`,
/// `si:scout`), which can change; `uuid` is its permanent Silicon Accounts uuid (API v2). The device
/// wire (`DeviceSelf.owner`) never carries `uuid`, so installed apps read exactly what they did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    #[serde(rename = "type")]
    pub kind: MemberKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 2.0: the permanent Silicon Accounts uuid (short, case-sensitive, e.g. `zQo`). API v2 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

impl Member {
    /// A Carbon or Silicon by public id, with no display name or uuid.
    pub fn new(kind: MemberKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            display_name: None,
            uuid: None,
        }
    }

    /// Adds the permanent Silicon Accounts uuid.
    pub fn with_uuid(mut self, uuid: impl Into<String>) -> Self {
        self.uuid = Some(uuid.into());
        self
    }
}

/// Who can see a device. 2.0: every device is `personal`, private to the Carbon who paired it and
/// the Silicons they give access to; an API v2 service refuses `team` (`422 invalid_input`) and
/// always answers `personal`. `team` stays only so 1.x answers still decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// 1.x only: Extend 3's shared devices. API v2 refuses it.
    Team,
    /// Private to the Carbon who paired it and the Silicons they give access to.
    #[default]
    Personal,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Team => "team",
            Self::Personal => "personal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Setup,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InUse {
    pub silicon_id: String,
    pub session_id: SessionId,
    #[serde(with = "time::serde::rfc3339")]
    pub since: Timestamp,
    #[serde(default)]
    pub paused: bool,
    /// 1.1 (API v1 only); API v2 never sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 2.0: the Silicon's permanent Silicon Accounts uuid (`silicon_id` is its current public id).
    /// API v2 only; never on the device wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silicon_uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub device_id: DeviceId,
    pub name: String,
    pub os: DeviceOs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub kind: DeviceKind,
    pub owner: Member,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub visibility: Visibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_device_id: Option<DeviceId>,
    pub state: DeviceState,
    pub online: bool,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_seen_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<InUse>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<Timestamp>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub paired_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_ttl_days: Option<i32>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub pair_expires_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_left: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// Present on single-device reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<Capability>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<MissingCapability>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    /// Set only on a removed device, which only the Carbon who paired it can read
    /// (`GET /api/v1/devices?include_removed=true`, `GET /api/v1/devices/{device_id}`).
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub removed_at: Option<Timestamp>,
    /// Why the pair ended, alongside `removed_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_reason: Option<EndReason>,

    // ── 1.1.0. A device belongs to the Carbons who paired it: each row is one Carbon's pair.
    // `team` is None in owner views and the Silicon's Team (its X-Org-ID) in Silicon views;
    // `visibility` is always `personal`. A 1.0 reader ignores every field below.
    /// Version of the device engine on the device (or, for a carried device, on its computer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// Deprecated duplicate of `engine_version`, kept for API v1 readers of the old name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_device_version: Option<String>,
    /// Whether the device is awake: screen on and unlocked, a computer awake and unlocked, a TV on.
    /// None while it is offline, or while an online device hasn't said (1.0 apps never do).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awake: Option<bool>,
    /// Why an online device isn't awake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_state: Option<SleepState>,
    /// For an offline device: how it last reported itself, when that was not awake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sleep_state: Option<SleepState>,
    /// When `awake` last changed. Owner views only.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub awake_changed_at: Option<Timestamp>,
    /// Whether Extend can tell when this device wakes. When false, its Carbon says so by answering
    /// the wake request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_detectable: Option<bool>,
    /// A Silicon the viewer can't see is using this device (or, for a computer, a device it
    /// carries). `in_use` is then absent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_use_by_other: bool,
    /// Owner views of a computer: what is busy is a device this computer carries that the viewer
    /// didn't pair. A remote Stop can't end it; the computer's own Stop can.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_use_by_other_carried: bool,
    /// Open wake requests the viewer may see: all of them on the owner's pair, a Silicon's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_wake_requests: Option<i64>,
    /// The open wake requests themselves, on single-device reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_requests: Option<Vec<WakeRequest>>,
    /// Whether the owner turned wake requests off for this pair. Owner views only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_muted: Option<bool>,
    /// Whether another Carbon also paired this device. It never says who. Owner views only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_by_others: Option<bool>,
    /// Silicon views: other pairs of this same physical device the Silicon has access to
    /// (another Carbon's id for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_device: Option<Vec<DeviceId>>,
    /// Whether the device shows the badge, banner or notification naming the Silicon using it.
    /// One setting per physical device, shared by every pair of it. Absent (a 1.0 service) means
    /// shown.
    #[serde(default)]
    pub in_use_indicator: InUseIndicator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingCapability {
    pub capability: Capability,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    InProgress,
    NeedsCarbon,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Todo,
    InProgress,
    NeedsCarbon,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStep {
    pub key: String,
    pub title: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Why the step failed, in one or two sentences for the Carbon: what is wrong and what to do
    /// ("The iPhone is locked or not connected by cable. Unlock it and keep it plugged in.").
    /// Never environment variables, file paths, build commands, exit codes or stack traces: those
    /// go to the app's or agent's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What the Carbon enters on the website for this step, if anything (`"code"` for an Apple TV).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub state: SetupState,
    pub steps: Vec<SetupStep>,
}

impl Setup {
    pub fn from_steps(steps: Vec<SetupStep>) -> Self {
        let state = if steps.iter().all(|s| s.status == StepStatus::Done) {
            SetupState::Complete
        } else if steps
            .iter()
            .any(|s| matches!(s.status, StepStatus::NeedsCarbon | StepStatus::Failed))
        {
            SetupState::NeedsCarbon
        } else {
            SetupState::InProgress
        };
        Self { state, steps }
    }
    pub fn complete() -> Self {
        Self {
            state: SetupState::Complete,
            steps: vec![],
        }
    }
    /// The steps that failed, which `setup_retry` runs again.
    pub fn failed(&self) -> impl Iterator<Item = &SetupStep> {
        self.steps.iter().filter(|s| s.status == StepStatus::Failed)
    }
    pub fn step(&self, key: &str) -> Option<&SetupStep> {
        self.steps.iter().find(|s| s.key == key)
    }
}

// ───────────── Enrollment and pairing ─────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentCreate {
    pub os: DeviceOs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub app_version: String,
    /// Version of the device engine. 1.0 apps send it as `agent_device_version`, still read.
    #[serde(default, alias = "agent_device_version", skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentCreated {
    pub enrollment_id: Uuid,
    pub enrollment_secret: String,
    pub pairing_code: String,
    #[serde(with = "time::serde::rfc3339")]
    pub code_expires_at: Timestamp,
    pub rotates_every_s: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnrollmentState {
    Waiting {
        pairing_code: String,
        #[serde(with = "time::serde::rfc3339")]
        code_expires_at: Timestamp,
    },
    Paired {
        device_id: DeviceId,
        device_credential: String,
        #[serde(default)]
        environment: Option<TestingEnvironment>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairingClaim {
    pub pairing_code: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_ttl_days: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub silicon_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentCreate {
    pub os: DeviceOs,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_ttl_days: Option<i32>,
    /// Network address of a TV the host should reach (optional; the host discovers otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DevicePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_ttl_days: Option<i32>,
}

/// The 1.1 device settings request. Kept separate so 1.0 callers constructing
/// `DevicePatch` with a struct literal remain source compatible.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_ttl_days: Option<i32>,
    /// 1.1: show or hide the in-use banner on the device (every pair of it). Carbon only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use_indicator: Option<InUseIndicator>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessGrant {
    pub device_id: DeviceId,
    pub silicon_id: String,
    pub granted_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub granted_at: Timestamp,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<Timestamp>,
    /// 1.1 (API v1 only). API v2 has one grant per pair and Silicon, and never sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 1.1: whether the owner turned off this Silicon's wake requests on this pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_muted: Option<bool>,
    /// 2.0: the Silicon's permanent Silicon Accounts uuid (`silicon_id` is its current public id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silicon_uuid: Option<String>,
    /// 2.0: the granting Carbon's permanent uuid (`granted_by` is their current public id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by_uuid: Option<String>,
    /// 2.0, custodian views (`GET /api/v2/silicons/{silicon}/grants`): the device's name and OS,
    /// and the Carbon who paired it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_os: Option<DeviceOs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Member>,
}

// ───────────── Sessions and commands ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Paused,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    EndedBySilicon,
    IdleTimeout,
    StoppedByCarbon,
    AccessRemoved,
    DeviceRemoved,
    PairRevoked,
    PairExpired,
    SiliconLoggedOut,
    LeftTeam,
    DeviceOffline,
    EnvironmentDisabled,
    EnvironmentCleaned,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EndedBySilicon => "ended_by_silicon",
            Self::IdleTimeout => "idle_timeout",
            Self::StoppedByCarbon => "stopped_by_carbon",
            Self::AccessRemoved => "access_removed",
            Self::DeviceRemoved => "device_removed",
            Self::PairRevoked => "pair_revoked",
            Self::PairExpired => "pair_expired",
            Self::SiliconLoggedOut => "silicon_logged_out",
            Self::LeftTeam => "left_team",
            Self::DeviceOffline => "device_offline",
            Self::EnvironmentDisabled => "environment_disabled",
            Self::EnvironmentCleaned => "environment_cleaned",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
    }
    pub fn explain(self) -> &'static str {
        match self {
            Self::EndedBySilicon => "the Silicon ended it",
            Self::IdleTimeout => "5 minutes passed without a command",
            Self::StoppedByCarbon => "the device's Carbon stopped it",
            Self::AccessRemoved => "the Silicon's access to the device was removed",
            Self::DeviceRemoved => "the device was removed",
            Self::PairRevoked => "the pair was revoked on the device",
            Self::PairExpired => "the device went unused for longer than its pairing lasts",
            Self::SiliconLoggedOut => "the Silicon logged out",
            Self::LeftTeam => "a membership it depended on ended (before Extend 4)",
            Self::DeviceOffline => "the device stayed offline for 2 minutes",
            Self::EnvironmentDisabled => "the test environment was disabled",
            Self::EnvironmentCleaned => "the test environment was cleaned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub session_id: SessionId,
    pub device_id: DeviceId,
    pub silicon_id: String,
    pub state: SessionState,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: Timestamp,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_command_at: Option<Timestamp>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub idle_ends_at: Option<Timestamp>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<Timestamp>,
    pub end_reason: Option<EndReason>,
    #[serde(default)]
    pub command_count: i64,
    /// Present on single-session reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Box<Device>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<Capability>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    /// 1.1 (API v1 only); API v2 never sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 2.0: the Silicon's permanent Silicon Accounts uuid (`silicon_id` is its current public id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silicon_uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCreate {
    pub device_id: DeviceId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Takeover {
    pub takeover_id: Uuid,
    pub session_id: SessionId,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: Timestamp,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeoverCreate {
    pub reason: String,
}

/// A command sent into a session. `args` are the command-line tokens after the command name,
/// exactly as the device engine's command line accepts them; the engine's parser stays the authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_destruct_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permanent: bool,
    /// Script contents for `replay`/`test`, read by the caller (the device engine reads scripts client side too).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// A small file the caller sends along with a command (a replay script, an image to display).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    /// Base64 (standard, padded).
    pub content_base64: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: Uuid,
    pub session_id: SessionId,
    pub command: String,
    pub ok: bool,
    #[serde(default)]
    pub output: serde_json::Value,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub files: Vec<FileInfo>,
    #[serde(default)]
    pub error: Option<CommandError>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: Timestamp,
    pub duration_ms: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub idle_ends_at: Option<Timestamp>,
    /// Things that went wrong around the command without failing it, each saying what happened,
    /// why, and what to do: chiefly a file the device made that Extend could not store in
    /// Briefcase, or could not share with the device's Carbon. Empty when all went well; older
    /// services leave it out.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Screenshot,
    Recording,
    Log,
    ReplayScript,
    Diff,
    Other,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Recording => "recording",
            Self::Log => "log",
            Self::ReplayScript => "replay_script",
            Self::Diff => "diff",
            Self::Other => "other",
        }
    }
    pub fn parse(s: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).unwrap_or(Self::Other)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_id: Uuid,
    pub name: String,
    pub kind: FileKind,
    pub content_type: String,
    pub size_bytes: i64,
    pub url: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub self_destruct_at: Option<Timestamp>,
    pub permanent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_with: Option<String>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<Timestamp>,
    /// 1.1 (API v1 only); API v2 never sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 2.0: the permanent uuid of the Silicon that made it (`created_by` is its current public id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_uuid: Option<String>,
    /// 2.0: the permanent uuid of the Carbon it is shared with (`shared_with` is their current id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_with_uuid: Option<String>,
}

// ───────────── Requests and activity ─────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestCreate {
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Pending,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestInfo {
    pub request_id: Uuid,
    pub device_id: DeviceId,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: Timestamp,
    pub delivery: Delivery,
    /// Why the last delivery attempt through Ting failed, while `delivery` is `pending` or once it
    /// is `failed` (what happened, why, and what to do).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 1.1 (API v1 only): set for the requester's side and its Carbon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// 1.1: who the request went to: the Silicon using the device (`holder`, as in 1.0), or the
    /// Carbon who gave that Silicon access (`carbon`), when it's on another side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_to: Option<RequestRoute>,
    /// 1.1: `to` is [`crate::REQUEST_TO_HIDDEN`]: the requester may not see who holds the device.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub to_hidden: bool,
    /// 1.1: `from` is [`crate::REQUEST_FROM_HIDDEN`]. A 1.1.0 service never sets it: the Carbon a
    /// request is routed to sees the requesting Silicon (Carbon decision, 2026-09-27).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_hidden: bool,
    /// 2.0: the requesting Silicon's permanent uuid (`from` is its current public id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_uuid: Option<String>,
    /// 2.0: the recipient's permanent uuid, when the viewer may see who it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub at: Timestamp,
    pub actor: Member,
    pub action: String,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub files: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
    /// 1.1 (API v1 only); API v2 never sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

// ───────────── Versions, reports and the device wire ─────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestingEnvironment {
    pub environment_id: Uuid,
    pub name: String,
    pub state: String,
    #[serde(default)]
    pub paired_devices: i64,
    #[serde(default = "default_limit")]
    pub device_limit: i64,
}

fn default_limit() -> i64 {
    crate::TEST_DEVICE_LIMIT
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub api_version: u32,
    pub supported: Vec<u32>,
    pub service_version: String,
    #[serde(default)]
    pub deprecated: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportInput {
    pub message: String,
    #[serde(default)]
    pub pr: Option<String>,
    pub client_version: String,
    #[serde(default)]
    pub context: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportReceipt {
    pub report_id: Uuid,
    pub notification: String,
    pub repository_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSelf {
    pub device_id: DeviceId,
    pub name: String,
    pub owner: Member,
    pub team: String,
    pub os: DeviceOs,
    pub in_use: Option<InUse>,
    pub takeover: Option<Takeover>,
    pub setup: Setup,
    pub environment: Option<TestingEnvironment>,
    /// 1.1: the physical device this pair is of. Every pair of one device shares it; an app that
    /// finds two among its credentials keeps each working and logs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,
    /// 1.1, computer pairs only: the world's key for `hardware_key` on carried devices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_salt: Option<String>,
    /// 1.1: true for the pair made by the app's first enrollment (the Carbon who installed Extend
    /// on this device), false for pairs added with "Pair with another Carbon". On a computer
    /// several Carbons paired, only Silicons given access through the first pair get the terminal
    /// (see [`crate::TERMINAL_NOT_SHARED_REASON`]). Absent from 1.0 services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_pair: Option<bool>,
    /// 1.1: whether this device shows the badge, banner or notification naming the Silicon using
    /// it. Shared by every pair of the device. Absent (a 1.0 service) means shown.
    #[serde(default)]
    pub in_use_indicator: InUseIndicator,
}

// ───────────── 1.1.0: waking a device ─────────────
//
// New enums are open (unknown values decode as `Other`); new structs are `#[non_exhaustive]`, so
// they're built with their constructors and setters, and later fields won't break callers.

open_enum! {
    /// Why an online device isn't awake.
    pub enum SleepState {
        /// A phone or tablet with its screen off.
        ScreenOff => "screen_off",
        /// Screen on, but locked.
        Locked => "locked",
        /// A computer asleep.
        Asleep => "asleep",
        /// A TV in standby.
        Standby => "standby",
        /// A computer showing another account's session.
        OtherSession => "other_session",
    }
}

impl SleepState {
    /// How Extend says it to a Carbon or Silicon: "screen off", "locked", "asleep", "standby",
    /// "another account".
    pub fn label(self) -> &'static str {
        match self {
            Self::ScreenOff => "screen off",
            Self::Locked => "locked",
            Self::Asleep => "asleep",
            Self::Standby => "standby",
            Self::OtherSession => "another account",
            Self::Other => "not awake",
        }
    }
}

open_enum! {
    /// Whether Extend's Tings reach an account.
    pub enum TingStatus {
        On => "on",
        /// The member turned Extend's Tings off in Ting.
        Off => "off",
        /// Not registered with Ting yet.
        Pending => "pending",
    }
}

open_enum! {
    /// Where a Ting about a wake request is.
    pub enum TingDelivery {
        /// Not delivered yet; it is retried.
        Pending => "pending",
        /// Held back by the hourly limit per Carbon; sent when the hour's window frees.
        Deferred => "deferred",
        Delivered => "delivered",
        Failed => "failed",
        /// No Ting of its own: an earlier Ting for the same device covers it.
        Covered => "covered",
    }
}

open_enum! {
    pub enum WakeState {
        Open => "open",
        Woken => "woken",
        Expired => "expired",
        Withdrawn => "withdrawn",
        Declined => "declined",
    }
}

open_enum! {
    /// Why a wake request ended.
    pub enum WakeEndReason {
        /// The device reported itself awake, with an unlock or real input.
        WokenOnDevice => "woken_on_device",
        /// Its Carbon answered "It's awake".
        ConfirmedByCarbon => "confirmed_by_carbon",
        Expired => "expired",
        /// The asking Silicon withdrew it.
        Cancelled => "cancelled",
        Declined => "declined",
        /// The asking Silicon started a session on the device.
        SessionStarted => "session_started",
        AccessRemoved => "access_removed",
        LeftTeam => "left_team",
        DeviceRemoved => "device_removed",
        /// Its Carbon turned wake requests off.
        Muted => "muted",
        /// Withdrawn by a rollback of the service to 1.0.
        Rollback => "rollback",
    }
}

open_enum! {
    /// What the device did with a wake request.
    pub enum DeviceNotice {
        /// Sent to the device; it hasn't answered yet.
        Sent => "sent",
        /// The device showed it.
        Shown => "shown",
        /// The device couldn't show it (`device_notice_note` says why).
        NotShown => "not_shown",
        /// The device was offline; it gets the request when it reconnects.
        Offline => "offline",
        /// The device (or the computer it pairs through) can't show wake requests; Ting is the
        /// only way it reaches the Carbon.
        Unsupported => "unsupported",
    }
}

open_enum! {
    pub enum WakeAnswerKind {
        /// "It's awake": ends every open request on the device, whichever Carbon's.
        Woken => "woken",
        /// Ends the requests on the answering Carbon's own pair.
        Declined => "declined",
    }
}

open_enum! {
    /// Who a request for a device in use went to.
    pub enum RequestRoute {
        /// The Silicon using the device (through the same Carbon's pair as the requester), as in
        /// 1.0.
        Holder => "holder",
        /// The Carbon who gave the Silicon using it access.
        Carbon => "carbon",
    }
}

/// Body of `POST /api/v1/devices/{device_id}/wake-requests`, envelope type `wake_request`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WakeCreate {
    /// Why the Silicon needs the device awake: 1–300 characters, kept exactly as written.
    pub reason: String,
}

impl WakeCreate {
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

/// Body of `POST /api/v1/devices/{device_id}/wake-requests/answer`, envelope type `wake_answer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WakeAnswer {
    pub answer: WakeAnswerKind,
    /// Only for `declined`: the requests to decline (all on this pair when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_ids: Option<Vec<Uuid>>,
}

impl WakeAnswer {
    pub fn new(answer: WakeAnswerKind) -> Self {
        Self { answer, wake_ids: None }
    }
    pub fn woken() -> Self {
        Self::new(WakeAnswerKind::Woken)
    }
    pub fn declined() -> Self {
        Self::new(WakeAnswerKind::Declined)
    }
    pub fn wake_ids(mut self, ids: Vec<Uuid>) -> Self {
        self.wake_ids = Some(ids);
        self
    }
}

/// The answer to a [`WakeAnswer`], envelope type `wake_answer`: the requests on the answering
/// Carbon's own pair that it ended. "It's awake" also ends other Carbons' requests on the device,
/// which are never listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WakeAnswered {
    pub answer: WakeAnswerKind,
    #[serde(default)]
    pub ended: Vec<WakeRequest>,
}

impl WakeAnswered {
    pub fn new(answer: WakeAnswerKind, ended: Vec<WakeRequest>) -> Self {
        Self { answer, ended }
    }
}

/// Body of `PUT /api/v1/devices/{device_id}/wake-settings`, envelope type `wake_settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WakeSettings {
    pub muted: bool,
    /// Mute one Silicon's wake requests instead of the whole pair's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silicon_id: Option<String>,
    /// 1.1 (API v1 only); API v2 refuses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

impl WakeSettings {
    pub fn new(muted: bool) -> Self {
        Self {
            muted,
            silicon_id: None,
            team: None,
        }
    }
    pub fn silicon(mut self, silicon_id: impl Into<String>) -> Self {
        self.silicon_id = Some(silicon_id.into());
        self
    }
    pub fn team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }
}

/// A Silicon whose wake requests the owner turned off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MutedSilicon {
    pub silicon_id: String,
    /// 1.1 (API v1 only); API v2 leaves it out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team: String,
}

impl MutedSilicon {
    pub fn new(silicon_id: impl Into<String>, team: impl Into<String>) -> Self {
        Self {
            silicon_id: silicon_id.into(),
            team: team.into(),
        }
    }
}

/// A pair's wake settings, envelope type `wake_settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WakeSettingsView {
    pub device_id: DeviceId,
    /// Wake requests are off for the whole pair.
    pub muted: bool,
    #[serde(default)]
    pub silicons_muted: Vec<MutedSilicon>,
}

impl WakeSettingsView {
    pub fn new(device_id: DeviceId, muted: bool) -> Self {
        Self {
            device_id,
            muted,
            silicons_muted: vec![],
        }
    }
}

/// The computer a carried device pairs through (the Carbon's own pair of it). It must be awake too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HostDevice {
    pub device_id: DeviceId,
    pub name: String,
    pub online: bool,
}

impl HostDevice {
    pub fn new(device_id: DeviceId, name: impl Into<String>, online: bool) -> Self {
        Self {
            device_id,
            name: name.into(),
            online,
        }
    }
}

/// A Silicon's request that its Carbon wake a device, envelope type `wake_request`. The owner of
/// the pair sees every request on it; a Silicon sees its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WakeRequest {
    pub wake_id: Uuid,
    /// The pair the Silicon asked through.
    pub device_id: DeviceId,
    /// 1.1 (API v1 only); API v2 leaves it out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team: String,
    /// The asking Silicon (its current public id).
    pub from: String,
    /// The Carbon who gave it access, the pair's owner (their current public id).
    pub to: String,
    /// The latest ask's reason, exactly as written.
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: Timestamp,
    #[serde(with = "time::serde::rfc3339")]
    pub last_asked_at: Timestamp,
    /// How many times the Silicon asked (asking again after 5 minutes refreshes the request).
    pub asks: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: Timestamp,
    pub state: WakeState,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub ended_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<WakeEndReason>,
    /// Whether Extend can tell when the device wakes; when false the Carbon answers instead.
    pub wake_detectable: bool,
    pub device_notice: DeviceNotice,
    /// What the device said about showing it ("Notifications are off for Silicon Extend on this phone.").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_notice_note: Option<String>,
    /// The Ting to the Carbon. A Silicon sees `deferred` as `pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ting: Option<TingDelivery>,
    /// For `ting: covered`: the request whose Ting covers this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ting_covered_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ting_last_error: Option<String>,
    /// The Ting back to the Silicon once the request is woken or declined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_ting: Option<TingDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_ting_last_error: Option<String>,
    /// For a carried device: the computer it pairs through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostDevice>,
    /// 2.0: the asking Silicon's permanent uuid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_uuid: Option<String>,
    /// 2.0: the pair owner's permanent uuid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_uuid: Option<String>,
}

impl WakeRequest {
    /// A new open request, asked once, with `device_notice: sent`; set the rest on the result.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wake_id: Uuid,
        device_id: DeviceId,
        team: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        reason: impl Into<String>,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Self {
        Self {
            wake_id,
            device_id,
            team: team.into(),
            from: from.into(),
            to: to.into(),
            reason: reason.into(),
            created_at,
            last_asked_at: created_at,
            asks: 1,
            expires_at,
            state: WakeState::Open,
            ended_at: None,
            end_reason: None,
            wake_detectable: false,
            device_notice: DeviceNotice::Sent,
            device_notice_note: None,
            ting: None,
            ting_covered_by: None,
            ting_last_error: None,
            answer_ting: None,
            answer_ting_last_error: None,
            host: None,
            from_uuid: None,
            to_uuid: None,
        }
    }
}

/// Whether Extend's Tings reach an account (`GET`/`PUT /api/v2/ting-registration`), envelope
/// type `ting_registration`. (API v1's `?team=any` answered a `list` page of these.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TingRegistration {
    /// 1.1 (API v1 only). API v2 has one registration per account and leaves it out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team: String,
    /// The member's current public id.
    pub member: String,
    pub status: TingStatus,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub registered_at: Option<Timestamp>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub refused_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// App types Ting reported missing, by full name.
    #[serde(default)]
    pub missing_types: Vec<String>,
    /// 2.0: the member's permanent Silicon Accounts uuid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_uuid: Option<String>,
    /// 2.0: whether this Extend service delivers notifications through Ting at all. `false` while
    /// Ting is not configured on the server: requests and wake requests are then only on the
    /// website, in the CLI and on the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_enabled: Option<bool>,
}

impl TingRegistration {
    pub fn new(team: impl Into<String>, member: impl Into<String>, status: TingStatus) -> Self {
        Self {
            team: team.into(),
            member: member.into(),
            status,
            registered_at: None,
            refused_at: None,
            last_error: None,
            missing_types: vec![],
            member_uuid: None,
            delivery_enabled: None,
        }
    }
}

// ───────────── 1.1.0: several Carbons, several Teams ─────────────

/// `POST /api/v1/devices/{device_id}/stop` when the session it stopped ran through another
/// Carbon's pair of the device, envelope type `device_stopped`. (When it ran through the caller's
/// own pair, the answer is the stopped `session`, as in 1.0.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DeviceStopped {
    pub device_id: DeviceId,
    #[serde(with = "time::serde::rfc3339")]
    pub stopped_at: Timestamp,
    /// Always true: the stopped Silicon was on another side, so it isn't named.
    pub in_use_by_other: bool,
}

impl DeviceStopped {
    pub fn new(device_id: DeviceId, stopped_at: Timestamp) -> Self {
        Self {
            device_id,
            stopped_at,
            in_use_by_other: true,
        }
    }
}

// ───────────── 1.1.0: setup retry ─────────────

/// Body of `POST /api/v1/devices/{device_id}/setup/retry`: `{"step":"<key>"}`, or `{}` (or no
/// body) for every failed step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SetupRetryInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

impl SetupRetryInput {
    /// Every failed step.
    pub fn all() -> Self {
        Self::default()
    }
    pub fn step(key: impl Into<String>) -> Self {
        Self { step: Some(key.into()) }
    }
}

/// 202 answer to a setup retry: the keys of the steps the device was asked to run again. The
/// service changes no step itself; the device reports progress as usual.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RetryResult {
    pub retrying: Vec<String>,
}

impl RetryResult {
    pub fn new(retrying: Vec<String>) -> Self {
        Self { retrying }
    }
}

// ───────────── 1.1.0: the in-use banner ─────────────

open_enum! {
    /// Whether a device shows that a Silicon is using it: the badge, banner or notification that
    /// names the Silicon. Shown, it appears for 10 seconds when a session starts, then hides (a
    /// menu bar or tray icon stays changed for the session). Hidden, nothing is drawn or notified
    /// about the Silicon. Either way, a takeover prompt shows until it is answered, and the
    /// Extend app's own screen and the website show who is using the device, with Stop.
    ///
    /// One setting per physical device, shared by every pair of it. Absent on the wire means
    /// `Shown`; so does a value this build doesn't know (see [`InUseIndicator::shows`]).
    #[non_exhaustive]
    #[derive(Default)]
    pub enum InUseIndicator {
        #[default]
        Shown => "shown",
        Hidden => "hidden",
    }
}

impl InUseIndicator {
    /// Seconds the badge, banner or notification stays up after a Silicon starts using the device.
    pub const AUTO_HIDE_S: u64 = 10;

    /// Whether the device should show it. Only `Hidden` hides it: a value from a newer service
    /// shows it, the safe side for the people around the device.
    pub fn shows(self) -> bool {
        self != Self::Hidden
    }

    /// "on" or "off", as the CLI and the website say it.
    pub fn on_off(self) -> &'static str {
        if self.shows() { "on" } else { "off" }
    }

    /// Reads "on"/"off" (and "shown"/"hidden").
    pub fn from_on_off(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on" | "shown" | "show" => Some(Self::Shown),
            "off" | "hidden" | "hide" => Some(Self::Hidden),
            _ => None,
        }
    }
}

/// Body of `PATCH /api/v1/device` (device credential, any of the device's pairs): the device app
/// changes its own settings. Absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSelfPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use_indicator: Option<InUseIndicator>,
}

impl DeviceSelfPatch {
    pub fn in_use_indicator(indicator: InUseIndicator) -> Self {
        Self {
            in_use_indicator: Some(indicator),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use time::macros::datetime;

    use super::*;

    const WAKE: &str = "0192f3a4-0000-7000-8000-000000000001";

    /// Serializes to exactly `expected`, and `expected` decodes back to the same value.
    fn exact<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T, expected: Value) {
        assert_eq!(serde_json::to_value(value).unwrap(), expected);
        assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
    }

    fn open_enum_round_trips<T>(all: &[T], as_str: fn(T) -> &'static str, parse: fn(&str) -> T, other: T)
    where
        T: Copy + Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        for v in all {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(as_str(*v)));
            assert_eq!(serde_json::from_value::<T>(json!(as_str(*v))).unwrap(), *v);
            assert_eq!(parse(as_str(*v)), *v);
        }
        assert_eq!(
            serde_json::from_value::<T>(json!("from_a_newer_service")).unwrap(),
            other
        );
        assert_eq!(parse("from_a_newer_service"), other);
        assert_eq!(serde_json::to_value(other).unwrap(), json!("other"));
        assert_eq!(serde_json::from_value::<T>(json!("other")).unwrap(), other);
    }

    #[test]
    fn open_enums_have_catch_alls() {
        open_enum_round_trips(
            SleepState::ALL,
            SleepState::as_str,
            SleepState::parse,
            SleepState::Other,
        );
        open_enum_round_trips(
            TingStatus::ALL,
            TingStatus::as_str,
            TingStatus::parse,
            TingStatus::Other,
        );
        open_enum_round_trips(
            TingDelivery::ALL,
            TingDelivery::as_str,
            TingDelivery::parse,
            TingDelivery::Other,
        );
        open_enum_round_trips(WakeState::ALL, WakeState::as_str, WakeState::parse, WakeState::Other);
        open_enum_round_trips(
            WakeEndReason::ALL,
            WakeEndReason::as_str,
            WakeEndReason::parse,
            WakeEndReason::Other,
        );
        open_enum_round_trips(
            DeviceNotice::ALL,
            DeviceNotice::as_str,
            DeviceNotice::parse,
            DeviceNotice::Other,
        );
        open_enum_round_trips(
            WakeAnswerKind::ALL,
            WakeAnswerKind::as_str,
            WakeAnswerKind::parse,
            WakeAnswerKind::Other,
        );
        open_enum_round_trips(
            RequestRoute::ALL,
            RequestRoute::as_str,
            RequestRoute::parse,
            RequestRoute::Other,
        );
        open_enum_round_trips(
            crate::frames::WakeEnd::ALL,
            crate::frames::WakeEnd::as_str,
            crate::frames::WakeEnd::parse,
            crate::frames::WakeEnd::Other,
        );
        assert_eq!(SleepState::OtherSession.as_str(), "other_session");
        assert_eq!(SleepState::OtherSession.label(), "another account");
        assert_eq!(TingDelivery::Deferred.to_string(), "deferred");
        assert_eq!(WakeEndReason::ALL.len(), 11);
    }

    /// A device as a 1.0.0 service sends it (owner view, single read).
    fn device_1_0() -> Value {
        json!({"device_id":"7c1e09ab","name":"Living room TV","os":"android_tv","kind":"tv",
            "owner":{"type":"carbon","id":"c:alice"},"team":"labs","visibility":"team","state":"ready",
            "online":true,"last_seen_at":"2026-09-27T10:00:00Z",
            "in_use":{"silicon_id":"si:chef","session_id":"a3f","since":"2026-09-27T09:58:00Z","paused":false},
            "access_count":2,"app_version":"1.0.2","version":7,"capabilities":["input.remote"],"missing":[],
            "commands":["tv-remote"]})
    }

    #[test]
    fn a_1_0_device_decodes() {
        let d: Device = serde_json::from_value(device_1_0()).unwrap();
        assert_eq!(d.in_use.as_ref().unwrap().team, None);
        assert_eq!(
            (d.awake, d.sleep_state, d.wake_detectable, d.paired_by_others),
            (None, None, None, None)
        );
        assert!(!d.in_use_by_other && !d.in_use_by_other_carried);
        assert_eq!((d.engine_version, d.same_device, d.wake_requests), (None, None, None));
        // Nothing new appears when a 1.1 service leaves the new fields unset, except
        // `in_use_indicator`, which a 1.1 service always writes.
        let mut back = serde_json::to_value(serde_json::from_value::<Device>(device_1_0()).unwrap()).unwrap();
        assert_eq!(back["in_use_indicator"], "shown");
        back.as_object_mut().unwrap().remove("in_use_indicator");
        assert_eq!(back, device_1_0());
    }

    fn wake_request() -> WakeRequest {
        let mut w = WakeRequest::new(
            WAKE.parse().unwrap(),
            "0d44e1f2".parse().unwrap(),
            "labs",
            "si:chef",
            "c:alice",
            "Check the order screen",
            datetime!(2026-09-27 10:02 UTC),
            datetime!(2026-09-27 10:32 UTC),
        );
        w.wake_detectable = true;
        w.ting = Some(TingDelivery::Delivered);
        w.host = Some(HostDevice::new("3a2b0c1d".parse().unwrap(), "Studio Mac", true));
        w
    }

    fn wake_request_json() -> Value {
        json!({"wake_id":WAKE,"device_id":"0d44e1f2","team":"labs","from":"si:chef","to":"c:alice",
            "reason":"Check the order screen","created_at":"2026-09-27T10:02:00Z",
            "last_asked_at":"2026-09-27T10:02:00Z","asks":1,"expires_at":"2026-09-27T10:32:00Z",
            "state":"open","wake_detectable":true,"device_notice":"sent","ting":"delivered",
            "host":{"device_id":"3a2b0c1d","name":"Studio Mac","online":true}})
    }

    #[test]
    fn a_1_1_device_round_trips() {
        let mut d: Device = serde_json::from_value(device_1_0()).unwrap();
        d.team = None;
        d.visibility = Visibility::Personal;
        d.in_use.as_mut().unwrap().team = Some("labs".into());
        d.engine_version = Some("0.21.15".into());
        d.agent_device_version = Some("0.21.15".into());
        d.awake = Some(false);
        d.sleep_state = Some(SleepState::Standby);
        d.awake_changed_at = Some(datetime!(2026-09-27 10:01 UTC));
        d.wake_detectable = Some(true);
        d.open_wake_requests = Some(1);
        d.wake_requests = Some(vec![wake_request()]);
        d.wake_muted = Some(false);
        d.paired_by_others = Some(true);
        let v = serde_json::to_value(&d).unwrap();
        assert!(v.get("team").is_none());
        assert_eq!(v["visibility"], "personal");
        assert_eq!(v["in_use"]["team"], "labs");
        assert_eq!(v["awake"], false);
        assert_eq!(v["sleep_state"], "standby");
        assert_eq!(v["awake_changed_at"], "2026-09-27T10:01:00Z");
        assert_eq!(v["wake_requests"][0], wake_request_json());
        assert_eq!(v["paired_by_others"], true);
        assert_eq!(v["engine_version"], v["agent_device_version"]);
        assert!(v.get("in_use_by_other").is_none(), "false is left out");
        assert_eq!(serde_json::from_value::<Device>(v).unwrap(), d);

        // A Silicon's view of a device another side is using.
        let mut s = d.clone();
        s.team = Some("labs".into());
        s.in_use = None;
        s.in_use_by_other = true;
        s.same_device = Some(vec!["3a2b0c1d".parse().unwrap()]);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["in_use_by_other"], true);
        assert_eq!(v["same_device"], json!(["3a2b0c1d"]));
        assert!(v.get("in_use").is_none());
        assert_eq!(serde_json::from_value::<Device>(v).unwrap(), s);

        let mut c = d;
        c.in_use = None;
        c.in_use_by_other = true;
        c.in_use_by_other_carried = true;
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            (v["in_use_by_other"].clone(), v["in_use_by_other_carried"].clone()),
            (json!(true), json!(true))
        );
        assert_eq!(serde_json::from_value::<Device>(v).unwrap(), c);
    }

    #[test]
    fn wake_types_round_trip() {
        exact(&wake_request(), wake_request_json());
        let mut ended = wake_request();
        ended.state = WakeState::Woken;
        ended.ended_at = Some(datetime!(2026-09-27 10:07 UTC));
        ended.end_reason = Some(WakeEndReason::ConfirmedByCarbon);
        ended.device_notice = DeviceNotice::NotShown;
        ended.device_notice_note = Some("This TV can't show notifications; its Carbon was told through Ting.".into());
        ended.ting = Some(TingDelivery::Covered);
        ended.ting_covered_by = Some(Uuid::nil());
        ended.answer_ting = Some(TingDelivery::Failed);
        ended.answer_ting_last_error = Some("It couldn't be delivered.".into());
        ended.ting_last_error = Some("Not delivered yet; it is retried.".into());
        let v = serde_json::to_value(&ended).unwrap();
        assert_eq!(v["state"], "woken");
        assert_eq!(v["end_reason"], "confirmed_by_carbon");
        assert_eq!(v["device_notice"], "not_shown");
        assert_eq!(v["ting"], "covered");
        assert_eq!(v["answer_ting"], "failed");
        assert_eq!(serde_json::from_value::<WakeRequest>(v).unwrap(), ended);
        // Unknown fields and values from a newer service don't break the decode.
        let mut newer = wake_request_json();
        newer["state"] = json!("snoozed");
        newer["device_notice"] = json!("vibrated");
        newer["priority"] = json!("high");
        let w: WakeRequest = serde_json::from_value(newer).unwrap();
        assert_eq!((w.state, w.device_notice), (WakeState::Other, DeviceNotice::Other));

        exact(
            &WakeCreate::new("Check the order screen"),
            json!({"reason":"Check the order screen"}),
        );
        exact(&WakeAnswer::woken(), json!({"answer":"woken"}));
        exact(
            &WakeAnswer::declined().wake_ids(vec![WAKE.parse().unwrap()]),
            json!({"answer":"declined","wake_ids":[WAKE]}),
        );
        exact(
            &WakeAnswered::new(WakeAnswerKind::Woken, vec![wake_request()]),
            json!({"answer":"woken","ended":[wake_request_json()]}),
        );
        exact(&WakeSettings::new(true), json!({"muted":true}));
        exact(
            &WakeSettings::new(false).silicon("si:chef").team("labs"),
            json!({"muted":false,"silicon_id":"si:chef","team":"labs"}),
        );
        let mut view = WakeSettingsView::new("0d44e1f2".parse().unwrap(), false);
        view.silicons_muted.push(MutedSilicon::new("si:chef", "labs"));
        exact(
            &view,
            json!({"device_id":"0d44e1f2","muted":false,"silicons_muted":[{"silicon_id":"si:chef","team":"labs"}]}),
        );
    }

    #[test]
    fn ting_registration_round_trips() {
        let mut r = TingRegistration::new("labs", "c:alice", TingStatus::On);
        r.registered_at = Some(datetime!(2026-09-27 09:00 UTC));
        r.missing_types = vec!["extend.device.wake_requested".into()];
        exact(
            &r,
            json!({"team":"labs","member":"c:alice","status":"on","registered_at":"2026-09-27T09:00:00Z",
                   "missing_types":["extend.device.wake_requested"]}),
        );
        let mut p = TingRegistration::new("globex", "c:alice", TingStatus::Pending);
        p.last_error = Some("Sign in to Extend for globex".into());
        exact(
            &p,
            json!({"team":"globex","member":"c:alice","status":"pending","last_error":"Sign in to Extend for globex","missing_types":[]}),
        );
        let t: TingRegistration =
            serde_json::from_value(json!({"team":"labs","member":"si:chef","status":"muted_forever"})).unwrap();
        assert_eq!((t.status, t.missing_types.len()), (TingStatus::Other, 0));
    }

    #[test]
    fn several_carbons_types_round_trip() {
        exact(
            &DeviceStopped::new("7c1e09ab".parse().unwrap(), datetime!(2026-09-27 10:10 UTC)),
            json!({"device_id":"7c1e09ab","stopped_at":"2026-09-27T10:10:00Z","in_use_by_other":true}),
        );
    }

    #[test]
    fn setup_retry_bodies() {
        exact(&SetupRetryInput::all(), json!({}));
        exact(&SetupRetryInput::step("usb_debugging"), json!({"step":"usb_debugging"}));
        assert_eq!(
            serde_json::from_value::<SetupRetryInput>(json!({"step":null})).unwrap(),
            SetupRetryInput::all()
        );
        exact(
            &RetryResult::new(vec!["usb_debugging".into(), "helper".into()]),
            json!({"retrying":["usb_debugging","helper"]}),
        );
    }

    #[test]
    fn in_use_indicator() {
        open_enum_round_trips(
            InUseIndicator::ALL,
            InUseIndicator::as_str,
            InUseIndicator::parse,
            InUseIndicator::Other,
        );
        assert_eq!(InUseIndicator::default(), InUseIndicator::Shown);
        assert!(InUseIndicator::Shown.shows() && !InUseIndicator::Hidden.shows() && InUseIndicator::Other.shows());
        assert_eq!(
            (InUseIndicator::Shown.on_off(), InUseIndicator::Hidden.on_off()),
            ("on", "off")
        );
        assert_eq!(InUseIndicator::from_on_off("off"), Some(InUseIndicator::Hidden));
        assert_eq!(InUseIndicator::from_on_off(" ON "), Some(InUseIndicator::Shown));
        assert_eq!(InUseIndicator::from_on_off("maybe"), None);

        // Device: absent (a 1.0 service) reads as shown; a 1.1 service always writes it.
        let d: Device = serde_json::from_value(device_1_0()).unwrap();
        assert_eq!(d.in_use_indicator, InUseIndicator::Shown);
        let mut hidden = d.clone();
        hidden.in_use_indicator = InUseIndicator::Hidden;
        let v = serde_json::to_value(&hidden).unwrap();
        assert_eq!(v["in_use_indicator"], "hidden");
        assert_eq!(serde_json::from_value::<Device>(v).unwrap(), hidden);

        // The patches: only what is set is written.
        exact(&DevicePatch::default(), json!({}));
        exact(
            &DeviceSettingsPatch {
                in_use_indicator: Some(InUseIndicator::Hidden),
                ..Default::default()
            },
            json!({"in_use_indicator":"hidden"}),
        );
        exact(&DeviceSelfPatch::default(), json!({}));
        exact(
            &DeviceSelfPatch::in_use_indicator(InUseIndicator::Shown),
            json!({"in_use_indicator":"shown"}),
        );
        assert_eq!(
            serde_json::from_value::<DeviceSelfPatch>(json!({"in_use_indicator":null})).unwrap(),
            DeviceSelfPatch::default()
        );
    }

    #[test]
    fn team_fields_on_1_0_shapes() {
        // 1.0 JSON decodes; the 1.1 field round-trips; unset, it's left out.
        let grant = json!({"device_id":"7c1e09ab","silicon_id":"si:chef","granted_by":"c:alice",
            "granted_at":"2026-09-01T00:00:00Z","last_used_at":null});
        let mut g: AccessGrant = serde_json::from_value(grant.clone()).unwrap();
        assert_eq!((g.team.as_deref(), g.wake_muted), (None, None));
        assert_eq!(serde_json::to_value(&g).unwrap(), grant);
        g.team = Some("labs".into());
        g.wake_muted = Some(true);
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!(
            (v["team"].clone(), v["wake_muted"].clone()),
            (json!("labs"), json!(true))
        );
        assert_eq!(serde_json::from_value::<AccessGrant>(v).unwrap(), g);

        let req = json!({"request_id":WAKE,"device_id":"7c1e09ab","from":"si:chef","to":"si:sous",
            "session_id":"a3f","reason":"I need the TV","created_at":"2026-09-27T10:00:00Z","delivery":"delivered"});
        let r: RequestInfo = serde_json::from_value(req.clone()).unwrap();
        assert_eq!(
            (r.team.clone(), r.routed_to, r.to_hidden, r.from_hidden),
            (None, None, false, false)
        );
        assert_eq!(serde_json::to_value(&r).unwrap(), req);
        let routed = RequestInfo {
            to: crate::REQUEST_TO_HIDDEN.into(),
            session_id: None,
            team: Some("labs".into()),
            routed_to: Some(RequestRoute::Carbon),
            to_hidden: true,
            delivery: Delivery::Pending,
            last_error: Some("Not delivered yet; it is retried.".into()),
            ..r
        };
        let v = serde_json::to_value(&routed).unwrap();
        assert_eq!(v["to"], "the Carbon who gave access to the Silicon using it");
        assert_eq!(
            (v["routed_to"].clone(), v["to_hidden"].clone()),
            (json!("carbon"), json!(true))
        );
        assert!(v.get("from_hidden").is_none() && v.get("session_id").is_none());
        assert_eq!(serde_json::from_value::<RequestInfo>(v).unwrap(), routed);

        let act = json!({"id":WAKE,"at":"2026-09-27T10:00:00Z","actor":{"type":"silicon","id":"si:chef"},
            "action":"wake_requested","session_id":null,"command":null,"args":null,"outcome":null,"files":[]});
        let mut a: ActivityEntry = serde_json::from_value(act.clone()).unwrap();
        assert_eq!(serde_json::to_value(&a).unwrap(), act);
        a.team = Some("labs".into());
        assert_eq!(serde_json::to_value(&a).unwrap()["team"], "labs");

        let sess = json!({"session_id":"a3f","device_id":"7c1e09ab","silicon_id":"si:chef","state":"active",
            "started_at":"2026-09-27T10:00:00Z","last_command_at":null,"idle_ends_at":null,"ended_at":null,
            "end_reason":null,"command_count":0});
        let mut s: Session = serde_json::from_value(sess.clone()).unwrap();
        assert_eq!(serde_json::to_value(&s).unwrap(), sess);
        s.team = Some("labs".into());
        assert_eq!(
            serde_json::from_value::<Session>(serde_json::to_value(&s).unwrap()).unwrap(),
            s
        );

        let file = json!({"file_id":WAKE,"name":"screenshot.png","kind":"screenshot","content_type":"image/png",
            "size_bytes":28,"url":"https://briefcase.example/f","self_destruct_at":null,"permanent":false});
        let mut f: FileInfo = serde_json::from_value(file.clone()).unwrap();
        assert_eq!(serde_json::to_value(&f).unwrap(), file);
        f.team = Some("labs".into());
        assert_eq!(
            serde_json::from_value::<FileInfo>(serde_json::to_value(&f).unwrap()).unwrap(),
            f
        );
    }

    #[test]
    fn device_self_and_enrollment() {
        // A 1.0 service's GET /api/v1/device.
        let old = json!({"device_id":"7c1e09ab","name":"Studio Mac","owner":{"type":"carbon","id":"c:alice"},
            "team":"labs","os":"macos","in_use":null,"takeover":null,"setup":{"state":"complete","steps":[]},
            "environment":null});
        let mut me: DeviceSelf = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(
            (me.instance_id, me.hardware_salt.clone(), me.first_pair),
            (None, None, None)
        );
        assert_eq!(me.in_use_indicator, InUseIndicator::Shown);
        let mut back = serde_json::to_value(&me).unwrap();
        assert_eq!(back["in_use_indicator"], "shown");
        back.as_object_mut().unwrap().remove("in_use_indicator");
        assert_eq!(back, old);
        me.in_use_indicator = InUseIndicator::Hidden;
        assert_eq!(serde_json::to_value(&me).unwrap()["in_use_indicator"], "hidden");
        me.instance_id = Some(WAKE.parse().unwrap());
        me.hardware_salt = Some("ab".repeat(32));
        me.first_pair = Some(true);
        let v = serde_json::to_value(&me).unwrap();
        assert_eq!(v["instance_id"], WAKE);
        assert_eq!(v["first_pair"], true);
        assert_eq!(serde_json::from_value::<DeviceSelf>(v).unwrap(), me);

        // 1.0 apps send agent_device_version; 1.1 apps send engine_version.
        let old = json!({"os":"macos","app_version":"1.0.0","agent_device_version":"0.13.0"});
        let e: EnrollmentCreate = serde_json::from_value(old).unwrap();
        assert_eq!(e.engine_version.as_deref(), Some("0.13.0"));
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"os":"macos","app_version":"1.0.0","engine_version":"0.13.0"})
        );
    }
}
