//! Wire types shared by every part of Silicon Extend.
//!
//! The service, the `silicon-extend-client` crate, the `extend` CLI and the device agents all
//! depend on this crate, so a field or identifier format is defined exactly once. The formats
//! follow `understanding/TECHNICAL.md` section 1; the HTTP shapes follow `understanding/api.yaml`.

pub mod capability;
pub mod envelope;
pub mod error;
pub mod frames;
pub mod ids;
pub mod model;

pub use capability::{Capability, CommandSpec, DeviceOs, COMMANDS, NOT_EXPOSED};
pub use envelope::Envelope;
pub use error::{ApiError, ErrorCode};
pub use ids::{DeviceId, PairingCode, SessionId};

/// The API major this build speaks.
pub const API_VERSION: u32 = 1;
/// Request header listing the majors a client supports.
pub const SUPPORTED_VERSIONS_HEADER: &str = "Silicon-Extend-Supported-API-Versions";
/// Response and request header carrying the agreed major.
pub const API_VERSION_HEADER: &str = "Silicon-Extend-API-Version";
/// Header selecting a test environment by its test application secret.
pub const TESTING_SECRET_HEADER: &str = "X-Testing-Application-Secret";
/// IAM's wire name for the Team handle.
pub const TEAM_HEADER: &str = "X-Org-ID";
/// Seconds a pairing code stays valid.
pub const PAIRING_CODE_TTL_S: i64 = 300;
/// Seconds without a command before a session ends on its own.
pub const SESSION_IDLE_S: i64 = 300;
/// Seconds a device may stay disconnected during a session before it ends.
pub const SESSION_OFFLINE_GRACE_S: i64 = 120;
/// Seconds between service pings on a device socket.
pub const HEARTBEAT_S: u64 = 15;
/// Seconds without a pong before a device counts as offline.
pub const OFFLINE_AFTER_S: u64 = 45;
/// Longest a takeover may pause a session.
pub const TAKEOVER_MAX_S: i64 = 30 * 60;
/// Longest a request reason may be, in Unicode scalar values.
pub const REASON_MAX_CHARS: usize = 300;
/// Most devices one test environment may pair.
pub const TEST_DEVICE_LIMIT: i64 = 5;
/// Most test environments active at once across the deployment.
pub const TEST_ENVIRONMENT_LIMIT: i64 = 10;
/// The exact message `UNDERSTANDING.md` requires when a test environment is full.
pub const TEST_DEVICE_LIMIT_MESSAGE: &str =
    "In test environment you are limited to 5 paired devices per environment.";
/// Default and bounds for a command deadline.
pub const COMMAND_TIMEOUT_DEFAULT_MS: u64 = 30_000;
pub const COMMAND_TIMEOUT_MIN_MS: u64 = 1_000;
pub const COMMAND_TIMEOUT_MAX_MS: u64 = 300_000;
/// Default and bounds for file self-destruct, in minutes.
pub const SELF_DESTRUCT_DEFAULT_MIN: u32 = 1_440;
pub const SELF_DESTRUCT_MAX_MIN: u32 = 43_200;
/// Largest file a device may upload.
pub const MAX_ARTIFACT_BYTES: u64 = 1 << 30;
