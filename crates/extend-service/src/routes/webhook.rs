//! `POST /webhooks/accounts`: Silicon Accounts' signed events about the accounts that use Extend.
//!
//! - The signature (`X-Accounts-Signature: v1=<hex HMAC-SHA256>` over `"{X-Accounts-Timestamp}.{raw
//!   body}"`, 5 minutes of tolerance) is verified over the exact raw body before anything else is
//!   read, with `EXTEND_ACCOUNTS_WEBHOOK_SECRET`, then `EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET`
//!   during a rotation. A bad signature answers 401, a body that isn't an event 400.
//! - Deliveries are deduplicated on `event_id` (retries and replays reuse it): an event is recorded
//!   only after its effects succeeded, so a failure answers 500 and Accounts' retry applies it
//!   again. Effects are idempotent (crate::lifecycle).
//! - Unknown event types are acknowledged (2xx) and logged.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use extend_protocol::ErrorCode;
use silicon_accounts_client::{
    DEFAULT_WEBHOOK_TOLERANCE, SIGNATURE_HEADER, TIMESTAMP_HEADER, WebhookError, verify_and_parse_webhook,
};

use crate::error::AppError;
use crate::state::Shared;

const MAX_BODY: usize = 1 << 20;

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

pub async fn receive(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if body.len() > MAX_BODY {
        return AppError::invalid("Silicon Accounts webhook bodies are limited to 1 MiB.").into_response();
    }
    let Some(secret) = state.cfg.webhook_secret.as_deref() else {
        tracing::warn!("a Silicon Accounts webhook arrived, but EXTEND_ACCOUNTS_WEBHOOK_SECRET is not set; refused");
        return AppError::new(
            ErrorCode::Unauthorized,
            "This Extend server has no webhook signing secret (EXTEND_ACCOUNTS_WEBHOOK_SECRET), so it can't trust any delivery.",
        )
        .into_response();
    };
    let (ts, sig) = (header(&headers, TIMESTAMP_HEADER), header(&headers, SIGNATURE_HEADER));
    let mut verified = verify_and_parse_webhook(secret, ts, sig, &body, DEFAULT_WEBHOOK_TOLERANCE);
    if matches!(verified, Err(WebhookError::SignatureMismatch))
        && let Some(previous) = state.cfg.webhook_previous_secret.as_deref()
    {
        verified = verify_and_parse_webhook(previous, ts, sig, &body, DEFAULT_WEBHOOK_TOLERANCE);
    }
    let event = match verified {
        Ok(e) => e,
        Err(WebhookError::InvalidBody(why)) => {
            tracing::warn!(error = %why, "a signed Silicon Accounts webhook body is not an event");
            let mut resp = AppError::invalid(why.to_string())
                .hint("Silicon Accounts sends events as documented at developers.teamofsilicons.com; nothing was applied.")
                .into_response();
            *resp.status_mut() = StatusCode::BAD_REQUEST;
            return resp;
        }
        Err(e) => {
            tracing::warn!(error = %e, "refused a Silicon Accounts webhook delivery");
            return AppError::new(
                ErrorCode::Unauthorized,
                format!("The webhook delivery was refused: {e}"),
            )
            .into_response();
        }
    };
    match crate::lifecycle::receive(&state, &event).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(event_id = %event.event_id, event_type = %event.event_type, error = %e, "applying a Silicon Accounts event failed; it will be retried");
            e.into_response()
        }
    }
}

/// `POST /webhook/`: Silicon IAM's webhook, retired in 4.0.
pub async fn retired() -> Response {
    let mut resp = AppError::new(
        ErrorCode::ApiVersionSunset,
        "Silicon Extend 4 no longer takes Silicon IAM events. Silicon Accounts delivers to POST /webhooks/accounts.",
    )
    .into_response();
    *resp.status_mut() = StatusCode::GONE;
    resp
}
