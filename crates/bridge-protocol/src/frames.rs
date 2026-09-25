//! WebSocket frames between a Bridge app and the service (TECHNICAL.md section 7).
//!
//! Every frame is a JSON text message `{"type": ..., "id": ..., "data": ...}`. Files never travel
//! on the socket; a device uploads them over HTTP with the upload ids a `command` carries.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capability::{Capability, DeviceOs};
use crate::ids::{DeviceId, SessionId};
use crate::model::{Attachment, EndReason, MissingCapability, Setup, TestingEnvironment, Timestamp};

/// Frames a paired device sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceFrame {
    /// First frame after connecting, and again whenever capabilities change.
    Hello(Hello),
    SetupProgress { setup: Setup },
    /// The answer to a `command` with the same id.
    Result(CommandOutcome),
    /// The Carbon tapped Stop.
    Stop,
    /// The Carbon tapped Done on a takeover.
    TakeoverDone,
    /// A device paired through this host changed state.
    Attached(AttachedStatus),
    Pong { nonce: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub app_version: String,
    pub os: DeviceOs,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agent_device_version: Option<String>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub missing: Vec<MissingCapability>,
    pub setup: Setup,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachedStatus {
    pub device_id: DeviceId,
    pub online: bool,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub missing: Vec<MissingCapability>,
    pub setup: Setup,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub id: Uuid,
    pub ok: bool,
    #[serde(default)]
    pub output: serde_json::Value,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub error: Option<crate::model::CommandError>,
    /// Files uploaded for this command, each under one of the command's upload ids.
    #[serde(default)]
    pub files: Vec<ProducedFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducedFile {
    pub upload_id: Uuid,
    pub name: String,
    pub content_type: String,
    pub kind: crate::model::FileKind,
    pub size_bytes: i64,
}

/// Frames the service sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceFrame {
    /// Run a command. `target` is set when the command is for a device paired through this host.
    Command(CommandFrame),
    Cancel { id: Uuid },
    SessionStarted {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
        silicon_id: String,
        #[serde(with = "time::serde::rfc3339")]
        since: Timestamp,
    },
    SessionEnded {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
        reason: EndReason,
    },
    Takeover {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
        reason: String,
        #[serde(with = "time::serde::rfc3339")]
        expires_at: Timestamp,
    },
    TakeoverEnded {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
    },
    /// The device's name, owner or environment changed; re-read `GET /api/v1/device`.
    Refresh,
    /// A device paired through this host needs setting up (or was removed when `removed`).
    Attach {
        device_id: DeviceId,
        os: DeviceOs,
        name: String,
        #[serde(default)]
        address: Option<String>,
        #[serde(default)]
        removed: bool,
    },
    /// A setup code the Carbon entered for an attached device (Apple TV).
    SetupCode { device_id: DeviceId, code: String },
    Environment { environment: Option<TestingEnvironment> },
    /// The pair ended; forget the credential and show the pairing screen.
    Unpaired { reason: EndReason },
    /// A newer connection replaced this one; close without reconnecting.
    Superseded,
    Ping { nonce: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandFrame {
    pub id: Uuid,
    pub session_id: SessionId,
    #[serde(default)]
    pub target: Option<DeviceId>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Milliseconds the device has before the service gives up.
    pub timeout_ms: u64,
    /// Upload ids for files this command may produce, used in order.
    pub upload_ids: Vec<Uuid>,
}

/// Frames on an unpaired app's enrollment socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnrollmentFrame {
    Code {
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
    Ping { nonce: u64 },
}

/// WebSocket close codes.
pub mod close {
    pub const UNAUTHORIZED: u16 = 4401;
    pub const SUPERSEDED: u16 = 4409;
    pub const UPGRADE_REQUIRED: u16 = 4426;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let f = ServiceFrame::Command(CommandFrame {
            id: Uuid::nil(),
            session_id: "a3f".parse().unwrap(),
            target: None,
            command: "click".into(),
            args: vec!["@e2".into()],
            attachments: vec![],
            timeout_ms: 30_000,
            upload_ids: vec![],
        });
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.starts_with("{\"type\":\"command\""), "{s}");
        assert_eq!(serde_json::from_str::<ServiceFrame>(&s).unwrap(), f);
        let stop: DeviceFrame = serde_json::from_str(r#"{"type":"stop"}"#).unwrap();
        assert_eq!(stop, DeviceFrame::Stop);
    }
}
