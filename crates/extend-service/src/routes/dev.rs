//! Development-only routes that stand in for Silicon Accounts and Ting when
//! `EXTEND_ACCOUNTS_MODE=local` (and `EXTEND_TING_MODE=local`). Every handler refuses unless the
//! local stand-in is active, which production configuration forbids.
//!
//! `POST /dev/accounts/token {"type":"dev_token","data":{"id":"si:scout","custodian":"c:ada"}}`
//! creates the account if it is new and answers `{access_token, account}`: an EdDSA access token
//! for Extend, signed by the stand-in, verified like a real one.

use axum::extract::State;
use axum::response::Response;
use extend_protocol::ErrorCode;
use serde::Deserialize;

use super::{Body, no_content, ok};
use crate::accounts::local::LocalAccounts;
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Shared};

fn local(state: &AppState) -> AppResult<&LocalAccounts> {
    state.accounts.local.as_deref().ok_or_else(|| {
        AppError::new(
            ErrorCode::UnknownCommand,
            "Development routes exist only with EXTEND_ACCOUNTS_MODE=local.",
        )
    })
}

#[derive(Deserialize)]
pub struct TokenRequest {
    /// `c:…` or `si:…`.
    id: String,
    /// A Silicon's custodian (`c:…`); created too if new.
    #[serde(default)]
    custodian: Option<String>,
    /// Lifetime in seconds (default 1800, like Silicon Accounts).
    #[serde(default)]
    ttl_s: Option<i64>,
}

pub async fn token(State(state): State<Shared>, Body(input): Body<TokenRequest>) -> AppResult<Response> {
    let accounts = local(&state)?;
    let account = accounts.ensure(&input.id, input.custodian.as_deref())?;
    let ttl = input.ttl_s.unwrap_or(1800).clamp(-3600, 86_400);
    let token = accounts.mint(&account, ttl);
    Ok(ok(
        "dev_token",
        serde_json::json!({
            "access_token": token,
            "token_type": "Bearer",
            "expires_in": ttl,
            "account": {
                "uuid": account.uuid,
                "id": account.id,
                "kind": match account.kind { extend_protocol::model::MemberKind::Carbon => "carbon", extend_protocol::model::MemberKind::Silicon => "silicon" },
                "display_name": account.display_name,
                "custodian": account.custodian,
            },
        }),
    ))
}

pub async fn jwks(State(state): State<Shared>) -> AppResult<Response> {
    use axum::response::IntoResponse as _;
    let accounts = local(&state)?;
    Ok(axum::Json(accounts.jwks_value()).into_response())
}

pub async fn tings(State(state): State<Shared>) -> AppResult<Response> {
    let sent = match &state.local_ting {
        Some(t) => t.sent.lock().await.clone(),
        None => {
            return Err(AppError::new(
                ErrorCode::UnknownCommand,
                "This service doesn't use the local Ting stand-in (EXTEND_TING_MODE=local).",
            ));
        }
    };
    Ok(ok("tings", sent))
}

#[derive(Deserialize)]
pub struct MissingType {
    /// The type's event (`device.wake_requested`) or full name.
    event: String,
    #[serde(default = "yes")]
    missing: bool,
}

fn yes() -> bool {
    true
}

/// Injects a missing-type refusal in the local stand-in (or removes it with `missing: false`).
pub async fn ting_missing(State(state): State<Shared>, Body(input): Body<MissingType>) -> AppResult<Response> {
    let Some(ting) = &state.local_ting else {
        return Err(AppError::new(
            ErrorCode::UnknownCommand,
            "This service doesn't use the local Ting stand-in (EXTEND_TING_MODE=local).",
        ));
    };
    let Some(ty) = extend_protocol::ting::find(&input.event) else {
        return Err(AppError::invalid(format!(
            "{} isn't one of Extend's Ting types.",
            input.event
        )));
    };
    ting.set_missing(ty.event, input.missing);
    Ok(no_content())
}
