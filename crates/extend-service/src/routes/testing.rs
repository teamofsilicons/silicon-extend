//! The selected test environment, and Honeycomb's lifecycle instructions (TECHNICAL.md section 8).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use extend_protocol::model::EndReason;
use extend_protocol::{ErrorCode, TEST_ENVIRONMENT_LIMIT};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use super::devices::env_view;
use super::ok;
use crate::db::{self, World};
use crate::domain;
use crate::error::{AppError, AppResult};
use crate::state::{Sel, Shared};

pub async fn current(State(state): State<Shared>, sel: Sel) -> AppResult<Response> {
    let s = sel.sel.as_ref().ok_or_else(|| {
        AppError::new(ErrorCode::TestOnly, "No test environment is selected.")
            .hint("Send the test application's secret in X-Testing-Application-Secret.")
    })?;
    Ok(ok("testing_environment", env_view(&state, Some(s), &sel.world).await))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    operation_id: Uuid,
    environment_id: Uuid,
    org_id: String,
    app_id: String,
    environment_revision: i64,
    generation: i64,
    key_version: i64,
    action: String,
    testing_key: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    snapshot: Option<serde_json::Value>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    retired_apps: Option<Vec<String>>,
}

fn service_auth(state: &Shared, headers: &HeaderMap) -> AppResult<()> {
    let expected = state.cfg.honeycomb_service_token.as_deref().ok_or_else(|| {
        AppError::new(
            ErrorCode::Unauthorized,
            "Extend has no Honeycomb service credential configured (EXTEND_HONEYCOMB_SERVICE_TOKEN).",
        )
    })?;
    let presented = headers
        .get_all("authorization")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.strip_prefix("Bearer "))
        .collect::<Vec<_>>();
    let [token] = presented.as_slice() else {
        return Err(AppError::new(
            ErrorCode::Unauthorized,
            "Send exactly one Authorization: Bearer <Honeycomb service credential>.",
        ));
    };
    let a = Sha256::digest(token.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    if bool::from(a.ct_eq(&b)) {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::Unauthorized,
            "The Honeycomb service credential is wrong.",
        ))
    }
}

fn make_receipt(op: &Operation, state_word: &str, target: &str) -> serde_json::Value {
    serde_json::json!({
        "operation_id": op.operation_id,
        "environment_id": op.environment_id,
        "app_id": op.app_id,
        "environment_revision": op.environment_revision,
        "generation": op.generation,
        "key_version": op.key_version,
        "state": state_word,
        "target_state": target,
        "retired_apps": op.retired_apps.clone().unwrap_or_default(),
    })
}

pub async fn receipt_get(state: &Shared, env: Uuid, op: Uuid) -> AppResult<Option<(String, serde_json::Value)>> {
    Ok(sqlx::query_as("SELECT request_hash, receipt FROM extend_global.honeycomb_operations WHERE environment_id = $1 AND operation_id = $2")
        .bind(env)
        .bind(op)
        .fetch_optional(&state.pool)
        .await?)
}

pub async fn receipt(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((_org, env, op)): Path<(String, Uuid, Uuid)>,
) -> AppResult<Response> {
    service_auth(&state, &headers)?;
    let (_, r) = receipt_get(&state, env, op)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::RequestNotFound, "Unknown operation."))?;
    Ok((StatusCode::OK, axum::Json(r)).into_response())
}

async fn active_count(state: &Shared, except: Uuid) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM extend_global.test_environments WHERE state IN ('preparing','ready','cleaning') AND environment_id <> $1")
        .bind(except)
        .fetch_one(&state.pool)
        .await?)
}

async fn end_everything(state: &Shared, world: &World, reason: EndReason, unpair: bool) -> AppResult<()> {
    let actor = domain::system_member();
    let sessions: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT session_id FROM {} WHERE state <> 'ended'",
        world.t("sessions")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (sid,) in sessions {
        domain::end_session(state, world, &sid, reason, &actor).await?;
    }
    let devices: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT device_id FROM {} WHERE removed_at IS NULL AND host_device_id IS NULL",
        world.t("devices")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (id,) in devices {
        if unpair {
            domain::unpair(state, world, &id, reason, &actor).await?;
        } else {
            state.hub.disconnect(&(world.schema.clone(), id)).await;
        }
    }
    Ok(())
}

pub async fn apply(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((org, env, op_id)): Path<(String, Uuid, Uuid)>,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    service_auth(&state, &headers)?;
    let op: Operation =
        serde_json::from_slice(&body).map_err(|e| AppError::invalid(format!("Invalid lifecycle instruction: {e}")))?;
    if op.environment_id != env || op.operation_id != op_id || op.org_id != org {
        return Err(AppError::invalid(
            "The path and the body name different environments, operations or teams.",
        ));
    }
    if op.testing_key.len() != 32 || !op.testing_key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AppError::invalid("testing_key must be 32 alphanumeric characters."));
    }
    let hash = extend_protocol::ids::hex_lower(&Sha256::digest(&body));
    if let Some((h, r)) = receipt_get(&state, env, op_id).await? {
        if h != hash {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "This operation id was already used with a different instruction.",
            ));
        }
        return Ok((StatusCode::OK, axum::Json(r)).into_response());
    }
    let existing: Option<(String, i64, i64)> = sqlx::query_as(
        "SELECT state, environment_revision, generation FROM extend_global.test_environments WHERE environment_id = $1",
    )
    .bind(env)
    .fetch_optional(&state.pool)
    .await?;
    if let Some((_, rev, generation)) = &existing
        && (op.environment_revision < *rev || op.generation < *generation)
    {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "Stale instruction: revision {} / generation {} is older than the environment's {rev} / {generation}.",
                op.environment_revision, op.generation
            ),
        ));
    }
    let world = World::test(env);
    let target = match op.action.as_str() {
        "prepare" => {
            if existing.as_ref().is_none_or(|(s, _, _)| s == "removed")
                && active_count(&state, env).await? >= TEST_ENVIRONMENT_LIMIT
            {
                return Err(AppError::new(
                    ErrorCode::TestEnvironmentLimit,
                    "All 10 test environment slots are in use across Silicon Extend.",
                ));
            }
            db::ensure_world(&state.pool, &world).await?;
            sqlx::query(
                "INSERT INTO extend_global.test_environments (environment_id, org_id, app_id, name, state, environment_revision, generation, key_version)
                 VALUES ($1, $2, $3, $4, 'ready', $5, $6, $7)
                 ON CONFLICT (environment_id) DO UPDATE SET name = EXCLUDED.name, state = 'ready', environment_revision = EXCLUDED.environment_revision,
                   generation = EXCLUDED.generation, key_version = EXCLUDED.key_version, retired_at = NULL",
            )
            .bind(env)
            .bind(&op.org_id)
            .bind(&op.app_id)
            .bind(op.name.clone().unwrap_or_else(|| env.to_string()))
            .bind(op.environment_revision)
            .bind(op.generation)
            .bind(op.key_version)
            .execute(&state.pool)
            .await?;
            state.ready_worlds.write().await.insert(world.schema.clone());
            "ready"
        }
        "rotate-key" => {
            sqlx::query("UPDATE extend_global.test_environments SET key_version = $2, environment_revision = $3 WHERE environment_id = $1")
                .bind(env)
                .bind(op.key_version)
                .bind(op.environment_revision)
                .execute(&state.pool)
                .await?;
            "ready"
        }
        "clean" => {
            sqlx::query("UPDATE extend_global.test_environments SET state = 'cleaning' WHERE environment_id = $1")
                .bind(env)
                .execute(&state.pool)
                .await?;
            end_everything(&state, &world, EndReason::EnvironmentCleaned, true).await?;
            db::truncate_world(&state.pool, &world).await?;
            sqlx::query("UPDATE extend_global.test_environments SET state = 'ready', generation = $2, environment_revision = $3 WHERE environment_id = $1")
                .bind(env)
                .bind(op.generation)
                .bind(op.environment_revision)
                .execute(&state.pool)
                .await?;
            state.selections.write().await.clear();
            "ready"
        }
        "disable" => {
            sqlx::query("UPDATE extend_global.test_environments SET state = 'disabled' WHERE environment_id = $1")
                .bind(env)
                .execute(&state.pool)
                .await?;
            state.selections.write().await.clear();
            end_everything(&state, &world, EndReason::EnvironmentDisabled, false).await?;
            "disabled"
        }
        "restore" => {
            if active_count(&state, env).await? >= TEST_ENVIRONMENT_LIMIT {
                return Err(AppError::new(
                    ErrorCode::TestEnvironmentLimit,
                    "Restoring needs a free slot; all 10 test environment slots are in use.",
                ));
            }
            db::ensure_world(&state.pool, &world).await?;
            sqlx::query("UPDATE extend_global.test_environments SET state = 'ready', retired_at = NULL WHERE environment_id = $1").bind(env).execute(&state.pool).await?;
            "ready"
        }
        "purge" => {
            end_everything(&state, &world, EndReason::EnvironmentDisabled, true).await?;
            db::drop_world(&state.pool, &world).await?;
            sqlx::query("UPDATE extend_global.test_environments SET state = 'removed', retired_at = now() WHERE environment_id = $1").bind(env).execute(&state.pool).await?;
            state.ready_worlds.write().await.remove(&world.schema);
            state.selections.write().await.clear();
            "removed"
        }
        "import" | "refresh-import" | "retire-applications" => existing
            .as_ref()
            .map_or("ready", |(s, _, _)| if s == "disabled" { "disabled" } else { "ready" }),
        other => return Err(AppError::invalid(format!("Unknown lifecycle action {other:?}."))),
    };
    let r = make_receipt(&op, "completed", target);
    sqlx::query("INSERT INTO extend_global.honeycomb_operations (environment_id, operation_id, request_hash, receipt) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
        .bind(env)
        .bind(op_id)
        .bind(&hash)
        .bind(&r)
        .execute(&state.pool)
        .await?;
    tracing::info!(environment_id = %env, action = op.action, "Honeycomb lifecycle applied");
    Ok((StatusCode::OK, axum::Json(r)).into_response())
}
