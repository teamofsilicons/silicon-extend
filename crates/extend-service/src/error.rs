//! One error type for every handler; rendered as the `{"type":"error","data":{...}}` envelope.

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use extend_protocol::{ApiError, ErrorCode};

/// An error answer: the envelope's `data`, and, rarely, an HTTP status other than the one its code
/// maps to (the setup retry route answers `invalid_input` with 400 and `device_offline` with 409,
/// as the shared contract for it says; clients read the code, never the status).
#[derive(Debug)]
pub struct AppError(pub Box<ApiError>, pub Option<u16>);

pub type AppResult<T> = Result<T, AppError>;

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0.code.as_str(), self.0.message)
    }
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(Box::new(ApiError::new(code, message)), None)
    }
    /// Answers with `status` instead of the code's own.
    pub fn status(mut self, status: u16) -> Self {
        self.1 = Some(status);
        self
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.0.hint = Some(hint.into());
        self
    }
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.0.details = details;
        self
    }
    pub fn code(&self) -> ErrorCode {
        self.0.code
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }
    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "internal error");
        Self::new(
            ErrorCode::Internal,
            "Extend hit an unexpected error. The request id identifies it in the service logs.",
        )
    }
    pub fn unavailable(what: &str, err: impl std::fmt::Display) -> Self {
        tracing::warn!(dependency = what, error = %err, "dependency unavailable");
        Self::new(
            ErrorCode::ServiceUnavailable,
            format!("{what} is unavailable right now: {err}"),
        )
        .hint("Retry in a moment. If it keeps failing, report it with `extend report`.")
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        Self::internal(format!("database: {err}"))
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal(format!("{err:#}"))
    }
}

tokio::task_local! {
    /// The request id of the request being handled, stamped into every error.
    pub static REQUEST_ID: String;
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status_override = self.1;
        let mut err = *self.0;
        if err.request_id.is_empty() {
            err.request_id = REQUEST_ID.try_with(Clone::clone).unwrap_or_default();
        }
        let status = StatusCode::from_u16(status_override.unwrap_or_else(|| err.code.http_status()))
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::json!({"type": "error", "data": err});
        let mut resp = (status, axum::Json(body)).into_response();
        resp.headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        resp
    }
}
