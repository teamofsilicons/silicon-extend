//! Development-only routes that stand in for Silicon Accounts and Ting when
//! `EXTEND_ACCOUNTS_MODE=local` (and `EXTEND_TING_MODE=local`). Every handler refuses unless the
//! local stand-in is active, which production configuration forbids.
//!
//! `POST /dev/accounts/token {"type":"dev_token","data":{"id":"si:scout","custodian":"c:ada"}}`
//! creates the account if it is new and answers `{access_token, account}`: an EdDSA access token
//! for Extend, signed by the stand-in, verified like a real one.
//!
//! The CLI signs in to the stand-in the way it signs in to Silicon Accounts, with
//! `ACCOUNTS_URL={service}/dev/accounts`: `POST /dev/accounts/slt` (same body) mints a short-lived
//! token, as `silicon-accounts login --app extend -q` does, and `extend login --slt-stdin` exchanges
//! it at `POST /dev/accounts/v1/oauth/token`, which also rotates refresh tokens; `POST
//! /dev/accounts/v1/oauth/revoke` ends a sign-in. Those two answer in Silicon Accounts' OAuth
//! shape, not Extend's envelope.

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::{Form, Json};
use extend_protocol::ErrorCode;
use serde::Deserialize;

use super::{Body, no_content, ok};
use crate::accounts::local::{LocalAccounts, OAuthRefusal, SLT_GRANT, SLT_TTL_S};
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
    let accounts = local(&state)?;
    Ok(Json(accounts.jwks_value()).into_response())
}

#[derive(Deserialize)]
pub struct SltRequest {
    /// `c:…` or `si:…`.
    id: String,
    /// A Silicon's custodian (`c:…`); created too if new.
    #[serde(default)]
    custodian: Option<String>,
}

/// `POST /dev/accounts/slt`: a short-lived token for Extend (works once, for 120 s), for the
/// account named, which is created on first use.
pub async fn slt(State(state): State<Shared>, Body(input): Body<SltRequest>) -> AppResult<Response> {
    let accounts = local(&state)?;
    let account = accounts.ensure(&input.id, input.custodian.as_deref())?;
    Ok(ok(
        "slt",
        serde_json::json!({
            "slt": accounts.mint_slt(&account),
            "expires_in": SLT_TTL_S,
            "account": {"uuid": account.uuid, "id": account.id},
        }),
    ))
}

/// The form fields of the token and revoke endpoints the CLI posts.
#[derive(Deserialize)]
pub struct OAuthForm {
    #[serde(default)]
    grant_type: String,
    #[serde(default)]
    slt: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    client_id: String,
}

/// `POST /dev/accounts/v1/oauth/token` (form): the short-lived token and refresh grants of a
/// public client (`client_id` alone), answered as Silicon Accounts answers them.
pub async fn oauth_token(State(state): State<Shared>, Form(form): Form<OAuthForm>) -> AppResult<Response> {
    let accounts = local(&state)?;
    let answer = match form.grant_type.as_str() {
        SLT_GRANT => match form.slt.as_deref() {
            Some(slt) => accounts.exchange_slt(slt, &form.client_id),
            None => Err(invalid_request("slt is required with the short-lived token grant.")),
        },
        "refresh_token" => match form.refresh_token.as_deref() {
            Some(refresh) => accounts.refresh_grant(refresh, &form.client_id),
            None => Err(invalid_request("refresh_token is required with the refresh grant.")),
        },
        other => Err(OAuthRefusal {
            status: 400,
            error: "unsupported_grant_type",
            description: format!(
                "The local Silicon Accounts stand-in supports the short-lived token and refresh grants, not {other:?}."
            ),
        }),
    };
    Ok(oauth_answer(answer))
}

/// `POST /dev/accounts/v1/oauth/revoke` (form `token`, `client_id`): ends the sign-in behind a
/// refresh or access token. 200 either way, `revoked` says whether the stand-in knew it.
pub async fn oauth_revoke(State(state): State<Shared>, Form(form): Form<OAuthForm>) -> AppResult<Response> {
    let accounts = local(&state)?;
    if form.client_id.trim() != accounts.app_id {
        return Ok(oauth_answer(Err(OAuthRefusal {
            status: 401,
            error: "invalid_client",
            description: format!("client_id {:?} isn't {}.", form.client_id, accounts.app_id),
        })));
    }
    let revoked = form.token.as_deref().is_some_and(|t| accounts.revoke_sign_in(t));
    Ok(oauth_answer(Ok(serde_json::json!({"revoked": revoked}))))
}

fn invalid_request(description: &str) -> OAuthRefusal {
    OAuthRefusal {
        status: 400,
        error: "invalid_request",
        description: description.to_owned(),
    }
}

fn oauth_answer(answer: Result<serde_json::Value, OAuthRefusal>) -> Response {
    let mut resp = match answer {
        Ok(v) => Json(v).into_response(),
        Err(r) => (
            StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_REQUEST),
            Json(serde_json::json!({"error": r.error, "error_description": r.description})),
        )
            .into_response(),
    };
    resp.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    resp
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
