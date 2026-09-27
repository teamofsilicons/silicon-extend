//! Development-only routes that stand in for Silicon IAM and Ting when `EXTEND_IAM_MODE=local`.
//! Every handler refuses unless the local IAM is active, which production configuration forbids.

use std::collections::HashMap;

use axum::Form;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use extend_protocol::{ErrorCode, ids};
use serde::Deserialize;
use uuid::Uuid;

use super::{Body, no_content, ok};
use crate::db::World;
use crate::error::{AppError, AppResult};
use crate::iam::{IamEvent, LocalIam};
use crate::revocation;
use crate::state::{AppState, Shared};

fn local(state: &AppState) -> AppResult<&LocalIam> {
    state.local_iam.as_deref().ok_or_else(|| {
        AppError::new(
            ErrorCode::UnknownCommand,
            "Development routes exist only with EXTEND_IAM_MODE=local.",
        )
    })
}

#[derive(Deserialize)]
pub struct MemberChange {
    id: String,
    /// New teams; `null` removes the member from IAM entirely.
    teams: Option<Vec<String>>,
    /// Revoke every token the member holds (a logout).
    #[serde(default)]
    revoke: bool,
    #[serde(default)]
    environment_id: Option<Uuid>,
}

pub async fn member(State(state): State<Shared>, Body(input): Body<MemberChange>) -> AppResult<Response> {
    let iam = local(&state)?;
    if ids::member_kind(&input.id).is_none() {
        return Err(AppError::invalid("id must be a member id like c:alice or si:chef"));
    }
    let event_type = if input.revoke {
        "session.revoked.v1"
    } else {
        "organization.member.removed.v1"
    };
    iam.set_member(&input.id, input.teams.clone()).await;
    if input.revoke {
        iam.revoke_member(&input.id).await;
    }
    let world = input.environment_id.map_or_else(World::production, World::test);
    let event = IamEvent {
        event_id: Uuid::now_v7().to_string(),
        event_type: event_type.into(),
        members: vec![input.id.clone()],
        teams: input.teams.clone().unwrap_or_default(),
        removed: vec![],
        testing_environment_id: input.environment_id,
    };
    revocation::apply(&state, &world, &event).await?;
    Ok(no_content())
}

#[derive(Deserialize)]
pub struct TestApp {
    secret: String,
    environment_id: Uuid,
}

pub async fn test_app(State(state): State<Shared>, Body(input): Body<TestApp>) -> AppResult<Response> {
    local(&state)?;
    if !ids::is_secret(ids::APP_SECRET_PREFIX, &input.secret) {
        return Err(AppError::invalid(
            "secret must be ask_ followed by 43 base64url characters",
        ));
    }
    sqlx::query(
        "INSERT INTO extend_global.local_test_apps (secret_digest, environment_id) VALUES ($1, $2)
         ON CONFLICT (secret_digest) DO UPDATE SET environment_id = EXCLUDED.environment_id",
    )
    .bind(ids::secret_digest(&input.secret))
    .bind(input.environment_id)
    .execute(&state.pool)
    .await?;
    state.selections.write().await.clear();
    Ok(no_content())
}

/// A stand-in for IAM's consent screen: pick a member, get sent back with a short-lived token.
pub async fn authorize_page(
    State(state): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> AppResult<Response> {
    local(&state)?;
    let hidden: String = q
        .iter()
        .map(|(k, v)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                html_escape(k),
                html_escape(v)
            )
        })
        .collect();
    Ok(Html(format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Local Silicon IAM</title>
<style>body{{font:16px system-ui;max-width:420px;margin:48px auto;padding:0 16px}}input,button{{font:inherit;padding:8px;width:100%;margin:6px 0;box-sizing:border-box}}</style>
<h1>Local Silicon IAM</h1><p>Development only. Sign in to Silicon Extend as:</p>
<form method="post">{hidden}<input name="member" placeholder="c:alice" autofocus required><button>Approve</button></form>"#
    ))
    .into_response())
}

pub async fn authorize_submit(
    State(state): State<Shared>,
    Form(mut form): Form<HashMap<String, String>>,
) -> AppResult<Response> {
    local(&state)?;
    let member = form.remove("member").unwrap_or_default();
    if ids::member_kind(member.trim()).is_none() {
        return Err(AppError::invalid("Enter a member id like c:alice."));
    }
    let redirect = form
        .remove("redirect_uri")
        .or_else(|| form.remove("return_to"))
        .ok_or_else(|| AppError::invalid("redirect_uri is required"))?;
    let mut url = url::Url::parse(&redirect).map_err(|_| AppError::invalid("redirect_uri must be an absolute URL"))?;
    {
        // Like IAM: append `slt` to redirect_uri and keep its existing query.
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("slt", member.trim());
        if let Some(s) = form.get("state") {
            pairs.append_pair("state", s);
        }
    }
    Ok(Redirect::to(url.as_str()).into_response())
}

pub async fn tings(State(state): State<Shared>) -> AppResult<Response> {
    local(&state)?;
    let sent = match &state.local_ting {
        Some(t) => t.sent.lock().await.clone(),
        None => vec![],
    };
    Ok(ok("tings", sent))
}

#[derive(Deserialize)]
pub struct MissingType {
    team: String,
    /// The type's event (`device.wake_requested`) or full name.
    event: String,
    #[serde(default = "yes")]
    missing: bool,
}

fn yes() -> bool {
    true
}

/// Injects a missing-type refusal on a Team's send in the local stand-in (or removes the injection
/// with `missing: false`). Real Ting resolves types app-wide; this limits a test failure's scope.
pub async fn ting_missing(State(state): State<Shared>, Body(input): Body<MissingType>) -> AppResult<Response> {
    local(&state)?;
    let Some(ting) = &state.local_ting else {
        return Err(AppError::new(
            ErrorCode::UnknownCommand,
            "This service sends Tings through the real Ting, not the local stand-in.",
        ));
    };
    let Some(ty) = extend_protocol::ting::find(&input.event) else {
        return Err(AppError::invalid(format!(
            "{} isn't one of Extend's Ting types.",
            input.event
        )));
    };
    ting.set_missing(&input.team, ty.event, input.missing);
    Ok(no_content())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
