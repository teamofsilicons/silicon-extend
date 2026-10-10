//! Who am I, and signing out. Signing in happens at Silicon Accounts: the CLI uses the device
//! flow (Carbons) or exchanges a Silicon's short-lived token itself as a public client; the
//! website exchanges its authorization code on its own server.

use axum::body::Bytes;
use axum::extract::State;
use axum::response::Response;
use extend_protocol::account::{AccountMe, AccountRef, SignOut};
use extend_protocol::model::{EndReason, MemberKind};

use super::{no_content, ok};
use crate::domain;
use crate::error::{AppError, AppResult};
use crate::state::{Auth, Shared};

/// `GET /api/v2/me`.
pub async fn me(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    let row = state.accounts.directory.get(auth.p.uuid()).await?;
    let me = AccountRef::new(auth.p.uuid(), auth.p.public_id(), auth.p.kind)
        .display_name(row.as_ref().and_then(|r| r.display_name.clone()))
        .pfp_url(row.as_ref().and_then(|r| r.pfp_url.clone()));
    let custodian = match row.as_ref().and_then(|r| r.custodian_uuid.clone()) {
        Some(c) if auth.p.is_silicon() => {
            let shown = state.accounts.directory.public_id(&c).await;
            Some(AccountRef::new(c, shown, MemberKind::Carbon))
        }
        _ => None,
    };
    Ok(ok("me", AccountMe::new(me, custodian)))
}

/// `POST /api/v2/auth/logout`: signs this sign-in out of Extend. Silicon Accounts revokes the
/// refresh token sent (or, without one, the sign-in of the access token used). Then a Silicon's
/// running sessions end; a Carbon's sign-out ends the sessions of the Silicons they gave access
/// to, through their own pairs only, never another Carbon's (Carbon decision, 2026-09-27).
pub async fn logout(State(state): State<Shared>, auth: Auth, body: Bytes) -> AppResult<Response> {
    let input: SignOut = if body.iter().all(u8::is_ascii_whitespace) {
        SignOut::default()
    } else {
        super::parse_envelope(&body)?
    };
    let token = input
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&auth.p.token)
        .to_owned();
    state.accounts.api().revoke_token(&token).await.map_err(|e| {
        AppError::new(
            e.code(),
            format!(
                "Extend couldn't sign this sign-in out in Silicon Accounts ({}), so nothing changed.",
                e.0.message
            ),
        )
        .hint("Retry `extend logout` in a moment; if it keeps failing, report it with `extend report`.")
    })?;
    state.accounts.forget(auth.p.uuid()).await;
    state.proofs.drop_account(auth.p.uuid()).await;
    let ended = if auth.p.is_silicon() {
        domain::end_silicon_sessions(
            &state,
            &auth.world,
            auth.p.uuid(),
            EndReason::SiliconLoggedOut,
            &auth.p.actor(),
        )
        .await?
    } else {
        domain::end_carbon_side(
            &state,
            &auth.world,
            auth.p.uuid(),
            EndReason::AccessRemoved,
            &auth.p.actor(),
        )
        .await?
    };
    tracing::info!(account = auth.p.public_id(), uuid = auth.p.uuid(), sessions = ?ended, "signed out of Extend");
    Ok(no_content())
}
