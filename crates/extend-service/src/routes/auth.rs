//! Login with a short-lived token from Silicon IAM; refresh; logout; who am I.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use extend_protocol::model::{EndReason, LoginInput, LogoutInput, Me, RefreshInput, TestingEnvironment};

use super::{Body, no_content, ok};
use crate::domain;
use crate::error::AppResult;
use crate::state::{Auth, Sel, Shared};

fn key(headers: &HeaderMap) -> String {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

async fn env_view(
    state: &Shared,
    sel: Option<&crate::iam::TestingSelection>,
    world: &crate::db::World,
) -> Option<TestingEnvironment> {
    let s = sel?;
    let count = super::devices::paired_count(&state.pool, world).await.unwrap_or(0);
    Some(TestingEnvironment {
        environment_id: s.environment_id,
        name: s.name.clone(),
        state: "ready".into(),
        paired_devices: count,
        device_limit: extend_protocol::TEST_DEVICE_LIMIT,
    })
}

pub async fn login(
    State(state): State<Shared>,
    Sel { world, sel }: Sel,
    headers: HeaderMap,
    Body(input): Body<LoginInput>,
) -> AppResult<Response> {
    let mut session = state.iam.login(input.slt.trim(), &key(&headers), sel.as_ref()).await?;
    session.testing_environment = env_view(&state, sel.as_ref(), &world).await;
    tracing::info!(member = %session.member.id, test = sel.is_some(), "login");
    Ok(ok("login", session))
}

pub async fn refresh(
    State(state): State<Shared>,
    Sel { world, sel }: Sel,
    headers: HeaderMap,
    Body(input): Body<RefreshInput>,
) -> AppResult<Response> {
    let mut session = state
        .iam
        .refresh(input.refresh_token.trim(), &key(&headers), sel.as_ref())
        .await?;
    session.testing_environment = env_view(&state, sel.as_ref(), &world).await;
    Ok(ok("refresh", session))
}

/// Signs a login out. A Silicon signing out ends its running sessions in this world; a Carbon
/// signing out ends the running sessions of the Silicons they gave access to, through their own
/// pairs only, never another Carbon's (Carbon decision, 2026-09-27). So Extend finds out whose
/// login it is before revoking it: from the token being revoked (a refresh token works on its own,
/// no Authorization header needed), else from the access token in Authorization. If IAM can't
/// say, nothing is revoked and the error says so.
pub async fn logout(
    State(state): State<Shared>,
    Sel { world, sel }: Sel,
    headers: HeaderMap,
    Body(input): Body<LogoutInput>,
) -> AppResult<Response> {
    let token = input.token.trim().to_owned();
    if token.is_empty() {
        return Err(crate::error::AppError::invalid("token is empty.")
            .hint("Send the refresh token (or the access token) to sign out, as {\"type\":\"logout\",\"data\":{\"token\":\"...\"}}."));
    }
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != token)
        .map(str::to_owned);
    let unsure = |e: crate::error::AppError| {
        crate::error::AppError::new(
            e.code(),
            format!(
                "Extend couldn't sign this login out: it couldn't ask Silicon IAM whose login it is ({}), and a Silicon's \
                 running sessions must end with it. Nothing was revoked.",
                e.0.message
            ),
        )
        .hint("Retry `extend logout` in a moment; if it keeps failing, report it with `extend report`.")
    };
    let mut member = state.iam.identify(&token, sel.as_ref()).await.map_err(unsure)?;
    if member.is_none()
        && let Some(b) = &bearer
    {
        member = match state.iam.authorize(b, None, sel.as_ref()).await {
            Ok(p) => Some(p.member),
            Err(e) if crate::iam::refuses_token(&e) => None,
            Err(e) => return Err(unsure(e)),
        };
    }
    state.iam.logout(&token, sel.as_ref()).await?;
    state.auth_cache.forget_token(&token).await;
    if let Some(b) = &bearer {
        state.auth_cache.forget_token(b).await;
    }
    tracing::info!(member = ?member.as_ref().map(|m| m.id.clone()), world = %world.schema, "logout");
    if let Some(m) = member {
        state.auth_cache.forget(std::slice::from_ref(&m.id)).await;
        if m.kind == extend_protocol::model::MemberKind::Silicon {
            let running: Vec<(String,)> = sqlx::query_as(sql!(
                "SELECT session_id FROM {} WHERE silicon_id = $1 AND state <> 'ended'",
                world.t("sessions")
            ))
            .bind(&m.id)
            .fetch_all(&state.pool)
            .await?;
            for (sid,) in running {
                domain::end_session(&state, &world, &sid, EndReason::SiliconLoggedOut, &m).await?;
            }
        } else {
            let ended = domain::end_carbon_side(&state, &world, &m.id, None, EndReason::AccessRemoved, &m).await?;
            if !ended.is_empty() {
                tracing::info!(member = m.id, world = %world.schema, sessions = ?ended, "the Carbon logged out; the sessions of the Silicons they gave access to ended");
            }
        }
    }
    Ok(no_content())
}

pub async fn me(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    let testing_environment = env_view(&state, auth.sel.as_ref(), &auth.world).await;
    Ok(ok(
        "me",
        Me {
            authenticated: true,
            member: auth.p.member.clone(),
            teams: auth.p.teams.clone(),
            team: auth.p.team.clone(),
            team_role: auth.p.role.clone(),
            testing_environment,
        },
    ))
}
