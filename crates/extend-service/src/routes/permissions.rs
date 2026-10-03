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
fn validate_callback(input: &PermissionInput, website_url: &str) -> AppResult<()> {
    if let Some(callback) = &input.callback {
        let expected = format!("{}/auth/obo/callback", website_url.trim_end_matches('/'));
        if callback.redirect_uri != expected
            || !(32..=512).contains(&callback.state.len())
            || !callback
                .state
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(AppError::invalid(
                "Use the configured Extend approval callback and an unpredictable state.",
            ));
        }
    }
    Ok(())
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
    validate_callback(&input, &state.cfg.website_url)?;
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
            .permission_complete(&auth.p, id, &input, auth.sel.as_ref())
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obo::PermissionCallback;
    #[test]
    fn popup_callback_cannot_escape_the_configured_origin_or_change_route() {
        let mut input = PermissionInput {
            endpoints: vec![],
            callback: None,
        };
        assert!(validate_callback(&input, "https://extend.example").is_ok());
        for uri in [
            "https://evil.example/auth/obo/callback",
            "https://extend.example/auth/obo/callback?next=evil",
            "https://extend.example/other",
            "https://extend.example@evil.example/auth/obo/callback",
        ] {
            input.callback = Some(PermissionCallback {
                redirect_uri: uri.into(),
                state: "a".repeat(43),
            });
            assert!(validate_callback(&input, "https://extend.example").is_err());
        }
        input.callback = Some(PermissionCallback {
            redirect_uri: "https://extend.example/auth/obo/callback".into(),
            state: "a".repeat(43),
        });
        assert!(validate_callback(&input, "https://extend.example").is_ok());
        input.callback.as_mut().unwrap().state = "short".into();
        assert!(validate_callback(&input, "https://extend.example").is_err());
    }
}
