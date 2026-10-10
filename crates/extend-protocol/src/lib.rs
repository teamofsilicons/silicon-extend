//! Wire types shared by every part of Silicon Extend.
//!
//! The service, the `silicon-extend-client` crate, the `extend` CLI and the device agents all
//! depend on this crate, so a field or identifier format is defined exactly once. The formats
//! follow `understanding/TECHNICAL.md` section 1; the HTTP shapes follow `understanding/api.yaml`.
//!
//! What changed in each release, and what a caller must update, is in `CHANGELOG.md`.

#[macro_use]
mod macros;

pub mod account;
pub mod capability;
pub mod envelope;
pub mod error;
pub mod frames;
pub mod ids;
pub mod model;
pub mod ting;

pub use capability::{COMMANDS, Capability, CommandSpec, DeviceOs, NOT_EXPOSED};
pub use envelope::Envelope;
pub use error::{ApiError, ErrorCode};
pub use ids::{DeviceCredential, DeviceId, PairingCode, SessionId};

/// The API major of the device wire (`/api/v1/device…`, `/api/v1/enrollments…`, the WebSocket
/// frames). Installed device apps speak it; it doesn't change with the account API.
pub const API_VERSION: u32 = 1;
/// 2.0: the API major of every account-facing route (`/api/v2/…`): Silicon Accounts access
/// tokens as `Authorization: Bearer`. `/api/v1` account routes answer 410.
pub const ACCOUNT_API_VERSION: u32 = 2;
/// 2.0: the app id Extend has in Silicon Accounts and Silicon Apps.
pub const APP_ID: &str = "extend";
/// Request header listing the majors a client supports.
pub const SUPPORTED_VERSIONS_HEADER: &str = "Silicon-Extend-Supported-API-Versions";
/// Response and request header carrying the agreed major.
pub const API_VERSION_HEADER: &str = "Silicon-Extend-API-Version";
/// 1.x only: the header that selected a test environment. A 2.0 service has no test
/// environments and refuses a request that sends it.
pub const TESTING_SECRET_HEADER: &str = "X-Testing-Application-Secret";
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
pub const TEST_DEVICE_LIMIT_MESSAGE: &str = "In test environment you are limited to 5 paired devices per environment.";
/// Default and bounds for a command deadline.
pub const COMMAND_TIMEOUT_DEFAULT_MS: u64 = 30_000;
pub const COMMAND_TIMEOUT_MIN_MS: u64 = 1_000;
pub const COMMAND_TIMEOUT_MAX_MS: u64 = 300_000;
/// Default and bounds for file self-destruct, in minutes.
pub const SELF_DESTRUCT_DEFAULT_MIN: u32 = 1_440;
pub const SELF_DESTRUCT_MAX_MIN: u32 = 43_200;
/// Largest file a device may upload.
pub const MAX_ARTIFACT_BYTES: u64 = 1 << 30;

// ───────────── 1.1.0: waking a device ─────────────

/// Seconds a wake request stays open after its latest ask, unless the device wakes first.
pub const WAKE_REQUEST_TTL_S: i64 = 1_800;
/// Seconds before a Silicon may ask again to wake the same device (asking again refreshes the request).
pub const WAKE_ASK_AGAIN_AFTER_S: i64 = 300;
/// Seconds between wake notifications that sound on the device itself.
pub const WAKE_ALERT_EVERY_S: i64 = 900;
/// Seconds between wake Tings to a Carbon for one pair.
pub const WAKE_TING_EVERY_S: i64 = 900;
/// Wake Tings one Carbon gets per hour at most; later asks are deferred, never refused.
pub const WAKE_TINGS_PER_CARBON_PER_HOUR: i64 = 6;
/// Longest note a device may attach to `wake_request_shown`, in Unicode scalar values.
pub const WAKE_NOTE_MAX_CHARS: usize = 300;

// ───────────── 1.1.0: requests and sides ─────────────

/// `to` on a request routed to a Carbon, as the requesting side reads it: the requester never
/// learns which Carbon, Silicon or session holds the device.
pub const REQUEST_TO_HIDDEN: &str = "the Carbon who gave access to the Silicon using it";
/// `from` on a request routed to a Carbon, when the service hides the requesting Silicon.
/// Carbon decision (2026-09-27): the Carbon a request is routed to sees the requesting Silicon's
/// id and reason, so a 1.1.0 service doesn't hide it; the value stays for services that do.
pub const REQUEST_FROM_HIDDEN: &str = "a Silicon another Carbon gave access to";
/// Length of a side tag: the first hex characters of an HMAC, so it names no account.
pub const SIDE_TAG_LEN: usize = 16;

// ───────────── 1.1.0: setup retry ─────────────

/// Fewest seconds between two setup retries of one device (`POST .../setup/retry`).
pub const SETUP_RETRY_EVERY_S: i64 = 5;

/// Features an app or agent advertises in `hello.features`.
pub mod feature {
    /// Reruns failed setup steps on a `setup_retry` frame.
    pub const SETUP_RETRY: &str = "setup_retry";
}

// ───────────── 1.1.0: computers paired by several Carbons ─────────────

/// The `missing` reason for the terminal on a computer several Carbons paired, for pairs other
/// than the one made by the app's first enrollment. The terminal runs as the computer's own
/// account, so only Silicons given access by the Carbon who installed Extend on it get it
/// (Carbon decision, 2026-09-27). It names no Carbon, since the reader may be another side.
pub const TERMINAL_NOT_SHARED_REASON: &str = "Several Carbons paired this computer. Only Silicons given access by the Carbon who installed Silicon Extend on it can use its terminal. The screen, keyboard and apps work as usual.";
