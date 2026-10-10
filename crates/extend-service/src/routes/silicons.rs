//! The Silicons a Carbon looks after (is the custodian of) and the ones they gave access to.
//!
//! A custodian sees what its Silicons do in Extend and can stop it: their grants (and renounce
//! them), their sessions (`GET /api/v2/sessions?silicon=`, and end them), their files and their
//! requests. A custodian never acts as the Silicon: it can't start a session, ask for a device,
//! or grant access in its name.

use axum::extract::{Path, State};
use axum::response::Response;
use extend_protocol::DeviceOs;
use extend_protocol::ErrorCode;
use extend_protocol::account::{AccountRef, SiliconSummary};
use extend_protocol::model::{AccessGrant, MemberKind};
use time::OffsetDateTime;

use super::{no_content, ok};
use crate::accounts::AccountRow;
use crate::domain::{self, GrantEnd, RevokeScope};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Auth, Shared};

/// A Silicon named in a path (`si:scout` or its uuid) that the caller looks after.
pub async fn looked_after(state: &AppState, auth: &Auth, given: &str) -> AppResult<AccountRow> {
    let row = state
        .accounts
        .directory
        .resolve(given, Some(MemberKind::Silicon))
        .await?;
    if row.custodian_uuid.as_deref() != Some(auth.p.uuid()) {
        return Err(AppError::new(
            ErrorCode::NoAccess,
            format!(
                "{} doesn't look after {}: only a Silicon's custodian sees its activity in Extend.",
                auth.p.public_id(),
                row.shown_id()
            ),
        )
        .hint("A Silicon's custodian is set in Silicon Accounts."));
    }
    Ok(row)
}

async fn summary(state: &AppState, auth: &Auth, row: &AccountRow) -> AppResult<SiliconSummary> {
    let world = &crate::db::World::production();
    let mut s = SiliconSummary::new(
        AccountRef::new(row.uuid.clone(), row.shown_id(), MemberKind::Silicon)
            .display_name(row.display_name.clone())
            .pfp_url(row.pfp_url.clone()),
    );
    s.looked_after = row.custodian_uuid.as_deref() == Some(auth.p.uuid());
    s.granted_by_you = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} a JOIN {} d ON d.device_id = a.device_id
         WHERE a.silicon_id = $1 AND a.granted_by = $2 AND d.removed_at IS NULL",
        world.t("device_access"),
        world.t("devices")
    ))
    .bind(&row.uuid)
    .bind(auth.p.uuid())
    .fetch_one(&state.pool)
    .await?;
    if s.looked_after {
        s.grants = Some(
            sqlx::query_scalar(sql!(
                "SELECT count(*) FROM {} a JOIN {} d ON d.device_id = a.device_id WHERE a.silicon_id = $1 AND d.removed_at IS NULL",
                world.t("device_access"),
                world.t("devices")
            ))
            .bind(&row.uuid)
            .fetch_one(&state.pool)
            .await?,
        );
        s.running_sessions = Some(
            sqlx::query_scalar(sql!(
                "SELECT count(*) FROM {} WHERE silicon_id = $1 AND state <> 'ended'",
                world.t("sessions")
            ))
            .bind(&row.uuid)
            .fetch_one(&state.pool)
            .await?,
        );
    }
    Ok(s)
}

/// `GET /api/v2/silicons`: the Silicons the caller looks after, then the ones they gave access to.
pub async fn list(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    auth.require_carbon()?;
    let world = crate::db::World::production();
    let mut rows = state.accounts.directory.silicons_of(auth.p.uuid()).await?;
    let granted: Vec<String> = sqlx::query_scalar(sql!(
        "SELECT DISTINCT silicon_id FROM {} WHERE granted_by = $1",
        world.t("device_access")
    ))
    .bind(auth.p.uuid())
    .fetch_all(&state.pool)
    .await?;
    for uuid in granted {
        if rows.iter().any(|r| r.uuid == uuid) || crate::accounts::directory::legacy_id(&uuid) {
            continue;
        }
        if let Some(r) = state.accounts.directory.fetch(&uuid).await? {
            rows.push(r);
        }
    }
    let mut items = Vec::new();
    for r in rows.iter().filter(|r| r.status != "deleted") {
        items.push(summary(&state, &auth, r).await?);
    }
    Ok(ok("silicons", serde_json::json!({"items": items})))
}

/// `GET /api/v2/silicons/{silicon}`: one Silicon the caller looks after or gave access to.
pub async fn show(State(state): State<Shared>, auth: Auth, Path(silicon): Path<String>) -> AppResult<Response> {
    auth.require_carbon()?;
    let row = state
        .accounts
        .directory
        .resolve(&silicon, Some(MemberKind::Silicon))
        .await?;
    let s = summary(&state, &auth, &row).await?;
    if !s.looked_after && s.granted_by_you == 0 {
        return Err(AppError::new(
            ErrorCode::NoAccess,
            format!(
                "{} neither looks after {} nor gave it access to a device.",
                auth.p.public_id(),
                row.shown_id()
            ),
        ));
    }
    Ok(ok("silicon", s))
}

/// `GET /api/v2/silicons/{silicon}/grants`: every device a Silicon the caller looks after has
/// access to, whoever gave it.
pub async fn grants(State(state): State<Shared>, auth: Auth, Path(silicon): Path<String>) -> AppResult<Response> {
    let row = looked_after(&state, &auth, &silicon).await?;
    let world = crate::db::World::production();
    let rows: Vec<(String, String, String, OffsetDateTime, Option<OffsetDateTime>, bool, String, String, String)> =
        sqlx::query_as(sql!(
            "SELECT a.device_id, a.silicon_id, a.granted_by, a.granted_at, a.last_used_at, a.wake_muted, d.name, d.os, d.owner_id
             FROM {} a JOIN {} d ON d.device_id = a.device_id
             WHERE a.silicon_id = $1 AND d.removed_at IS NULL ORDER BY a.granted_at",
            world.t("device_access"),
            world.t("devices")
        ))
        .bind(&row.uuid)
        .fetch_all(&state.pool)
        .await?;
    let mut items = Vec::new();
    for (device_id, silicon_id, granted_by, granted_at, last_used_at, muted, name, os, owner) in rows {
        let Ok(device_id) = device_id.parse() else { continue };
        let directory = &state.accounts.directory;
        items.push(AccessGrant {
            device_id,
            silicon_id: directory.public_id(&silicon_id).await,
            granted_by: directory.public_id(&granted_by).await,
            granted_at,
            last_used_at,
            team: None,
            wake_muted: Some(muted),
            silicon_uuid: Some(silicon_id),
            granted_by_uuid: Some(granted_by),
            device_name: Some(name),
            device_os: serde_json::from_value::<DeviceOs>(serde_json::Value::String(os)).ok(),
            owner: Some(directory.member(MemberKind::Carbon, &owner).await),
        });
    }
    Ok(ok("access", serde_json::json!({"items": items})))
}

/// `DELETE /api/v2/silicons/{silicon}/grants/{device_id}`: the custodian (or the Silicon itself)
/// gives up the Silicon's access to a device. Its running session there ends.
pub async fn renounce(
    State(state): State<Shared>,
    auth: Auth,
    Path((silicon, device_id)): Path<(String, String)>,
) -> AppResult<Response> {
    let row = if auth.p.is_silicon() {
        let row = state
            .accounts
            .directory
            .resolve(&silicon, Some(MemberKind::Silicon))
            .await?;
        if row.uuid != auth.p.uuid() {
            return Err(AppError::new(
                ErrorCode::NoAccess,
                "A Silicon can only give up its own access.",
            ));
        }
        row
    } else {
        looked_after(&state, &auth, &silicon).await?
    };
    auth.live(&state).await?;
    let gone = domain::revoke_grants(
        &state,
        &auth.world,
        RevokeScope::Pair {
            device_id: &device_id,
            silicon_id: &row.uuid,
        },
        GrantEnd::Renounced,
        &auth.p.actor(),
    )
    .await?;
    if gone.is_empty() {
        return Err(AppError::new(
            ErrorCode::DeviceNotFound,
            format!("{} has no access to a device {device_id}.", row.shown_id()),
        )
        .hint(format!("See its access with `extend silicon show {}`.", row.shown_id())));
    }
    Ok(no_content())
}
