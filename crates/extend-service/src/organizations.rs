//! Device ownership and team authorization use the caller's live Organizations context.
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use extend_protocol::ErrorCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::accounts::Principal;
use crate::domain::{self, Access, DeviceRow};
use crate::error::{AppError, AppResult};
use crate::routes::{Body, ok};
use crate::state::{AppState, Auth, Shared};

pub const PREFIX: &str = "organization:";
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct Context {
    pub account_uuid: String,
    pub organizations: Vec<Organization>,
    pub groups: Vec<String>,
    pub share_targets: Vec<Target>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Organization {
    pub org_id: String,
    pub name: String,
    pub capabilities: Capabilities,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Capabilities {
    pub admin: bool,
    pub owner: bool,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Target {
    #[serde(rename = "type")]
    pub kind: String,
    pub org_id: String,
    pub id: String,
    pub name: String,
}
impl Context {
    pub fn manager(&self, org: &str) -> bool {
        self.organizations
            .iter()
            .any(|o| o.org_id == org && (o.capabilities.admin || o.capabilities.owner))
    }
    pub fn access(&self, owner: &str, teams: &[String]) -> Option<Access> {
        let org = owner.strip_prefix(PREFIX)?;
        if self.manager(org) {
            Some(Access::Owner)
        } else if self.organizations.iter().any(|o| o.org_id == org) && teams.iter().any(|t| self.groups.contains(t)) {
            Some(Access::Silicon)
        } else {
            None
        }
    }
    pub fn manager_keys(&self) -> Vec<String> {
        self.organizations
            .iter()
            .filter(|o| self.manager(&o.org_id))
            .map(|o| format!("{PREFIX}{}", o.org_id))
            .collect()
    }
    pub fn team_keys(&self, org: &str, ids: &[String]) -> AppResult<Vec<String>> {
        if !self.manager(org) {
            return Err(AppError::new(
                ErrorCode::NoAccess,
                "An organization owner or admin is required to manage its devices.",
            ));
        }
        let mut keys = Vec::new();
        for id in ids {
            let target = format!("{org}/{id}");
            if !self
                .share_targets
                .iter()
                .any(|t| t.org_id == org && t.kind == "team" && t.id == target)
            {
                return Err(AppError::invalid("Choose a current team in the device's organization."));
            }
            keys.push(format!("team:{target}"));
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
}
pub async fn context(state: &AppState, p: &Principal) -> AppResult<Context> {
    let url = format!(
        "{}/v1/integrations/{}/me",
        state.cfg.organizations_url, state.cfg.app_id
    );
    let res = state
        .integration_http
        .get(url)
        .bearer_auth(&p.token)
        .send()
        .await
        .map_err(|_| AppError::unavailable("Silicon Organizations", "connection failed"))?;
    if !res.status().is_success() {
        return Err(match res.status().as_u16() {
            401 => AppError::new(
                ErrorCode::TokenExpired,
                "Organizations could not verify this Extend session.",
            ),
            403 => AppError::new(ErrorCode::NoAccess, "Organizations denied access to Extend."),
            _ => AppError::unavailable("Silicon Organizations", res.status()),
        });
    }
    let value: Context = res
        .json()
        .await
        .map_err(|_| AppError::unavailable("Silicon Organizations", "invalid context"))?;
    if value.account_uuid != p.uuid() {
        return Err(AppError::new(
            ErrorCode::Unauthorized,
            "Organizations returned a different account.",
        ));
    }
    Ok(value)
}
/// Personal-only installations and queries do not depend on the Organizations service.
pub async fn device_context(state: &AppState, p: &Principal) -> AppResult<Context> {
    let has: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM extend.devices WHERE owner_id LIKE 'organization:%')")
            .fetch_one(&state.pool)
            .await?;
    if has {
        context(state, p).await
    } else {
        Ok(Context::default())
    }
}
pub async fn me(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    Ok(ok("organizations", context(&state, &auth.p).await?))
}
pub fn ids(group_keys: &[String]) -> Vec<&str> {
    group_keys
        .iter()
        .filter_map(|g| g.split_once('/').map(|(_, id)| id))
        .collect()
}
#[derive(Deserialize)]
pub struct TeamShare {
    team_ids: Vec<String>,
}
pub async fn share(
    State(state): State<Shared>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Body(update): Body<TeamShare>,
) -> AppResult<Response> {
    let device = domain::load_device(&state, &auth.world, &id)
        .await?
        .ok_or_else(|| domain::device_not_found(&id))?;
    let org = device
        .organization_id()
        .ok_or_else(|| AppError::invalid("Choose an organization-owned device."))?;
    if device.host_device_id.is_some() {
        return Err(AppError::invalid(
            "Change team access on the host device. Attached devices inherit its teams.",
        ));
    }
    let ctx = context(&state, &auth.p).await?;
    let teams = ctx.team_keys(org, &update.team_ids)?;
    crate::routes::devices::if_match(&headers, device.version)?;
    let mut tx = state.pool.begin().await?;
    domain::lock_instances(&mut tx, &auth.world, &[device.instance_id]).await?;
    let version:Option<i64>=sqlx::query_scalar("UPDATE extend.devices SET shared_teams=$2, version=version+1 WHERE device_id=$1 AND removed_at IS NULL AND version=$3 RETURNING version")
        .bind(&id).bind(&teams).bind(device.version).fetch_optional(&mut *tx).await?;
    let version = version.ok_or_else(|| {
        AppError::new(
            ErrorCode::VersionConflict,
            "Device settings changed. Refresh the device and retry.",
        )
    })?;
    sqlx::query("UPDATE extend.devices SET shared_teams=$2, version=version+1 WHERE host_device_id=$1 AND owner_id=$3 AND removed_at IS NULL")
        .bind(&id).bind(&teams).bind(&device.owner_id).execute(&mut *tx).await?;
    tx.commit().await?;
    domain::log(
        &state,
        &auth.world,
        &id,
        &auth.p.actor(),
        "team_access_updated",
        None,
        json!({"team_ids":ids(&teams)}),
    )
    .await;
    Ok(ok(
        "team_access",
        json!({"organization_id":org,"team_ids":ids(&teams),"version":version}),
    ))
}
pub fn can_use(device: &DeviceRow, access: Option<Access>) -> bool {
    access == Some(Access::Silicon) || (device.organization_id().is_some() && access == Some(Access::Owner))
}

/// IDs for filtering collections in SQL before pagination. A custodian's personal
/// authority never projects into an organization-owned device.
pub async fn visible_ids(state: &AppState, p: &Principal) -> AppResult<(Vec<String>, Vec<String>)> {
    let ctx = device_context(state, p).await?;
    let managers = ctx.manager_keys();
    let rows:Vec<(String,bool)>=sqlx::query_as("SELECT device_id,owner_id=ANY($1) FROM extend.devices WHERE owner_id LIKE 'organization:%' AND (owner_id=ANY($1) OR (removed_at IS NULL AND shared_teams && $2))")
        .bind(&managers).bind(&ctx.groups).fetch_all(&state.pool).await?;
    let managed = rows.iter().filter(|(_, m)| *m).map(|(id, _)| id.clone()).collect();
    Ok((rows.into_iter().map(|(id, _)| id).collect(), managed))
}
pub async fn usable_locked(
    state: &AppState,
    p: &Principal,
    id: &str,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<bool> {
    let row:Option<(String,Vec<String>,bool)>=sqlx::query_as("SELECT d.owner_id,d.shared_teams,EXISTS(SELECT 1 FROM extend.device_access g WHERE g.device_id=d.device_id AND g.silicon_id=$2) FROM extend.devices d WHERE d.device_id=$1 AND d.removed_at IS NULL")
        .bind(id).bind(p.uuid()).fetch_optional(&mut **tx).await?;
    match row {
        Some((owner, teams, _)) if owner.starts_with(PREFIX) => {
            Ok(context(state, p).await?.access(&owner, &teams).is_some())
        }
        Some((_, _, direct)) => Ok(p.is_silicon() && direct),
        None => Ok(false),
    }
}
