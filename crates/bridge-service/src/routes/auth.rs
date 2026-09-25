//! Login with a short-lived token from Silicon IAM; refresh; logout; who am I.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bridge_protocol::model::{EndReason, LoginInput, LogoutInput, Me, RefreshInput, TestingEnvironment};

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

async fn env_view(state: &Shared, sel: Option<&crate::iam::TestingSelection>, world: &crate::db::World) -> Option<TestingEnvironment> {
    let s = sel?;
    let count: i64 = sqlx::query_scalar(sql!("SELECT count(*) FROM {} WHERE removed_at IS NULL", world.t("devices")))
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    Some(TestingEnvironment {
        environment_id: s.environment_id,
        name: s.name.clone(),
        state: "ready".into(),
        paired_devices: count,
        device_limit: bridge_protocol::TEST_DEVICE_LIMIT,
    })
}

pub async fn login(State(state): State<Shared>, Sel { world, sel }: Sel, headers: HeaderMap, Body(input): Body<LoginInput>) -> AppResult<Response> {
    let mut session = state.iam.login(input.slt.trim(), &key(&headers), sel.as_ref()).await?;
    session.testing_environment = env_view(&state, sel.as_ref(), &world).await;
    tracing::info!(member = %session.member.id, test = sel.is_some(), "login");
    Ok(ok("login", session))
}

pub async fn refresh(State(state): State<Shared>, Sel { world, sel }: Sel, headers: HeaderMap, Body(input): Body<RefreshInput>) -> AppResult<Response> {
    let mut session = state.iam.refresh(input.refresh_token.trim(), &key(&headers), sel.as_ref()).await?;
    session.testing_environment = env_view(&state, sel.as_ref(), &world).await;
    Ok(ok("refresh", session))
}

pub async fn logout(State(state): State<Shared>, Sel { world, sel }: Sel, headers: HeaderMap, Body(input): Body<LogoutInput>) -> AppResult<Response> {
    let token = input.token.trim().to_owned();
    // A Silicon signing out ends its running sessions. Find who it is before the token dies.
    let who = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let mut member = None;
    for t in [who.as_deref(), Some(token.as_str())].into_iter().flatten() {
        if let Ok(p) = state.iam.authorize(t, None, sel.as_ref()).await {
            member = Some(p.member);
            break;
        }
    }
    state.iam.logout(&token, sel.as_ref()).await?;
    state.auth_cache.forget_token(&token).await;
    if let Some(m) = member {
        state.auth_cache.forget(std::slice::from_ref(&m.id)).await;
        if m.kind == bridge_protocol::model::MemberKind::Silicon {
            let running: Vec<(String,)> =
                sqlx::query_as(sql!("SELECT session_id FROM {} WHERE silicon_id = $1 AND state <> 'ended'", world.t("sessions")))
                    .bind(&m.id)
                    .fetch_all(&state.pool)
                    .await?;
            for (sid,) in running {
                domain::end_session(&state, &world, &sid, EndReason::SiliconLoggedOut, &m).await?;
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
