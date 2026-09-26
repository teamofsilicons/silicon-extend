//! Operations several handlers share: reading devices, checking who may do what, ending sessions,
//! ending pairs, and the activity log.

use extend_protocol::capability::{DeviceKind, commands_for};
use extend_protocol::frames::ServiceFrame;
use extend_protocol::model::{
    Device, DeviceState, EndReason, InUse, Member, MemberKind, MissingCapability, Session, SessionState, Setup,
    Visibility,
};
use extend_protocol::{Capability, DeviceOs, ErrorCode};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::World;
use crate::error::{AppError, AppResult};
use crate::iam::Principal;
use crate::state::AppState;

#[derive(Debug, Clone, FromRow)]
pub struct DeviceRow {
    pub device_id: String,
    pub team: String,
    pub owner_id: String,
    pub name: String,
    pub os: String,
    pub os_version: Option<String>,
    pub model: Option<String>,
    pub address: Option<String>,
    pub visibility: String,
    pub pair_ttl_days: i32,
    pub paired_at: OffsetDateTime,
    pub last_activity_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub last_seen_at: Option<OffsetDateTime>,
    pub version: i64,
    pub host_device_id: Option<String>,
    pub state: String,
    pub setup: serde_json::Value,
    pub capabilities: serde_json::Value,
    pub missing: serde_json::Value,
    pub app_version: Option<String>,
    pub removed_at: Option<OffsetDateTime>,
    pub in_use_session: Option<String>,
    pub in_use_silicon: Option<String>,
    pub in_use_since: Option<OffsetDateTime>,
    pub in_use_state: Option<String>,
    pub access_count: i64,
}

impl DeviceRow {
    pub fn os(&self) -> DeviceOs {
        serde_json::from_value(serde_json::Value::String(self.os.clone())).unwrap_or(DeviceOs::Linux)
    }
    pub fn key(&self, world: &World) -> (String, String) {
        (world.schema.clone(), self.device_id.clone())
    }
    /// The socket commands for this device travel on: its own, or its host's.
    pub fn route(&self, world: &World) -> (String, String) {
        (
            world.schema.clone(),
            self.host_device_id.clone().unwrap_or_else(|| self.device_id.clone()),
        )
    }
    pub fn capabilities(&self) -> Vec<Capability> {
        serde_json::from_value(self.capabilities.clone()).unwrap_or_default()
    }
    pub fn missing(&self) -> Vec<MissingCapability> {
        serde_json::from_value(self.missing.clone()).unwrap_or_default()
    }
    pub fn setup(&self) -> Setup {
        serde_json::from_value(self.setup.clone()).unwrap_or_else(|_| Setup::from_steps(vec![]))
    }
    pub fn is_owner(&self, p: &Principal) -> bool {
        p.is_carbon() && self.owner_id == p.id() && p.team.as_deref() == Some(self.team.as_str())
    }
}

pub fn device_select(world: &World) -> String {
    format!(
        "SELECT d.device_id, d.team, d.owner_id, d.name, d.os, d.os_version, d.model, d.address, d.visibility, d.pair_ttl_days,
                d.paired_at, d.last_activity_at, d.last_used_at, d.last_seen_at, d.version, d.host_device_id, d.state, d.setup,
                d.capabilities, d.missing, d.app_version, d.removed_at,
                s.session_id AS in_use_session, s.silicon_id AS in_use_silicon, s.started_at AS in_use_since, s.state AS in_use_state,
                (SELECT count(*) FROM {access} a WHERE a.device_id = d.device_id) AS access_count
         FROM {devices} d
         LEFT JOIN {locks} l ON l.device_id = d.device_id
         LEFT JOIN {sessions} s ON s.session_id = l.session_id",
        access = world.t("device_access"),
        devices = world.t("devices"),
        locks = world.t("device_locks"),
        sessions = world.t("sessions"),
    )
}

pub async fn load_device(state: &AppState, world: &World, device_id: &str) -> AppResult<Option<DeviceRow>> {
    let sql = format!(
        "{} WHERE d.device_id = $1 AND d.removed_at IS NULL",
        device_select(world)
    );
    Ok(sqlx::query_as::<_, DeviceRow>(sqlx::AssertSqlSafe(sql.clone()))
        .bind(device_id)
        .fetch_optional(&state.pool)
        .await?)
}

pub fn device_not_found(device_id: &str) -> AppError {
    AppError::new(
        ErrorCode::DeviceNotFound,
        format!("No device {device_id} is visible to you in this team and environment."),
    )
    .hint("List the devices you can see with `extend device ls`.")
}

/// What a member may do with a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Owner,
    Silicon,
    /// Another Carbon in the team, for a team-visible device: read-only basics.
    TeamViewer,
}

pub async fn access_of(state: &AppState, world: &World, d: &DeviceRow, p: &Principal) -> AppResult<Option<Access>> {
    if p.team.as_deref() != Some(d.team.as_str()) {
        return Ok(None);
    }
    if d.is_owner(p) {
        return Ok(Some(Access::Owner));
    }
    if p.is_silicon() {
        let has: Option<(i32,)> = sqlx::query_as(sql!(
            "SELECT 1 FROM {} WHERE device_id = $1 AND silicon_id = $2",
            world.t("device_access")
        ))
        .bind(&d.device_id)
        .bind(p.id())
        .fetch_optional(&state.pool)
        .await?;
        return Ok(has.map(|_| Access::Silicon));
    }
    Ok((d.visibility == "team").then_some(Access::TeamViewer))
}

/// Loads a device the caller can see, with the access they have.
pub async fn visible_device(
    state: &AppState,
    world: &World,
    device_id: &str,
    p: &Principal,
) -> AppResult<(DeviceRow, Access)> {
    if device_id.parse::<extend_protocol::DeviceId>().is_err() {
        return Err(AppError::invalid(format!(
            "{device_id:?} is not a device id; device ids are 8 lowercase hexadecimal characters, like 7c1e09ab."
        )));
    }
    let d = load_device(state, world, device_id)
        .await?
        .ok_or_else(|| device_not_found(device_id))?;
    let access = access_of(state, world, &d, p)
        .await?
        .ok_or_else(|| device_not_found(device_id))?;
    Ok((d, access))
}

pub async fn owned_device(state: &AppState, world: &World, device_id: &str, p: &Principal) -> AppResult<DeviceRow> {
    let (d, access) = visible_device(state, world, device_id, p).await?;
    if access != Access::Owner {
        return Err(AppError::new(
            ErrorCode::NotOwner,
            format!("Only {} (the Carbon who paired {}) can do this.", d.owner_id, d.name),
        ));
    }
    Ok(d)
}

pub async fn is_online(state: &AppState, world: &World, d: &DeviceRow) -> bool {
    let route = d.route(world);
    if !state.hub.is_connected(&route).await {
        return false;
    }
    if d.host_device_id.is_some() {
        return state.hub.attached(&d.key(world)).await.is_some_and(|a| a.online);
    }
    true
}

pub async fn device_view(state: &AppState, world: &World, d: &DeviceRow, access: Access, detail: bool) -> Device {
    let os = d.os();
    let online = is_online(state, world, d).await;
    let owner = Member {
        kind: MemberKind::Carbon,
        id: d.owner_id.clone(),
        display_name: None,
    };
    let base = Device {
        device_id: d
            .device_id
            .parse()
            .unwrap_or_else(|_| extend_protocol::DeviceId::random()),
        name: d.name.clone(),
        os,
        os_version: None,
        model: None,
        kind: os.kind(),
        owner,
        team: Some(d.team.clone()),
        visibility: if d.visibility == "personal" {
            Visibility::Personal
        } else {
            Visibility::Team
        },
        host_device_id: None,
        state: if d.state == "ready" {
            DeviceState::Ready
        } else {
            DeviceState::Setup
        },
        online,
        last_seen_at: None,
        in_use: None,
        last_used_at: None,
        paired_at: None,
        pair_ttl_days: None,
        pair_expires_at: None,
        days_left: None,
        access_count: None,
        app_version: None,
        version: None,
        capabilities: None,
        missing: None,
        commands: None,
    };
    if access == Access::TeamViewer {
        return base;
    }
    let expires = d.last_activity_at + time::Duration::days(i64::from(d.pair_ttl_days));
    let days_left = ((expires - OffsetDateTime::now_utc()).whole_hours() as f64 / 24.0)
        .ceil()
        .max(0.0) as i64;
    let in_use = match (&d.in_use_session, &d.in_use_silicon, d.in_use_since) {
        (Some(s), Some(si), Some(since)) => s.parse().ok().map(|session_id| InUse {
            silicon_id: si.clone(),
            session_id,
            since,
            paused: d.in_use_state.as_deref() == Some("paused"),
        }),
        _ => None,
    };
    let mut caps = d.capabilities();
    if !online {
        caps.clear();
    }
    Device {
        os_version: d.os_version.clone(),
        model: d.model.clone(),
        host_device_id: d.host_device_id.as_ref().and_then(|h| h.parse().ok()),
        last_seen_at: d.last_seen_at,
        in_use,
        last_used_at: d.last_used_at,
        paired_at: Some(d.paired_at),
        pair_ttl_days: Some(d.pair_ttl_days),
        pair_expires_at: Some(expires),
        days_left: Some(days_left),
        access_count: Some(d.access_count),
        app_version: d.app_version.clone(),
        version: Some(d.version),
        capabilities: detail.then(|| caps.clone()),
        missing: detail.then(|| {
            let mut m = d.missing();
            if !online {
                m.insert(
                    0,
                    MissingCapability {
                        capability: Capability::ScreenRead,
                        reason: "The device is offline, so nothing works on it right now.".into(),
                    },
                );
            }
            m
        }),
        commands: detail.then(|| commands_for(&caps).into_iter().map(str::to_owned).collect()),
        ..base
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct SessionRow {
    pub session_id: String,
    pub device_id: String,
    pub silicon_id: String,
    pub team: String,
    pub state: String,
    pub started_at: OffsetDateTime,
    pub last_command_at: Option<OffsetDateTime>,
    pub idle_ends_at: Option<OffsetDateTime>,
    pub ended_at: Option<OffsetDateTime>,
    pub end_reason: Option<String>,
    pub command_count: i64,
    pub takeover: Option<serde_json::Value>,
}

impl SessionRow {
    pub fn view(&self) -> Session {
        Session {
            session_id: self
                .session_id
                .parse()
                .unwrap_or_else(|_| extend_protocol::SessionId::from_parts(0, 3)),
            device_id: self
                .device_id
                .parse()
                .unwrap_or_else(|_| extend_protocol::DeviceId::random()),
            silicon_id: self.silicon_id.clone(),
            state: match self.state.as_str() {
                "active" => SessionState::Active,
                "paused" => SessionState::Paused,
                _ => SessionState::Ended,
            },
            started_at: self.started_at,
            last_command_at: self.last_command_at,
            idle_ends_at: if self.state == "ended" { None } else { self.idle_ends_at },
            ended_at: self.ended_at,
            end_reason: self.end_reason.as_deref().and_then(EndReason::parse),
            command_count: self.command_count,
            device: None,
            capabilities: None,
            commands: None,
        }
    }
}

pub const SESSION_COLUMNS: &str = "session_id, device_id, silicon_id, team, state, started_at, last_command_at, idle_ends_at, ended_at, end_reason, command_count, takeover";

pub async fn load_session(state: &AppState, world: &World, session_id: &str) -> AppResult<Option<SessionRow>> {
    Ok(sqlx::query_as::<_, SessionRow>(sql!(
        "SELECT {SESSION_COLUMNS} FROM {} WHERE session_id = $1",
        world.t("sessions")
    ))
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn log(
    state: &AppState,
    world: &World,
    device_id: &str,
    actor: &Member,
    action: &str,
    session_id: Option<&str>,
    details: serde_json::Value,
) {
    let res = sqlx::query(sql!(
        "INSERT INTO {} (id, device_id, actor_kind, actor_id, action, session_id, details) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        world.t("activity")
    ))
    .bind(Uuid::now_v7())
    .bind(device_id)
    .bind(match actor.kind {
        MemberKind::Carbon => "carbon",
        MemberKind::Silicon => "silicon",
    })
    .bind(&actor.id)
    .bind(action)
    .bind(session_id)
    .bind(details)
    .execute(&state.pool)
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, device_id, action, "writing the activity log failed");
    }
}

pub fn system_member() -> Member {
    Member {
        kind: MemberKind::Carbon,
        id: "extend".into(),
        display_name: Some("Silicon Extend".into()),
    }
}

/// Ends a session: releases the device, tells the device, and logs why. Safe to call twice.
pub async fn end_session(
    state: &AppState,
    world: &World,
    session_id: &str,
    reason: EndReason,
    actor: &Member,
) -> AppResult<Option<SessionRow>> {
    let mut tx = state.pool.begin().await?;
    let row: Option<SessionRow> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'ended', ended_at = now(), end_reason = $2, idle_ends_at = NULL
         WHERE session_id = $1 AND state <> 'ended' RETURNING {SESSION_COLUMNS}",
        world.t("sessions")
    ))
    .bind(session_id)
    .bind(reason.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query(sql!("DELETE FROM {} WHERE session_id = $1", world.t("device_locks")))
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let Some(row) = row else {
        return Ok(None);
    };
    state
        .session_principals
        .write()
        .await
        .remove(&(world.schema.clone(), session_id.to_owned()));
    if let Some(d) = load_device(state, world, &row.device_id).await? {
        let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
        let _ = state
            .hub
            .send(
                &d.route(world),
                ServiceFrame::SessionEnded {
                    target,
                    session_id: row
                        .session_id
                        .parse()
                        .unwrap_or_else(|_| extend_protocol::SessionId::from_parts(0, 3)),
                    reason,
                },
            )
            .await;
    }
    log(
        state,
        world,
        &row.device_id,
        actor,
        "session_ended",
        Some(session_id),
        serde_json::json!({"reason": reason.as_str(), "explain": reason.explain()}),
    )
    .await;
    tracing::info!(world = %world.schema, session_id, reason = reason.as_str(), "session ended");
    Ok(Some(row))
}

/// Ends every running session on a device.
pub async fn end_device_sessions(
    state: &AppState,
    world: &World,
    device_id: &str,
    reason: EndReason,
    actor: &Member,
) -> AppResult<()> {
    let ids: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT session_id FROM {} WHERE device_id = $1 AND state <> 'ended'",
        world.t("sessions")
    ))
    .bind(device_id)
    .fetch_all(&state.pool)
    .await?;
    for (id,) in ids {
        end_session(state, world, &id, reason, actor).await?;
    }
    Ok(())
}

/// Ends a pair: sessions end, access goes, the credential stops working, the log stays readable.
/// Devices paired through this one (when it's a host) end with it.
pub fn unpair<'a>(
    state: &'a AppState,
    world: &'a World,
    device_id: &'a str,
    reason: EndReason,
    actor: &'a Member,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let Some(d) = load_device(state, world, device_id).await? else {
            return Ok(());
        };
        end_device_sessions(state, world, device_id, reason, actor).await?;
        let hosted: Vec<(String,)> = sqlx::query_as(sql!(
            "SELECT device_id FROM {} WHERE host_device_id = $1 AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(device_id)
        .fetch_all(&state.pool)
        .await?;
        for (child,) in hosted {
            unpair(state, world, &child, reason, actor).await?;
        }
        sqlx::query(sql!("DELETE FROM {} WHERE device_id = $1", world.t("device_access")))
            .bind(device_id)
            .execute(&state.pool)
            .await?;
        sqlx::query(sql!(
            "UPDATE {} SET removed_at = now(), removed_reason = $2, credential_digest = NULL WHERE device_id = $1",
            world.t("devices")
        ))
        .bind(device_id)
        .bind(reason.as_str())
        .execute(&state.pool)
        .await?;
        if let Some(host) = &d.host_device_id {
            let _ = state
                .hub
                .send(
                    &(world.schema.clone(), host.clone()),
                    ServiceFrame::Attach {
                        device_id: d
                            .device_id
                            .parse()
                            .unwrap_or_else(|_| extend_protocol::DeviceId::random()),
                        os: d.os(),
                        name: d.name.clone(),
                        address: d.address.clone(),
                        removed: true,
                    },
                )
                .await;
        } else {
            let key = d.key(world);
            let _ = state.hub.send(&key, ServiceFrame::Unpaired { reason }).await;
            // Give the frame a moment to flush before the socket goes.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            state.hub.disconnect(&key).await;
        }
        let action = match reason {
            EndReason::PairRevoked => "pair_revoked",
            EndReason::PairExpired => "pair_expired",
            _ => "removed",
        };
        log(
            state,
            world,
            device_id,
            actor,
            action,
            None,
            serde_json::json!({"reason": reason.as_str()}),
        )
        .await;
        Ok(())
    })
}

/// The device kind label used in messages.
pub fn kind_word(os: DeviceOs) -> &'static str {
    match os.kind() {
        DeviceKind::Phone => "phone",
        DeviceKind::Tablet => "tablet",
        DeviceKind::Tv => "TV",
        DeviceKind::Computer => "computer",
    }
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}
