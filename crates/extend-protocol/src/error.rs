//! Error codes. Stable, machine-readable, and each maps to one HTTP status and one CLI exit code
//! (`understanding/cli.yaml`, `errors` and `exit_codes`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotSignedIn,
    TokenExpired,
    SltInvalid,
    Unauthorized,
    NotATeamMember,
    CarbonOnly,
    SiliconOnly,
    NotOwner,
    NoAccess,
    AccessRemoved,
    NotSessionOwner,
    DeviceNotFound,
    SessionNotFound,
    FileNotFound,
    RequestNotFound,
    EnrollmentNotFound,
    PairingCodeInvalid,
    UnknownCommand,
    InvalidInput,
    ConfirmationRequired,
    DeviceInUse,
    DeviceNotInUse,
    DeviceNotReady,
    SessionEnded,
    NoSession,
    VersionConflict,
    Conflict,
    NotSelfDestructing,
    TestDeviceLimit,
    TestEnvironmentLimit,
    TestOnly,
    TestingSecretInvalid,
    TestingEnvironmentNotReady,
    DeviceOffline,
    SessionPaused,
    CommandTimeout,
    CommandFailed,
    UnsupportedOnDevice,
    RateLimited,
    ApiVersionUnsupported,
    ApiVersionMismatch,
    ApiVersionSunset,
    UpgradeRequired,
    PayloadTooLarge,
    ServiceUnavailable,
    Internal,
}

impl ErrorCode {
    pub fn http_status(self) -> u16 {
        use ErrorCode::*;
        match self {
            NotSignedIn | TokenExpired | SltInvalid | Unauthorized | TestingSecretInvalid => 401,
            NotATeamMember | CarbonOnly | SiliconOnly | NotOwner | NoAccess | AccessRemoved | NotSessionOwner => 403,
            DeviceNotFound | SessionNotFound | FileNotFound | RequestNotFound | EnrollmentNotFound
            | PairingCodeInvalid | UnknownCommand => 404,
            InvalidInput
            | ConfirmationRequired
            | NoSession
            | UnsupportedOnDevice
            | ApiVersionUnsupported
            | ApiVersionMismatch => {
                if matches!(self, ApiVersionUnsupported | ApiVersionMismatch) {
                    400
                } else {
                    422
                }
            }
            DeviceInUse | DeviceNotInUse | DeviceNotReady | SessionEnded | Conflict | NotSelfDestructing
            | TestDeviceLimit | TestEnvironmentLimit => 409,
            VersionConflict => 412,
            TestOnly => 400,
            PayloadTooLarge => 413,
            SessionPaused => 423,
            UpgradeRequired => 426,
            RateLimited => 429,
            ApiVersionSunset => 410,
            CommandFailed => 200,
            DeviceOffline | TestingEnvironmentNotReady | ServiceUnavailable => 503,
            CommandTimeout => 504,
            Internal => 500,
        }
    }

    pub fn exit_code(self) -> i32 {
        use ErrorCode::*;
        match self {
            CommandFailed => 1,
            UnknownCommand | InvalidInput | ConfirmationRequired | NoSession => 2,
            NotSignedIn | TokenExpired | SltInvalid | Unauthorized => 3,
            NotATeamMember | CarbonOnly | SiliconOnly | NotOwner | NoAccess | AccessRemoved | NotSessionOwner => 4,
            DeviceNotFound | SessionNotFound | FileNotFound | RequestNotFound | EnrollmentNotFound
            | PairingCodeInvalid => 5,
            DeviceInUse | DeviceNotInUse | DeviceNotReady | SessionEnded | VersionConflict | Conflict
            | NotSelfDestructing => 6,
            DeviceOffline => 7,
            SessionPaused => 8,
            CommandTimeout => 9,
            UnsupportedOnDevice => 10,
            TestDeviceLimit | TestEnvironmentLimit | TestOnly | TestingSecretInvalid | TestingEnvironmentNotReady => 11,
            RateLimited => 12,
            ApiVersionUnsupported | ApiVersionMismatch | ApiVersionSunset | UpgradeRequired => 13,
            PayloadTooLarge => 2,
            ServiceUnavailable | Internal => 14,
        }
    }

    pub fn as_str(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// The `data` of an error envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub details: serde_json::Value,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            docs_url: Some(format!(
                "https://docs.extend.teamofsilicons.com/errors#{}",
                code.as_str()
            )),
            request_id: String::new(),
            details: serde_json::Value::Null,
        }
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_snake_case() {
        assert_eq!(ErrorCode::DeviceInUse.as_str(), "device_in_use");
        assert_eq!(ErrorCode::DeviceInUse.http_status(), 409);
        assert_eq!(ErrorCode::DeviceInUse.exit_code(), 6);
        assert_eq!(ErrorCode::TestDeviceLimit.exit_code(), 11);
    }
}
