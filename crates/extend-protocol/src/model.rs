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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    #[serde(rename = "type")]
    pub kind: MemberKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Team,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_device_version: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessGrant {
    pub device_id: DeviceId,
    pub silicon_id: String,
    pub granted_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub granted_at: Timestamp,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<Timestamp>,
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
            Self::LeftTeam => "a member left the team",
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
/// exactly as agent-device's CLI accepts them; agent-device's parser stays the authority.
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
    /// Script contents for `replay`/`test`, read by the caller (agent-device reads scripts client side too).
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
}

// ───────────── Auth, discovery, testing ─────────────

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
pub struct LoginInput {
    pub slt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshInput {
    pub refresh_token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogoutInput {
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthSession {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub member: Member,
    pub teams: Vec<String>,
    #[serde(default)]
    pub testing_environment: Option<TestingEnvironment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub authenticated: bool,
    pub member: Member,
    pub teams: Vec<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub team_role: Option<String>,
    #[serde(default)]
    pub testing_environment: Option<TestingEnvironment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IamInfo {
    pub app_id: String,
    pub iam_base_url: String,
    /// Where to send a Carbon to sign in: `{iam_login_url}?app_id=…&redirect_uri=…`; IAM appends `slt=…`.
    #[serde(default)]
    pub iam_login_url: Option<String>,
    pub api_base_url: String,
    pub website_url: String,
    pub docs_url: String,
    pub repository_url: String,
    #[serde(default)]
    pub testing_environment: Option<TestingEnvironment>,
}

/// A Silicon in the caller's team, for picking who gets access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamSilicon {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
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
}
