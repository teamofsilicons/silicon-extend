//! Durable, cross-instance claims for explicit Idempotency-Keys, using the existing response table.
//!
//! A claim is a valid 503 error envelope, so older readers of the same route/key fail safely.
//! Deployments must still stop old writers before starting this implementation: an old writer
//! that already missed the cache does not reserve before running. Route namespaces must also
//! match; this is not a cross-version migration of previously changed route keys.
//!
//! No connection or transaction is held while running the operation or waiting for its owner.
//! Every explicit result, including an error, is persisted before it is returned. Cancellation,
//! process death, or a failed final write leaves an indeterminate claim; it is never timed out
//! and stolen. Re-executing such work requires operation-specific reconciliation, not a new key.
//! This prevents concurrent duplicate execution, not crash-safe exactly-once provider delivery.

use std::time::Duration;

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use extend_protocol::ErrorCode;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout_at};
use uuid::Uuid;

use crate::db::World;
use crate::error::{AppError, AppResult, REQUEST_ID};
use crate::state::Shared;

const WAIT_LIMIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const CLAIM_MARKER: &str = "__extend_idempotency";

/// Replays the first persisted status/body for a key, or reserves it before invoking `run`.
/// Callers must authenticate and check current visibility before invoking this helper.
pub async fn idempotent<F, Fut>(
    state: &Shared,
    world: &World,
    principal: &str,
    route: &str,
    headers: &HeaderMap,
    body_hash: &str,
    run: F,
) -> AppResult<Response>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = AppResult<(StatusCode, &'static str, Value)>>,
{
    let Some(key) = headers.get("idempotency-key") else {
        let (status, kind, data) = run().await?;
        return Ok(response(status, json!({"type": kind, "data": data}), false));
    };
    let key = key
        .to_str()
        .ok()
        .filter(|key| (8..=255).contains(&key.len()) && key.bytes().all(|b| (b'!'..=b'~').contains(&b)))
        .ok_or_else(|| AppError::invalid("Idempotency-Key must be 8–255 printable characters."))?;
    let (_, pending) = error_body(indeterminate().details(json!({
        CLAIM_MARKER: {"version": 1, "owner": Uuid::new_v4().to_string()}
    })));
    // The deadline covers pool acquisition and SQL as well as duplicate polling. A cancelled
    // INSERT may have committed; leaving that claim indeterminate is safer than rerunning it.
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let found: Option<(String, i32, Value)> = timeout_at(
            deadline,
            sqlx::query_as(sql!(
                "SELECT request_hash, status, response FROM {} WHERE principal=$1 AND route=$2 AND key=$3",
                world.t("idempotency")
            ))
            .bind(principal)
            .bind(route)
            .bind(key)
            .fetch_optional(&state.pool),
        )
        .await
        .map_err(|_| indeterminate())??;
        if let Some((hash, status, body)) = found {
            if hash != body_hash {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "This Idempotency-Key was already used with a different body.",
                )
                .hint("Use a new key for a new request; reuse a key only to retry the identical request."));
            }
            if !is_pending(status, &body) {
                let status = u16::try_from(status)
                    .ok()
                    .and_then(|value| StatusCode::from_u16(value).ok())
                    .ok_or_else(|| AppError::internal("invalid stored idempotency response status"))?;
                return Ok(response(status, body, true));
            }
            timeout_at(deadline, sleep(POLL_INTERVAL))
                .await
                .map_err(|_| indeterminate())?;
            continue;
        }
        let inserted = timeout_at(
            deadline,
            sqlx::query(sql!(
                "INSERT INTO {} (principal,route,key,request_hash,status,response) VALUES ($1,$2,$3,$4,503,$5) ON CONFLICT DO NOTHING",
                world.t("idempotency")
            ))
            .bind(principal)
            .bind(route)
            .bind(key)
            .bind(body_hash)
            .bind(&pending)
            .execute(&state.pool),
        )
        .await
        .map_err(|_| indeterminate())??;
        if inserted.rows_affected() == 1 {
            break;
        }
        // Another instance won between SELECT and INSERT. Read its hash/result, never run ours.
    }

    let (status, body) = match run().await {
        Ok((status, kind, data)) => (status, json!({"type": kind, "data": data})),
        // An error is still an explicit answer; it may follow a committed operation or a provider
        // side effect. Never clear a claim merely because the operation returned Err.
        Err(error) => error_body(error),
    };
    let persisted = timeout_at(
        Instant::now() + WAIT_LIMIT,
        sqlx::query(sql!(
            "UPDATE {} SET status=$5,response=$6 WHERE principal=$1 AND route=$2 AND key=$3 AND request_hash=$4 AND status=503 AND response=$7",
            world.t("idempotency")
        ))
        .bind(principal)
        .bind(route)
        .bind(key)
        .bind(body_hash)
        .bind(i32::from(status.as_u16()))
        .bind(&body)
        .bind(&pending)
        .execute(&state.pool),
    )
    .await;
    match persisted {
        Ok(Ok(result)) if result.rows_affected() == 1 => Ok(response(status, body, false)),
        other => {
            tracing::error!(result = ?other, "could not finalize owned idempotency response");
            Err(indeterminate())
        }
    }
}

fn indeterminate() -> AppError {
    AppError::new(
        ErrorCode::ServiceUnavailable,
        "The result of this Idempotency-Key is not available yet; its operation may already have taken effect.",
    )
    .hint("Retry the identical request with the same key, or inspect its operation before contacting support. Do not use a new key to retry an uncertain result.")
}

fn is_pending(status: i32, body: &Value) -> bool {
    let marker = &body["data"]["details"][CLAIM_MARKER];
    status == 503
        && body["type"] == "error"
        && body["data"]["code"] == "service_unavailable"
        && body["data"]["details"]
            .as_object()
            .is_some_and(|details| details.len() == 1)
        && marker.as_object().is_some_and(|marker| marker.len() == 2)
        && marker["version"] == 1
        && marker["owner"]
            .as_str()
            .is_some_and(|owner| Uuid::parse_str(owner).is_ok())
}

/// Match AppError::into_response, including the original request id and unusual status overrides.
fn error_body(error: AppError) -> (StatusCode, Value) {
    let status_override = error.1;
    let mut error = *error.0;
    if error.request_id.is_empty() {
        error.request_id = REQUEST_ID.try_with(Clone::clone).unwrap_or_default();
    }
    let status = StatusCode::from_u16(status_override.unwrap_or_else(|| error.code.http_status()))
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, json!({"type": "error", "data": error}))
}

fn response(status: StatusCode, body: Value, replay: bool) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    if replay {
        response
            .headers_mut()
            .insert("idempotency-replayed", HeaderValue::from_static("true"));
    }
    response
}
