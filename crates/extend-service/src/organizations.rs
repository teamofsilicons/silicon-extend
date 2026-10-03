//! Organization bindings for a configured physical pair. Import never duplicates credentials.
use crate::{db::World, domain, error::AppResult, state::AppState};
use extend_protocol::model::Visibility;

pub const MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS {s}.device_organizations (
    device_id text NOT NULL REFERENCES {s}.devices(device_id),
    org_id text NOT NULL,
    visibility text NOT NULL DEFAULT 'personal' CHECK (visibility IN ('personal', 'team')),
    wake_muted boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    removed_at timestamptz,
    PRIMARY KEY (device_id, org_id)
);
CREATE INDEX IF NOT EXISTS device_organizations_org ON {s}.device_organizations (org_id, device_id) WHERE removed_at IS NULL;
-- Existing configurations remain private. Preserve the places with explicit grants, but a
-- private binding takes precedence over a grant until its owner explicitly shares it.
INSERT INTO {s}.device_organizations (device_id, org_id)
SELECT device_id, team FROM {s}.devices WHERE team <> ''
UNION SELECT device_id, team FROM {s}.device_access WHERE team <> ''
ON CONFLICT DO NOTHING;
UPDATE {s}.activity a SET team = COALESCE(
    (SELECT ss.team FROM {s}.sessions ss WHERE ss.session_id = a.session_id),
    (SELECT d.team FROM {s}.devices d WHERE d.device_id = a.device_id)) WHERE a.team IS NULL;
-- Older native agents still enroll physical pairs; create the initial private binding atomically.
CREATE OR REPLACE FUNCTION {s}.device_initial_organization() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    INSERT INTO {s}.device_organizations (device_id, org_id) VALUES (NEW.device_id, NEW.team) ON CONFLICT DO NOTHING;
    RETURN NEW;
END $f$;
CREATE OR REPLACE TRIGGER device_initial_organization AFTER INSERT ON {s}.devices
    FOR EACH ROW EXECUTE FUNCTION {s}.device_initial_organization();
"#;

pub async fn visibility(
    state: &AppState,
    world: &World,
    device: &str,
    org: &str,
    include_removed: bool,
) -> AppResult<Option<Visibility>> {
    let value: Option<String> = sqlx::query_scalar(sql!(
        "SELECT visibility FROM {} WHERE device_id = $1 AND org_id = $2 AND ($3 OR removed_at IS NULL)",
        world.t("device_organizations")
    ))
    .bind(device)
    .bind(org)
    .bind(include_removed)
    .fetch_optional(&state.pool)
    .await?;
    Ok(value.map(|v| {
        if v == "team" {
            Visibility::Team
        } else {
            Visibility::Personal
        }
    }))
}

/// Capture revocation under the instance lock so a later re-share cannot lose new grants.
#[derive(Default)]
pub struct DisabledUse {
    sessions: Vec<String>,
    wakes: Vec<uuid::Uuid>,
}

pub async fn disable_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    world: &World,
    devices: &[String],
    org: &str,
) -> AppResult<DisabledUse> {
    sqlx::query(sql!(
        "DELETE FROM {} WHERE device_id = ANY($1) AND team = $2",
        world.t("device_access")
    ))
    .bind(devices)
    .bind(org)
    .execute(&mut **tx)
    .await?;
    sqlx::query(sql!(
        "UPDATE {} SET delivery='failed', ting_next_at=NULL, last_error='Device access was removed in this organization.'
         WHERE device_id=ANY($1) AND team=$2 AND delivery='pending'", world.t("requests")))
        .bind(devices).bind(org).execute(&mut **tx).await?;
    let sessions = sqlx::query_scalar(sql!(
        "SELECT session_id FROM {} WHERE device_id = ANY($1) AND team = $2 AND state <> 'ended'",
        world.t("sessions")
    ))
    .bind(devices)
    .bind(org)
    .fetch_all(&mut **tx)
    .await?;
    let wakes = sqlx::query_scalar(sql!(
        "SELECT wake_id FROM {} WHERE device_id = ANY($1) AND team = $2 AND state = 'open'",
        world.t("wake_requests")
    ))
    .bind(devices)
    .bind(org)
    .fetch_all(&mut **tx)
    .await?;
    Ok(DisabledUse { sessions, wakes })
}

pub async fn finish_disable(
    state: &AppState,
    world: &World,
    disabled: DisabledUse,
    actor: &extend_protocol::model::Member,
) -> AppResult<()> {
    for session in disabled.sessions {
        domain::end_session(
            state,
            world,
            &session,
            extend_protocol::model::EndReason::AccessRemoved,
            actor,
        )
        .await?;
    }
    for wake_id in disabled.wakes {
        crate::wake::withdraw(
            state,
            world,
            crate::wake::Withdraw::One { wake_id },
            extend_protocol::model::WakeEndReason::AccessRemoved,
        )
        .await;
    }
    Ok(())
}

use crate::{
    error::AppError,
    routes::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, ok},
    state::{Auth, Shared},
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ImportableQuery {
    limit: Option<i64>,
    cursor: Option<String>,
}
#[derive(sqlx::FromRow, Serialize)]
struct ImportableDevice {
    device_id: String,
    name: String,
    os: String,
    model: Option<String>,
    host_device_id: Option<String>,
}

/// The only cross-organization discovery: a caller's own configurations, without organization
/// names, access lists, session state or credentials. It never crosses a testing world boundary.
pub async fn importable(
    State(state): State<Shared>,
    auth: Auth,
    Query(q): Query<ImportableQuery>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let org = auth.team()?;
    let limit = limit(q.limit)?;
    let after = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let mut rows: Vec<ImportableDevice> = sqlx::query_as(sql!(
        "SELECT d.device_id, d.name, d.os, d.model, d.host_device_id FROM {} d
         WHERE d.owner_id = $1 AND d.removed_at IS NULL AND ($3::text IS NULL OR d.device_id > $3)
           AND NOT EXISTS (SELECT 1 FROM {} o WHERE o.device_id = d.device_id AND o.org_id = $2 AND o.removed_at IS NULL)
         ORDER BY d.device_id LIMIT $4", auth.world.t("devices"), auth.world.t("device_organizations")))
        .bind(auth.p.id()).bind(org).bind(after).bind(limit + 1).fetch_all(&state.pool).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next = more.then(|| encode_cursor(&rows.last().map(|d| d.device_id.clone()).unwrap_or_default()));
    Ok(ok("devices", serde_json::json!({"items": rows, "next_cursor": next})))
}

pub async fn import(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Body(input): Body<extend_protocol::model::DeviceImport>,
) -> AppResult<Response> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<uuid::Uuid>().ok());
    if key.is_none_or(|key| key.is_nil()) {
        return Err(AppError::invalid(
            "Send a non-nil UUID Idempotency-Key for this import.",
        ));
    }
    auth.require_carbon()?;
    let org = auth.team()?.to_owned();
    let d = domain::load_device(&state, &auth.world, &device_id)
        .await?
        .filter(|d| d.is_owner(&auth.p))
        .ok_or_else(|| domain::device_not_found(&device_id))?;
    let operation = format!("device-import:{org}:{device_id}");
    let hash = hash_json(&input);
    idempotent(
        &state,
        &auth.world,
        auth.p.id(),
        &operation,
        &headers,
        &hash,
        || async {
            let mut tx = state.pool.begin().await?;
            // An attachment's physical transport stays with its configured host. Import the owner's
            // host privately too so the attachment cannot reveal a host outside the current context.
            let mut ids = vec![d.device_id.clone()];
            let mut instances = vec![d.instance_id];
            if let Some(host) = &d.host_device_id {
                let host = domain::load_device(&state, &auth.world, host)
                    .await?
                    .filter(|h| h.is_owner(&auth.p))
                    .ok_or_else(|| domain::device_not_found(&device_id))?;
                ids.push(host.device_id);
                instances.push(host.instance_id);
            }
            domain::lock_instances(&mut tx, &auth.world, &instances).await?;
            for id in &ids {
                let owner: Option<String> = sqlx::query_scalar(sql!(
                    "SELECT owner_id FROM {} WHERE device_id = $1 AND removed_at IS NULL FOR UPDATE",
                    auth.world.t("devices")
                ))
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
                if owner.as_deref() != Some(auth.p.id()) {
                    return Err(domain::device_not_found(&device_id));
                }
                sqlx::query(sql!(
                    "INSERT INTO {} (device_id, org_id, visibility) VALUES ($1, $2, $3)
                ON CONFLICT (device_id, org_id) DO UPDATE SET removed_at = NULL, visibility = EXCLUDED.visibility
                WHERE {}.removed_at IS NOT NULL",
                    auth.world.t("device_organizations"),
                    auth.world.t("device_organizations")
                ))
                .bind(id)
                .bind(&org)
                .bind(if id == &device_id {
                    input.visibility.unwrap_or(Visibility::Personal).as_str()
                } else {
                    "personal"
                })
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            domain::log_in(
                &state,
                &auth.world,
                &device_id,
                &auth.p.member,
                "organization_imported",
                None,
                Some(&org),
                serde_json::json!({}),
            )
            .await;
            let view = domain::device_view(
                &state,
                &auth.world,
                &d,
                domain::Viewer::of(domain::Access::Owner, &auth.p),
                true,
            )
            .await;
            Ok((
                StatusCode::CREATED,
                "device",
                serde_json::to_value(view).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

/// A website removal only unbinds this organization. Native revoke remains physical/global.
pub async fn remove(state: &AppState, auth: &Auth, d: &domain::DeviceRow) -> AppResult<()> {
    let org = auth.team()?;
    for _ in 0..4 {
        let mut tx = state.pool.begin().await?;
        let children: Vec<(String, uuid::Uuid)> = sqlx::query_as(sql!(
        "SELECT device_id, instance_id FROM {} WHERE host_device_id = $1 AND owner_id = $2 AND removed_at IS NULL ORDER BY device_id",
        auth.world.t("devices")
    ))
    .bind(&d.device_id)
    .bind(auth.p.id())
    .fetch_all(&mut *tx)
    .await?;
        let mut instances: Vec<_> = children.iter().map(|(_, i)| *i).collect();
        instances.push(d.instance_id);
        domain::lock_instances(&mut tx, &auth.world, &instances).await?;
        // An attachment may have committed while this transaction waited for the host lock.
        // Retry with the complete lock set, preserving global instance lock order.
        let current: Vec<(String, uuid::Uuid)> = sqlx::query_as(sql!(
        "SELECT device_id, instance_id FROM {} WHERE host_device_id=$1 AND owner_id=$2 AND removed_at IS NULL ORDER BY device_id",
        auth.world.t("devices")))
        .bind(&d.device_id).bind(auth.p.id()).fetch_all(&mut *tx).await?;
        if current != children {
            tx.rollback().await?;
            continue;
        }
        let mut ids: Vec<_> = children.into_iter().map(|(id, _)| id).collect();
        ids.push(d.device_id.clone());
        sqlx::query(sql!(
            "UPDATE {} SET removed_at = now() WHERE device_id = ANY($1) AND org_id = $2 AND removed_at IS NULL",
            auth.world.t("device_organizations")
        ))
        .bind(&ids)
        .bind(org)
        .execute(&mut *tx)
        .await?;
        let disabled = disable_in(&mut tx, &auth.world, &ids, org).await?;
        tx.commit().await?;
        finish_disable(state, &auth.world, disabled, &auth.p.member).await?;
        for id in ids {
            domain::log_in(
                state,
                &auth.world,
                &id,
                &auth.p.member,
                "organization_removed",
                None,
                Some(org),
                serde_json::json!({}),
            )
            .await;
        }
        return Ok(());
    }
    Err(AppError::new(
        extend_protocol::ErrorCode::Conflict,
        "The host attachments changed while removing it. Retry the removal.",
    ))
}

/// Recheck inside the instance lock, shared by wake and request writes and visibility changes.
pub async fn require_grant<'c>(
    db: impl sqlx::PgExecutor<'c>,
    world: &World,
    device: &str,
    org: &str,
    silicon: &str,
) -> AppResult<()> {
    let allowed: bool = sqlx::query_scalar(sql!("SELECT EXISTS(SELECT 1 FROM {} a JOIN {} o ON o.device_id = a.device_id AND o.org_id = a.team WHERE a.device_id = $1 AND a.team = $2 AND a.silicon_id = $3 AND o.visibility = 'team' AND o.removed_at IS NULL)", world.t("device_access"), world.t("device_organizations")))
        .bind(device).bind(org).bind(silicon).fetch_one(db).await?;
    if allowed {
        Ok(())
    } else {
        Err(domain::device_not_found_in(device, org))
    }
}

/// Notification choices follow the organization binding, not the shared physical transport.
pub async fn wake_muted(state: &AppState, world: &World, device: &str, org: &str) -> AppResult<bool> {
    Ok(sqlx::query_scalar(sql!(
        "SELECT wake_muted FROM {} WHERE device_id=$1 AND org_id=$2 AND removed_at IS NULL",
        world.t("device_organizations")
    ))
    .bind(device)
    .bind(org)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or(true))
}

/// Owner history remains readable in the organization where a binding was removed.
pub async fn project_removal(
    state: &AppState,
    world: &World,
    device: &mut domain::DeviceRow,
    org: &str,
) -> AppResult<()> {
    let removed: Option<Option<time::OffsetDateTime>> = sqlx::query_scalar(sql!(
        "SELECT removed_at FROM {} WHERE device_id=$1 AND org_id=$2",
        world.t("device_organizations")
    ))
    .bind(&device.device_id)
    .bind(org)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(Some(at)) = removed {
        device.removed_at = Some(at);
        device.removed_reason = Some("device_removed".into());
    }
    Ok(())
}

/// Apply private bindings to persisted work before serving after an upgrade/restart, and heal
/// an interrupted visibility change. Captured IDs cannot include newly authorized work.
pub async fn reconcile(state: &AppState, world: &World) -> AppResult<()> {
    let sessions = sqlx::query_scalar(sql!(
        "SELECT s.session_id FROM {} s WHERE s.state <> 'ended' AND NOT EXISTS
         (SELECT 1 FROM {} o WHERE o.device_id=s.device_id AND o.org_id=s.team AND o.visibility='team' AND o.removed_at IS NULL)",
        world.t("sessions"), world.t("device_organizations")))
        .fetch_all(&state.pool).await?;
    let wakes = sqlx::query_scalar(sql!(
        "SELECT w.wake_id FROM {} w WHERE w.state='open' AND NOT EXISTS
         (SELECT 1 FROM {} o WHERE o.device_id=w.device_id AND o.org_id=w.team AND o.visibility='team' AND o.removed_at IS NULL)",
        world.t("wake_requests"), world.t("device_organizations")))
        .fetch_all(&state.pool).await?;
    sqlx::query(sql!(
        "UPDATE {} r SET delivery='failed', ting_next_at=NULL, last_error='Device access was removed in this organization.'
         WHERE r.delivery='pending' AND NOT EXISTS (SELECT 1 FROM {} o WHERE o.device_id=r.device_id AND o.org_id=r.team AND o.visibility='team' AND o.removed_at IS NULL)",
        world.t("requests"), world.t("device_organizations")))
        .execute(&state.pool).await?;
    finish_disable(state, world, DisabledUse { sessions, wakes }, &domain::system_member()).await
}
