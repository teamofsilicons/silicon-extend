//! The selected test environment, and Honeycomb's lifecycle instructions (TECHNICAL.md section 8).
//!
//! Honeycomb sends one instruction per operation. Extend follows the same participant contract as
//! the other services:
//! - Operations on one environment run one at a time (a transaction-scoped advisory lock).
//! - Each operation gets a durable receipt before any effect: `pending`, then `completed` or
//!   `failed`. The identical instruction returns the stored receipt (or finishes a `pending` or
//!   `failed` one); a different instruction under the same `operation_id` is `409`.
//! - An operation must carry an `environment_revision` newer than or equal to the last one applied;
//!   an older one is stale. Only the latest operation can be retried: one superseded by a later
//!   operation never runs. Only `clean` advances `generation` (a new clean must), and only
//!   `rotate-key` advances `key_version` (a new rotate-key must).
//! - `prepare` and `restore` leave the environment `preparing`: test access opens once Honeycomb
//!   confirms every service is ready (see crate::state).
//! - `clean` and `rotate-key` keep the state the environment had (a disabled one stays disabled).
//!   A removed environment accepts nothing but `purge` — not even the retry of an operation
//!   accepted before it was removed.
//! - At most 10 environments are active (`preparing`, `ready`, `cleaning`); every transition into
//!   an active state takes one of them under a lock.

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

/// Every action Extend accepts. `activate` is Honeycomb's readiness confirmation (the phase it
/// runs with IAM once every participant is ready); the import actions concern IAM's application
/// records, so Extend only acknowledges them.
const ACTIONS: &[&str] = &[
    "prepare",
    "activate",
    "rotate-key",
    "clean",
    "disable",
    "restore",
    "purge",
    "import",
    "refresh-import",
    "retire-applications",
];

/// The states that hold one of the [`TEST_ENVIRONMENT_LIMIT`] slots.
const ACTIVE_STATES: &[&str] = &["preparing", "ready", "cleaning"];

fn service_auth(state: &Shared, headers: &HeaderMap) -> AppResult<()> {
    let expected = state.cfg.honeycomb_service_token.as_deref().ok_or_else(|| {
        AppError::new(
            ErrorCode::Unauthorized,
            "Extend has no Honeycomb service credential configured (EXTEND_HONEYCOMB_SERVICE_TOKEN), so it accepts no lifecycle instructions.",
        )
        .hint("Set EXTEND_HONEYCOMB_SERVICE_TOKEN to the credential Honeycomb uses for Extend, then restart the service.")
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
        Err(
            AppError::new(ErrorCode::Unauthorized, "The Honeycomb service credential is wrong.")
                .hint("Use the credential configured as EXTEND_HONEYCOMB_SERVICE_TOKEN on the Extend service."),
        )
    }
}

fn make_receipt(op: &Operation, state_word: &str, target: &str, error: Option<&str>) -> serde_json::Value {
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
        "error": error,
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
    let (_, r) = receipt_get(&state, env, op).await?.ok_or_else(|| {
        AppError::new(
            ErrorCode::RequestNotFound,
            format!("Extend has no receipt for operation {op} on test environment {env}."),
        )
        .hint(
            "Extend records a receipt once it accepts an instruction; one refused as invalid, stale or incompatible \
             has none. Send the instruction with PUT to this path.",
        )
    })?;
    Ok((StatusCode::OK, axum::Json(r)).into_response())
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EnvRow {
    org_id: String,
    name: String,
    state: String,
    environment_revision: i64,
    generation: i64,
    key_version: i64,
    last_operation_id: Option<Uuid>,
}

fn conflict(message: impl Into<String>, hint: &str) -> AppError {
    AppError::new(ErrorCode::Conflict, message).hint(hint)
}

const STALE_HINT: &str = "Honeycomb sends the environment's current revision; re-read it and send a new operation.";

/// Refuses an instruction that doesn't follow the environment's history (see the module docs).
/// `retry` is true when this identical instruction was accepted before and has not completed.
fn check_transition(row: Option<&EnvRow>, op: &Operation, retry: bool) -> AppResult<()> {
    let Some(row) = row else {
        // Like the other participants, an import may be the first instruction Extend sees.
        return if matches!(op.action.as_str(), "prepare" | "import" | "purge") {
            Ok(())
        } else {
            Err(conflict(
                format!(
                    "Honeycomb has not prepared test environment {} in Extend, so {} has nothing to act on.",
                    op.environment_id, op.action
                ),
                "Send prepare for this environment first.",
            ))
        };
    };
    if row.org_id != op.org_id {
        return Err(conflict(
            format!(
                "Test environment {} belongs to team {:?}, not {:?}.",
                op.environment_id, row.org_id, op.org_id
            ),
            "Send the instruction for the team that owns the environment.",
        ));
    }
    // A removed environment accepts nothing but purge (which only finishes removing it) — not even
    // the retry of an instruction accepted before the purge, which would bring it back.
    if row.state == "removed" {
        return if op.action == "purge" {
            Ok(())
        } else {
            Err(conflict(
                format!(
                    "Test environment {} was permanently removed; it can't be {}.",
                    row.name,
                    past(&op.action)
                ),
                "Create a new test environment in Honeycomb; a removed one never comes back.",
            ))
        };
    }
    // Only the latest operation may be retried: once a later one was applied, an earlier one is
    // superseded and never runs (Honeycomb itself never resumes a superseded operation).
    let latest = row.last_operation_id == Some(op.operation_id);
    if retry && !latest {
        return Err(conflict(
            format!(
                "Operation {} on test environment {} was superseded by a later operation, so it won't run now.",
                op.operation_id, row.name
            ),
            STALE_HINT,
        ));
    }
    let same_op = latest;
    if op.environment_revision < row.environment_revision {
        return Err(conflict(
            format!(
                "Stale instruction: revision {} is older than test environment {}'s revision {}.",
                op.environment_revision, row.name, row.environment_revision
            ),
            STALE_HINT,
        ));
    }
    if op.generation < row.generation || op.key_version < row.key_version {
        return Err(conflict(
            format!(
                "Stale instruction: generation {} / key version {} is older than test environment {}'s {} / {}.",
                op.generation, op.key_version, row.name, row.generation, row.key_version
            ),
            STALE_HINT,
        ));
    }
    if op.generation > row.generation && op.action != "clean" {
        return Err(conflict(
            format!(
                "Only a clean advances the generation; {} can't move it from {} to {}.",
                op.action, row.generation, op.generation
            ),
            "Send the environment's current generation.",
        ));
    }
    if op.action == "clean" && op.generation == row.generation && !same_op {
        return Err(conflict(
            format!(
                "A clean must advance the generation; test environment {} is already at {}.",
                row.name, row.generation
            ),
            "Send the next generation with the clean.",
        ));
    }
    if op.key_version > row.key_version && op.action != "rotate-key" {
        return Err(conflict(
            format!(
                "Only rotate-key advances the key version; {} can't move it from {} to {}.",
                op.action, row.key_version, op.key_version
            ),
            "Send the environment's current key version.",
        ));
    }
    if op.action == "rotate-key" && op.key_version == row.key_version && !same_op {
        return Err(conflict(
            format!(
                "rotate-key must advance the key version; test environment {} is already at {}.",
                row.name, row.key_version
            ),
            "Send the next key version with rotate-key.",
        ));
    }
    if row.state == "cleaning" && !same_op && !matches!(op.action.as_str(), "clean" | "purge") {
        return Err(conflict(
            format!(
                "A clean of test environment {} has not finished, so {} can't run yet.",
                row.name, op.action
            ),
            "Retry the pending clean (the identical instruction) first, then send this one.",
        ));
    }
    let disabled = row.state == "disabled";
    match op.action.as_str() {
        "prepare" if disabled => Err(conflict(
            format!(
                "Test environment {} is disabled; preparing it again would reopen it without a free slot.",
                row.name
            ),
            "Send restore instead: it reopens the environment when one of the 10 active slots is free.",
        )),
        "activate" if disabled => Err(conflict(
            format!(
                "Test environment {} is disabled, so there is nothing to activate.",
                row.name
            ),
            "Send restore first; activate once Honeycomb confirms every service is ready again.",
        )),
        _ => Ok(()),
    }
}

fn past(action: &str) -> &str {
    match action {
        "prepare" => "prepared again",
        "activate" => "activated",
        "restore" => "restored",
        "clean" => "cleaned",
        "disable" => "disabled",
        "rotate-key" => "given a new key",
        _ => "changed",
    }
}

/// The state the environment is in once the action completes.
fn target_state(row: Option<&EnvRow>, op: &Operation, pending_target: Option<&str>) -> String {
    // A retry finishes what the pending receipt promised (so a clean interrupted while a disabled
    // environment was `cleaning` still ends disabled).
    if let Some(t) = pending_target {
        return t.to_owned();
    }
    let current = row.map_or("preparing", |r| r.state.as_str());
    match op.action.as_str() {
        "prepare" => "preparing",
        "activate" => "ready",
        "disable" => "disabled",
        "retire-applications" if retires_extend(op) => "disabled",
        "purge" => "removed",
        "restore" => {
            if current == "disabled" {
                "preparing"
            } else {
                current
            }
        }
        _ => current,
    }
    .to_owned()
}

pub async fn apply(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path((org, env, op_id)): Path<(String, Uuid, Uuid)>,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    service_auth(&state, &headers)?;
    let op: Operation = serde_json::from_slice(&body).map_err(|e| {
        AppError::invalid(format!("Invalid lifecycle instruction: {e}"))
            .hint("Send Honeycomb's participant instruction as plain JSON (see HoneycombOperation in api.yaml).")
    })?;
    if op.environment_id != env || op.operation_id != op_id || op.org_id != org {
        return Err(AppError::invalid(
            "The path and the body name different environments, operations or teams.",
        ));
    }
    validate(&state, &op)?;
    let hash = extend_protocol::ids::hex_lower(&Sha256::digest(&body));

    // One operation at a time per environment, until this transaction ends.
    let mut serial = state.pool.begin().await?;
    db::lock_environment(&mut serial, env).await?;

    let stored = receipt_get(&state, env, op_id).await?;
    if let Some((h, r)) = &stored {
        if *h != hash {
            return Err(conflict(
                format!("Operation {op_id} was already used with a different instruction."),
                "Reuse an operation id only to retry the identical instruction; send a new operation for a new one.",
            ));
        }
        if r["state"] == "completed" {
            return Ok((StatusCode::OK, axum::Json(r.clone())).into_response());
        }
    }
    let retry = stored.is_some();
    let row: Option<EnvRow> = sqlx::query_as(
        "SELECT org_id, name, state, environment_revision, generation, key_version, last_operation_id
         FROM extend_global.test_environments WHERE environment_id = $1",
    )
    .bind(env)
    .fetch_optional(&state.pool)
    .await?;
    check_transition(row.as_ref(), &op, retry)?;
    let pending_target = stored
        .as_ref()
        .and_then(|(_, r)| r["target_state"].as_str().map(str::to_owned));
    let target = target_state(row.as_ref(), &op, pending_target.as_deref());

    // The pending receipt and the environment's new revision become durable together, before any
    // effect: a crash leaves a pending receipt to retry, and older instructions are fenced out.
    let pending = make_receipt(&op, "pending", &target, None);
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO extend_global.honeycomb_operations (environment_id, operation_id, request_hash, receipt) VALUES ($1, $2, $3, $4)
         ON CONFLICT (environment_id, operation_id) DO UPDATE SET receipt = EXCLUDED.receipt, updated_at = now()",
    )
    .bind(env)
    .bind(op_id)
    .bind(&hash)
    .bind(&pending)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE extend_global.test_environments SET environment_revision = $2, generation = $3, key_version = $4, last_operation_id = $5
         WHERE environment_id = $1 AND state <> 'removed'",
    )
    .bind(env)
    .bind(op.environment_revision)
    .bind(op.generation)
    .bind(op.key_version)
    .bind(op_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let outcome = execute(&state, &op, row.as_ref(), &target).await;
    let (receipt, result) = match outcome {
        Ok(()) => {
            let r = make_receipt(&op, "completed", &target, None);
            (r.clone(), Ok((StatusCode::OK, axum::Json(r)).into_response()))
        }
        Err(e) => {
            let why = format!("{}: {}", e.0.code.as_str(), e.0.message);
            (make_receipt(&op, "failed", &target, Some(&why)), Err(e))
        }
    };
    sqlx::query(
        "UPDATE extend_global.honeycomb_operations SET receipt = $3, updated_at = now() WHERE environment_id = $1 AND operation_id = $2",
    )
    .bind(env)
    .bind(op_id)
    .bind(&receipt)
    .execute(&state.pool)
    .await?;
    serial.commit().await?;
    state.forget_selections(env).await;
    tracing::info!(environment_id = %env, action = op.action, state = %receipt["state"], target_state = %target, "Honeycomb lifecycle instruction");
    result
}

fn validate(state: &Shared, op: &Operation) -> AppResult<()> {
    if op.testing_key.len() != 32 || !op.testing_key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AppError::invalid("testing_key must be 32 alphanumeric characters."));
    }
    if op.app_id != state.iam.app_id() {
        return Err(AppError::invalid(format!(
            "This instruction is for application {:?}, not Extend ({:?}).",
            op.app_id,
            state.iam.app_id()
        ))
        .hint("Send it to the service that application belongs to."));
    }
    if op.environment_revision < 1 || op.generation < 1 || op.key_version < 1 {
        return Err(AppError::invalid(
            "environment_revision, generation and key_version must each be 1 or more.",
        ));
    }
    if op.org_id.is_empty() || op.org_id.len() > 128 {
        return Err(AppError::invalid("org_id must be 1–128 characters."));
    }
    if op
        .name
        .as_ref()
        .is_some_and(|n| n.trim().is_empty() || n.chars().count() > 128)
    {
        return Err(AppError::invalid("name must be 1–128 characters when present."));
    }
    if !ACTIONS.contains(&op.action.as_str()) {
        return Err(AppError::invalid(format!("Unknown lifecycle action {:?}.", op.action))
            .hint(format!("Extend accepts {}.", ACTIONS.join(", "))));
    }
    Ok(())
}

async fn set_state(state: &Shared, env: Uuid, to: &str) -> AppResult<()> {
    sqlx::query("UPDATE extend_global.test_environments SET state = $2 WHERE environment_id = $1")
        .bind(env)
        .bind(to)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Takes one of the 10 active slots for `env` and moves it to `to`, under the slot lock.
async fn take_slot(state: &Shared, op: &Operation, to: &str, insert: bool) -> AppResult<()> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(db::TEST_SLOT_LOCK)
        .execute(&mut *tx)
        .await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM extend_global.test_environments
         WHERE state = ANY($2) AND environment_id <> $1",
    )
    .bind(op.environment_id)
    .bind(ACTIVE_STATES)
    .fetch_one(&mut *tx)
    .await?;
    if active >= TEST_ENVIRONMENT_LIMIT {
        return Err(AppError::new(
            ErrorCode::TestEnvironmentLimit,
            format!(
                "All {TEST_ENVIRONMENT_LIMIT} test environment slots are in use across Silicon Extend, so test environment {} can't be {}.",
                op.name.clone().unwrap_or_else(|| op.environment_id.to_string()),
                if op.action == "restore" { "restored" } else { "prepared" }
            ),
        )
        .hint(
            "Free a slot first: disable or permanently remove a test environment nobody uses (in Honeycomb), \
             then retry this identical instruction. Disabled and removed environments don't count.",
        ));
    }
    if insert {
        sqlx::query(
            "INSERT INTO extend_global.test_environments
               (environment_id, org_id, app_id, name, state, environment_revision, generation, key_version, last_operation_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (environment_id) DO UPDATE SET name = EXCLUDED.name, state = EXCLUDED.state, retired_at = NULL",
        )
        .bind(op.environment_id)
        .bind(&op.org_id)
        .bind(&op.app_id)
        .bind(op.name.clone().unwrap_or_else(|| op.environment_id.to_string()))
        .bind(to)
        .bind(op.environment_revision)
        .bind(op.generation)
        .bind(op.key_version)
        .bind(op.operation_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE extend_global.test_environments SET state = $2, retired_at = NULL, name = COALESCE($3, name) WHERE environment_id = $1",
        )
        .bind(op.environment_id)
        .bind(to)
        .bind(&op.name)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn execute(state: &Shared, op: &Operation, row: Option<&EnvRow>, target: &str) -> AppResult<()> {
    let env = op.environment_id;
    let world = World::test(env);
    let current = row.map(|r| r.state.as_str());
    match op.action.as_str() {
        "prepare" | "import" if op.action == "prepare" || current.is_none() => {
            // Extend's own preparation; test access waits for Honeycomb's readiness confirmation.
            match current {
                None => take_slot(state, op, "preparing", true).await?,
                // Only an environment that already holds a slot is prepared again without taking
                // one (check_transition refuses the others; this keeps the limit if that changes).
                Some(s) if !ACTIVE_STATES.contains(&s) => take_slot(state, op, "preparing", false).await?,
                Some(_) => {
                    sqlx::query(
                        "UPDATE extend_global.test_environments SET state = 'preparing', name = COALESCE($2, name) WHERE environment_id = $1",
                    )
                    .bind(env)
                    .bind(&op.name)
                    .execute(&state.pool)
                    .await?;
                }
            }
            db::ensure_world(&state.pool, &world).await?;
            state.ready_worlds.write().await.insert(world.schema.clone());
        }
        "activate" => {
            if current == Some("preparing") {
                set_state(state, env, "ready").await?;
            }
        }
        "restore" => {
            if current == Some("disabled") {
                take_slot(state, op, "preparing", false).await?;
            }
            db::ensure_world(&state.pool, &world).await?;
        }
        "retire-applications" if retires_extend(op) && current != Some("removed") => {
            // Extend leaves this environment: its data goes and access closes, like a clean
            // followed by a disable (Honeycomb can still restore or purge it).
            wipe(state, &world, current, target).await?;
        }
        "rotate-key" | "import" | "refresh-import" | "retire-applications" => {}
        "disable" => {
            set_state(state, env, "disabled").await?;
            state.forget_selections(env).await;
            close_world(state, &world, EndReason::EnvironmentDisabled).await?;
        }
        "clean" => {
            if current != Some("removed") {
                wipe(state, &world, current, target).await?;
            }
        }
        "purge" => {
            match current {
                None => {
                    // Nothing was ever prepared here; remember the id so nothing recreates it.
                    sqlx::query(
                        "INSERT INTO extend_global.test_environments
                           (environment_id, org_id, app_id, name, state, environment_revision, generation, key_version, last_operation_id, retired_at)
                         VALUES ($1, $2, $3, $4, 'removed', $5, $6, $7, $8, now()) ON CONFLICT (environment_id) DO NOTHING",
                    )
                    .bind(env)
                    .bind(&op.org_id)
                    .bind(&op.app_id)
                    .bind(op.name.clone().unwrap_or_else(|| env.to_string()))
                    .bind(op.environment_revision)
                    .bind(op.generation)
                    .bind(op.key_version)
                    .bind(op.operation_id)
                    .execute(&state.pool)
                    .await?;
                }
                Some(_) => {
                    sqlx::query(
                        "UPDATE extend_global.test_environments SET state = 'removed', retired_at = COALESCE(retired_at, now()),
                           webhook_key_digest = NULL WHERE environment_id = $1",
                    )
                    .bind(env)
                    .execute(&state.pool)
                    .await?;
                    state.forget_selections(env).await;
                    if schema_exists(state, &world).await? {
                        end_everything(state, &world, EndReason::EnvironmentDisabled, true).await?;
                    }
                    let fence = state.fence_exclusive(env).await;
                    forget_enrollments(state, &world).await?;
                    db::drop_world(&state.pool, &world).await?;
                    sqlx::query("DELETE FROM extend_global.test_secret_bindings WHERE environment_id = $1")
                        .bind(env)
                        .execute(&state.pool)
                        .await?;
                    state.ready_worlds.write().await.remove(&world.schema);
                    drop(fence);
                }
            }
        }
        other => return Err(AppError::invalid(format!("Unknown lifecycle action {other:?}."))),
    }
    Ok(())
}

fn retires_extend(op: &Operation) -> bool {
    op.retired_apps.as_ref().is_some_and(|apps| apps.contains(&op.app_id))
}

/// Clears a test world (a clean): blocks access, ends sessions, unpairs every device, waits for
/// work admitted before it, truncates, and leaves the environment in `target`.
async fn wipe(state: &Shared, world: &World, current: Option<&str>, target: &str) -> AppResult<()> {
    let env = world
        .environment_id
        .ok_or_else(|| AppError::internal("wipe on production"))?;
    // A disabled environment is closed already, and stays disabled (and out of the active count)
    // through the clean.
    if current != Some("disabled") {
        set_state(state, env, "cleaning").await?;
    }
    state.forget_selections(env).await;
    end_everything(state, world, EndReason::EnvironmentCleaned, true).await?;
    forget_enrollments(state, world).await?;
    // Wait for anything admitted before the clean; nothing new gets in meanwhile.
    let fence = state.fence_exclusive(env).await;
    end_everything(state, world, EndReason::EnvironmentCleaned, true).await?;
    forget_enrollments(state, world).await?;
    db::truncate_world(&state.pool, world).await?;
    set_state(state, env, target).await?;
    drop(fence);
    Ok(())
}

async fn schema_exists(state: &Shared, world: &World) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
            .bind(&world.schema)
            .fetch_one(&state.pool)
            .await?,
    )
}

/// Pairing codes waiting in this world die with its data.
async fn forget_enrollments(state: &Shared, world: &World) -> AppResult<()> {
    sqlx::query("DELETE FROM extend_global.enrollments WHERE world_schema = $1")
        .bind(&world.schema)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Ends every running session and, for a clean or purge, unpairs every device.
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
    if unpair {
        let devices: Vec<(String,)> = sqlx::query_as(sql!(
            "SELECT device_id FROM {} WHERE removed_at IS NULL AND host_device_id IS NULL",
            world.t("devices")
        ))
        .fetch_all(&state.pool)
        .await?;
        for (id,) in devices {
            domain::unpair(state, world, &id, reason, &actor).await?;
        }
    }
    Ok(())
}

/// Disabling: every running session ends and every device socket closes with a temporary code,
/// but no pair ends. Sessions end again once in-flight requests have drained, so none started by
/// a request admitted just before the disable survives it.
async fn close_world(state: &Shared, world: &World, reason: EndReason) -> AppResult<()> {
    let env = world
        .environment_id
        .ok_or_else(|| AppError::internal("close_world on production"))?;
    end_everything(state, world, reason, false).await?;
    disconnect_devices(state, world).await?;
    let fence = state.fence_exclusive(env).await;
    end_everything(state, world, reason, false).await?;
    disconnect_devices(state, world).await?;
    drop(fence);
    Ok(())
}

/// Closes every device socket of the world. Each socket sees its channel close, finds the
/// environment closed, and closes with `close::ENVIRONMENT_UNAVAILABLE` (routes/device_app.rs).
/// Commands waiting on a device already failed when their session ended.
async fn disconnect_devices(state: &Shared, world: &World) -> AppResult<()> {
    let devices: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT device_id FROM {} WHERE removed_at IS NULL AND host_device_id IS NULL",
        world.t("devices")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (id,) in devices {
        state.hub.disconnect(&(world.schema.clone(), id)).await;
    }
    Ok(())
}
