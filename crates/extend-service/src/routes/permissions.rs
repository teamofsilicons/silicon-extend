//! Feature consent is deliberately separate from account login. Responses never expose OBO credentials.
use super::{Body, ok};
use crate::{
    error::{AppError, AppResult},
    obo::{CompleteInput, PermissionInput},
    state::{Auth, Shared},
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use uuid::Uuid;
fn retry_key(headers: &HeaderMap) -> AppResult<Uuid> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| AppError::invalid("Send a non-nil UUID Idempotency-Key and reuse it for an identical retry."))
}
pub async fn list(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    Ok(ok(
        "permissions",
        state.iam.permissions(&auth.p, auth.sel.as_ref()).await?,
    ))
}
pub async fn start(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    Body(input): Body<PermissionInput>,
) -> AppResult<Response> {
    Ok(ok(
        "permission",
        state
            .iam
            .permission_start(&auth.p, input, retry_key(&headers)?, auth.sel.as_ref())
            .await?,
    ))
}
pub async fn complete(
    State(state): State<Shared>,
    auth: Auth,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Body(input): Body<CompleteInput>,
) -> AppResult<Response> {
    retry_key(&headers)?;
    Ok(ok(
        "permissions",
        state
            .iam
            .permission_complete(&auth.p, id, input.code.trim(), auth.sel.as_ref())
            .await?,
    ))
}
