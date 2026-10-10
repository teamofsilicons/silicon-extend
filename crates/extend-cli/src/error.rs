//! The CLI's errors: what failed, why, and what to run next, with a stable code and exit code
//! (`understanding/cli.yaml`, `errors` and `exit_codes`).

use extend_protocol::{ApiError, ErrorCode};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
    pub request_id: Option<String>,
    pub docs_url: Option<String>,
    /// Boxed to keep `Result<_, CliError>` small.
    pub details: Box<Value>,
    /// The HTTP status, when Extend answered.
    pub status: Option<u16>,
}

pub type R<T> = Result<T, CliError>;

impl CliError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            request_id: None,
            docs_url: None,
            details: Box::new(Value::Null),
            status: None,
        }
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
    pub fn details(mut self, d: Value) -> Self {
        self.details = Box::new(d);
        self
    }
    /// A usage error (exit 2). Every one says what to type instead.
    pub fn usage(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message).hint(hint)
    }
    pub fn exit(&self) -> i32 {
        self.code.exit_code()
    }
    /// The `--json` form: `{"error": {code, message, hint, request_id, docs_url, details, exit_code}}`.
    pub fn to_json(&self) -> Value {
        json!({"error": {
            "code": self.code.as_str(),
            "message": self.message,
            "hint": self.hint,
            "request_id": self.request_id,
            "docs_url": self.docs_url,
            "details": self.details,
            "exit_code": self.exit(),
        }})
    }
}

impl From<silicon_extend_client::Error> for CliError {
    fn from(e: silicon_extend_client::Error) -> Self {
        match e {
            silicon_extend_client::Error::Api { status, error } => {
                let ApiError {
                    code,
                    message,
                    hint,
                    request_id,
                    details,
                    docs_url,
                } = *error;
                Self {
                    code,
                    message,
                    hint,
                    request_id: Some(request_id).filter(|r| !r.is_empty()),
                    docs_url,
                    details: Box::new(details),
                    status: Some(status),
                }
            }
            silicon_extend_client::Error::Transport { url, source } => Self::new(
                ErrorCode::ServiceUnavailable,
                format!("Could not reach Silicon Extend at {url}: {source}"),
            )
            .hint("Check your network, or the api_url setting (`extend config get api_url`, or EXTEND_API_URL)."),
            silicon_extend_client::Error::Decode { status, detail } => {
                let mut e = Self::new(
                    ErrorCode::Internal,
                    format!("Extend answered {status} with something this CLI can't read: {detail}"),
                )
                .hint("Silicon Apps keeps extend up to date; check for an update now with `silicon-apps update extend`. If it persists, `extend report` it.");
                e.status = Some(status);
                e
            }
            silicon_extend_client::Error::Invalid(m) => Self::usage(
                m,
                "Check the value named above; `extend config ls` shows the settings and EXTEND_API_URL overrides api_url.",
            ),
        }
    }
}

impl From<silicon_extend_client::auth::AuthError> for CliError {
    /// A Silicon Accounts sign-in failure: `details.reason` is its stable code (`slt_already_used`,
    /// `device_expired`, `sign_in_ended`, `accounts_unreachable`, …), the message and hint its own.
    fn from(e: silicon_extend_client::auth::AuthError) -> Self {
        let code = match e.code.as_str() {
            c if c.starts_with("slt_") || c == "not_an_slt" => ErrorCode::SltInvalid,
            "sign_in_ended" => ErrorCode::TokenExpired,
            "device_denied" | "device_expired" => ErrorCode::NotSignedIn,
            "public_client_off" | "device_flow_off" | "unknown_app" => ErrorCode::Unauthorized,
            "rate_limited" => ErrorCode::RateLimited,
            "invalid_accounts_url" | "invalid_input" => ErrorCode::InvalidInput,
            "accounts_unreachable" | "accounts_error" | "temporarily_unavailable" | "server_error" => {
                ErrorCode::ServiceUnavailable
            }
            "unexpected_response" => ErrorCode::Internal,
            _ if e.status.is_some_and(|s| s >= 500) => ErrorCode::ServiceUnavailable,
            _ => ErrorCode::Unauthorized,
        };
        let mut out = Self::new(code, e.message.clone())
            .hint(e.hint.clone())
            .details(json!({"reason": e.code, "service": "silicon-accounts"}));
        out.request_id = e.request_id.clone();
        out.status = e.status;
        out
    }
}

impl From<anyhow::Error> for CliError {
    /// Local state problems (reading or writing files under the state directory).
    fn from(e: anyhow::Error) -> Self {
        Self::usage(
            format!("{e:#}"),
            format!(
                "Extend keeps its state in {}. Check that it exists and this user can write it, or move it with `extend config home <dir>`.",
                crate::store::root().display()
            ),
        )
    }
}
