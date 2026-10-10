//! Sessions: one Silicon at a time on a device, commands relayed to it, takeovers.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::Engine as _;
use extend_protocol::capability::{self, Origin, RESERVED_FLAGS};
use extend_protocol::frames::{CommandFrame, CommandOutcome, ServiceFrame};
use extend_protocol::model::{
    CommandError, CommandRequest, CommandResult, EndReason, FileInfo, FileKind, SessionCreate, Takeover, TakeoverCreate,
};
use extend_protocol::{
    COMMAND_TIMEOUT_DEFAULT_MS, COMMAND_TIMEOUT_MAX_MS, COMMAND_TIMEOUT_MIN_MS, ErrorCode, SELF_DESTRUCT_DEFAULT_MIN,
    SELF_DESTRUCT_MAX_MIN, SESSION_IDLE_S, SessionId, TAKEOVER_MAX_S,
};
use rand::Rng as _;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::db::World;
use crate::domain::{self, Access, SESSION_COLUMNS, SessionRow};
use crate::error::{AppError, AppResult};
use crate::files::{NewFile, Recipient};
use crate::hub::SendError;
use crate::state::{AppState, Auth, Shared};

/// The `device_in_use` refusal a Silicon gets. The holder is named, as in 1.0, only when it is the
/// caller itself, or runs through the same pair (the same Carbon gave both access) and is in the
/// caller's custodian circle; otherwise nothing about it is said.
pub async fn in_use_error(state: &AppState, d: &domain::DeviceRow, me: &str) -> AppError {
    let named = match &d.in_use_silicon {
        Some(h) if h == me => true,
        Some(h) if d.held_here() && d.held_by_side() => state.accounts.directory.same_circle(me, h).await,
        _ => false,
    };
    let hint = format!(
        "Ask for it with: extend request send {} --reason \"<why, up to 300 characters>\"",
        d.device_id
    );
    if !named {
        return AppError::new(
            ErrorCode::DeviceInUse,
            format!(
                "Device {} ({}) is in use. Only one Silicon can use a device at a time.",
                d.device_id, d.name
            ),
        )
        .hint(hint)
        .details(serde_json::json!({"in_use": {"hidden": true}}));
    }
    let since = d
        .in_use_since
        .map(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let holder = match &d.in_use_silicon {
        Some(h) => state.accounts.directory.public_id(h).await,
        None => String::new(),
    };
    AppError::new(
        ErrorCode::DeviceInUse,
        format!(
            "Device {} ({}) is being used by {holder} in session {} since {since}. Only one Silicon can use a device at a time.",
            d.device_id,
            d.name,
            d.in_use_session.clone().unwrap_or_default()
        ),
    )
    .hint(hint)
    .details(serde_json::json!({"in_use": {"silicon_id": holder, "silicon_uuid": d.in_use_silicon, "session_id": d.in_use_session, "since": since}}))
}

/// Another member of the device's lock group (its computer, or a device the computer carries) is
/// in use by another side.
pub fn group_in_use_error(d: &domain::DeviceRow) -> AppError {
    let message = match (&d.host_device_id, &d.host_name) {
        (Some(_), Some(host)) => format!(
            "{} pairs through {host}, and something on that computer is in use.",
            d.name
        ),
        _ => format!("A device {} carries is in use.", d.name),
    };
    AppError::new(ErrorCode::DeviceInUse, message)
        .hint(format!(
            "A computer and the devices it carries are used by one side at a time. Ask for it with: extend request send {} --reason \"...\"",
            d.device_id
        ))
        .details(serde_json::json!({"in_use": {"hidden": true}}))
}

/// Picks an unused session id of the shortest length that still has one (TECHNICAL.md section 1).
async fn allocate_session_id(
    state: &AppState,
    world: &World,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<String> {
    let mut len = 3usize;
    loop {
        let used: i64 = sqlx::query_scalar(sql!(
            "SELECT count(*) FROM {} WHERE length(session_id) = $1",
            world.t("session_ids")
        ))
        .bind(len as i32)
        .fetch_one(&mut **tx)
        .await?;
        let space = SessionId::space(len) as i64;
        if used < space {
            // Random tries first; near the end of a length, scan for the gaps.
            for _ in 0..32 {
                let candidate = SessionId::from_parts(rand::rng().random_range(0..space as u64), len).to_string();
                let res = sqlx::query(sql!(
                    "INSERT INTO {} (session_id) VALUES ($1) ON CONFLICT DO NOTHING",
                    world.t("session_ids")
                ))
                .bind(&candidate)
                .execute(&mut **tx)
                .await?;
                if res.rows_affected() == 1 {
                    return Ok(candidate);
                }
            }
            if len <= 4 {
                let taken: Vec<(String,)> = sqlx::query_as(sql!(
                    "SELECT session_id FROM {} WHERE length(session_id) = $1",
                    world.t("session_ids")
                ))
                .bind(len as i32)
                .fetch_all(&mut **tx)
                .await?;
                let taken: std::collections::HashSet<String> = taken.into_iter().map(|(s,)| s).collect();
                if let Some(free) = (0..space as u64)
                    .map(|v| SessionId::from_parts(v, len).to_string())
                    .find(|c| !taken.contains(c))
                {
                    let res = sqlx::query(sql!(
                        "INSERT INTO {} (session_id) VALUES ($1) ON CONFLICT DO NOTHING",
                        world.t("session_ids")
                    ))
                    .bind(&free)
                    .execute(&mut **tx)
                    .await?;
                    if res.rows_affected() == 1 {
                        return Ok(free);
                    }
                }
            }
            let _ = state;
            continue;
        }
        len += 1;
        if len > 16 {
            return Err(AppError::internal("session id space exhausted"));
        }
    }
}

pub async fn start(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    Body(input): Body<SessionCreate>,
) -> AppResult<Response> {
    auth.require_silicon()?;
    let device_id = input.device_id.to_string();
    let d = domain::load_device(&state, &auth.world, &device_id)
        .await?
        .ok_or_else(|| domain::device_not_found(&device_id))?;
    if domain::access_of(&state, &auth.world, &d, &auth.p).await? != Some(Access::Silicon) {
        return Err(domain::device_not_found(&device_id));
    }
    auth.live(&state).await?;
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let st = state.clone();
    let p = auth.p.clone();
    let isi = auth.isi.clone();
    idempotent(
        &state,
        &auth.world,
        auth.p.uuid(),
        "sessions",
        &headers,
        &hash,
        || async move {
            // Replay an already-created session before checking state changed by that creation.
            let online = domain::is_online(&st, &world, &d).await;
            if !online {
                let seen = d
                    .last_seen_at
                    .map(|t| format!(" It was last seen {t}."))
                    .unwrap_or_default();
                return Err(AppError::new(
                    ErrorCode::DeviceOffline,
                    format!("{} is offline, so it can't be used right now.{seen}", d.name),
                )
                .hint(offline_hint(&d)));
            }
            if !d.is_ready() {
                let left: Vec<String> = domain::setup_of(&st, &world, &d)
                    .await
                    .steps
                    .into_iter()
                    .filter(|s| s.status != extend_protocol::model::StepStatus::Done)
                    .map(|s| match s.error {
                        Some(e) => format!("{} ({e})", s.title),
                        None => s.title,
                    })
                    .collect();
                let owner = st.accounts.directory.public_id(&d.owner_id).await;
                return Err(AppError::new(
                    ErrorCode::DeviceNotReady,
                    format!("{} hasn't finished setup. Steps left: {}.", d.name, left.join("; ")),
                )
                .hint(format!(
                    "{owner} can finish them on the device; watch with `extend device setup {device_id}`."
                )));
            }
            if d.in_use_session.is_some() {
                return Err(in_use_error(&st, &d, p.uuid()).await);
            }
            let mut tx = st.pool.begin().await?;
            // The lock order: the whole lock group's instances first, so a revoke (which takes the
            // instance first too) either sees this session or this start sees no grant.
            let group = domain::lock_group(&mut *tx, &world, d.instance_id).await?;
            domain::lock_instances(&mut tx, &world, &group.members).await?;
            let granted: Option<(i32,)> = sqlx::query_as(sql!(
                "SELECT 1 FROM {} WHERE device_id = $1 AND silicon_id = $2",
                world.t("device_access")
            ))
            .bind(&device_id)
            .bind(p.uuid())
            .fetch_optional(&mut *tx)
            .await?;
            if granted.is_none() {
                return Err(AppError::new(
                    ErrorCode::NoAccess,
                    format!("{} has no access to {} ({device_id}).", p.public_id(), d.name),
                ));
            }
            // One side at a time across the lock group.
            let holders: Vec<(Uuid, String)> = sqlx::query_as(sql!(
                "SELECT l.instance_id, o.owner_id FROM {} l JOIN {} s ON s.session_id = l.session_id
               JOIN {} o ON o.device_id = s.device_id WHERE l.instance_id = ANY($1)",
                world.t("device_locks"),
                world.t("sessions"),
                world.t("devices")
            ))
            .bind(&group.members)
            .fetch_all(&mut *tx)
            .await?;
            for (instance, h_carbon) in &holders {
                if *instance == d.instance_id {
                    drop(tx);
                    let now = domain::load_device(&st, &world, &device_id)
                        .await?
                        .ok_or_else(|| domain::device_not_found(&device_id))?;
                    return Err(in_use_error(&st, &now, p.uuid()).await);
                }
                if *h_carbon != d.owner_id {
                    drop(tx);
                    return Err(group_in_use_error(&d));
                }
            }
            let sid = allocate_session_id(&st, &world, &mut tx).await?;
            let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
            sqlx::query(sql!(
            "INSERT INTO {} (session_id, device_id, silicon_id, state, idle_ends_at) VALUES ($1, $2, $3, 'active', $4)",
            world.t("sessions")
        ))
        .bind(&sid)
        .bind(&device_id)
        .bind(p.uuid())
        .bind(idle)
        .execute(&mut *tx)
        .await?;
            let locked = sqlx::query(sql!(
                "INSERT INTO {} (device_id, session_id, instance_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                world.t("device_locks")
            ))
            .bind(&device_id)
            .bind(&sid)
            .bind(d.instance_id)
            .execute(&mut *tx)
            .await?;
            if locked.rows_affected() == 0 {
                drop(tx);
                let now = domain::load_device(&st, &world, &device_id)
                    .await?
                    .ok_or_else(|| domain::device_not_found(&device_id))?;
                return Err(in_use_error(&st, &now, p.uuid()).await);
            }
            // A pair lasts while the physical device has activity, through any pair.
            domain::bump_activity(&mut *tx, &world, d.instance_id).await?;
            sqlx::query(sql!(
                "UPDATE {} SET last_used_at = now() WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(&device_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(sql!(
                "UPDATE {} SET last_used_at = now() WHERE device_id = $1 AND silicon_id = $2",
                world.t("device_access")
            ))
            .bind(&device_id)
            .bind(p.uuid())
            .execute(&mut *tx)
            .await?;
            let awake: Option<Option<bool>> = sqlx::query_scalar(sql!(
                "SELECT awake FROM {} WHERE instance_id = $1",
                world.t("device_instances")
            ))
            .bind(d.instance_id)
            .fetch_optional(&mut *tx)
            .await?;
            tx.commit().await?;
            st.session_principals
                .write()
                .await
                .insert((world.schema.clone(), sid.clone()), p.clone());
            // Ting delivers other Silicons' requests for this device to the Silicon now using it once
            // it is enrolled (in the background; requests stay pending meanwhile).
            crate::delivery::register_silicon(&st, &world, &p);
            // A Silicon that starts on a device whose awake state is unknown got what it asked for; on
            // one known not to be awake, its wake request stays open (and "woken" still comes).
            if awake.flatten().is_none() {
                crate::wake::withdraw(
                    &st,
                    &world,
                    crate::wake::Withdraw::Session {
                        instance: d.instance_id,
                        silicon_id: p.uuid(),
                    },
                    extend_protocol::model::WakeEndReason::SessionStarted,
                )
                .await;
            }
            let row = domain::load_session(&st, &world, &sid)
                .await?
                .ok_or_else(|| AppError::internal("session vanished"))?;
            let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
            let side = domain::side_of(&st, &world, &d).await.ok();
            st.hub
                .send(
                    &d.route(&world),
                    ServiceFrame::SessionStarted {
                        target,
                        session_id: sid.parse().map_err(AppError::internal)?,
                        silicon_id: p.public_id().to_owned(),
                        since: row.started_at,
                        side,
                    },
                )
                .await;
            // What devices in the group show of other sides' wake requests follows the new holder.
            crate::wake::resend_group(&st, &world, d.instance_id).await;
            domain::log(
                &st,
                &world,
                &device_id,
                &p.actor(),
                "session_started",
                Some(&sid),
                serde_json::json!({"isi": isi}),
            )
            .await;
            tracing::info!(session_id = %sid, device_id, silicon = p.uuid(), "session started");
            Ok((
                StatusCode::CREATED,
                "session",
                serde_json::to_value(row.view_for(&st).await).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

/// Why a device may be offline, and, when it last reported itself not awake, the wake hint.
fn offline_hint(d: &domain::DeviceRow) -> String {
    let base = match d.os() {
        extend_protocol::DeviceOs::Android => "Usual causes: no Wi-Fi, the Extend app was stopped, or battery optimisation paused it. Ask the device's Carbon to open the Extend app.".to_owned(),
        extend_protocol::DeviceOs::AndroidTv => "Usual causes: the TV is off or asleep, or not on the network. Ask the device's Carbon to open Silicon Extend TV.".to_owned(),
        extend_protocol::DeviceOs::Macos | extend_protocol::DeviceOs::Windows | extend_protocol::DeviceOs::Linux => {
            "Usual causes: the computer is asleep, off, or the Extend app isn't running.".to_owned()
        }
        _ => "It pairs through a computer: that computer must be online and on the same network as this device.".to_owned(),
    };
    match crate::wake::hint(d, false) {
        Some(h) => format!("{base} {h}"),
        None => base,
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    device_id: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
    /// A Carbon's view of a Silicon they look after (`si:` id or uuid).
    silicon: Option<String>,
}

pub async fn list(State(state): State<Shared>, auth: Auth, Query(q): Query<ListQuery>) -> AppResult<Response> {
    // A Silicon sees its own sessions; a Carbon, the sessions through their pairs, or, with
    // ?silicon=, those of a Silicon they look after.
    let lim = limit(q.limit)?;
    let before = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let (who, subject) = match (&q.silicon, auth.p.is_silicon()) {
        (None, true) => ("s.silicon_id = $1".to_owned(), auth.p.uuid().to_owned()),
        (None, false) => (
            format!(
                "EXISTS (SELECT 1 FROM {} d WHERE d.device_id = s.device_id AND d.owner_id = $1)",
                auth.world.t("devices")
            ),
            auth.p.uuid().to_owned(),
        ),
        (Some(silicon), false) => (
            "s.silicon_id = $1".to_owned(),
            super::silicons::looked_after(&state, &auth, silicon).await?.uuid,
        ),
        (Some(_), true) => {
            return Err(AppError::invalid(
                "A Silicon lists its own sessions; leave out ?silicon=.",
            ));
        }
    };
    let state_filter = match q.state.as_deref() {
        None => None,
        Some(s @ ("active" | "paused" | "ended")) => Some(s.to_owned()),
        Some(other) => {
            return Err(AppError::invalid(format!(
                "state must be active, paused or ended; got {other:?}."
            )));
        }
    };
    let rows: Vec<SessionRow> = sqlx::query_as(sql!(
        "SELECT {} FROM {} s WHERE {who}
           AND ($2::text IS NULL OR s.device_id = $2) AND ($3::text IS NULL OR s.state = $3)
           AND ($4::text IS NULL OR (s.started_at, s.session_id) < ((SELECT started_at FROM {} WHERE session_id = $4), $4))
         ORDER BY s.started_at DESC, s.session_id DESC LIMIT $5",
        SESSION_COLUMNS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", "),
        auth.world.t("sessions"),
        auth.world.t("sessions"),
    ))
    .bind(&subject)
    .bind(&q.device_id)
    .bind(&state_filter)
    .bind(&before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let mut items = Vec::new();
    for r in rows.iter().take(lim as usize) {
        items.push(r.view_for(&state).await);
    }
    let next = more.then(|| encode_cursor(&items.last().map(|s| s.session_id.to_string()).unwrap_or_default()));
    Ok(ok("sessions", serde_json::json!({"items": items, "next_cursor": next})))
}

/// Who may see a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seer {
    /// Its Silicon.
    Silicon,
    /// The Carbon who paired the device it runs on.
    Owner,
    /// The custodian of its Silicon (sees it and can end it; never acts as the Silicon).
    Custodian,
}

/// Loads a session the caller may see: its Silicon (while it has access), the Carbon who owns
/// its device, or its Silicon's custodian.
async fn visible_session(
    state: &AppState,
    auth: &Auth,
    session_id: &str,
) -> AppResult<(SessionRow, domain::DeviceRow, Seer)> {
    if session_id.parse::<SessionId>().is_err() {
        return Err(AppError::invalid(format!(
            "{session_id:?} is not a session id; session ids are 3 or more lowercase hexadecimal characters, like a3f."
        )));
    }
    let not_found = || {
        AppError::new(
            ErrorCode::SessionNotFound,
            format!("No session {session_id} is visible to you."),
        )
        .hint("List yours with `extend session ls`.")
    };
    let s = domain::load_session(state, &auth.world, session_id)
        .await?
        .ok_or_else(not_found)?;
    let d: domain::DeviceRow = sqlx::query_as(sql!("{} WHERE d.device_id = $1", domain::device_select(&auth.world)))
        .bind(&s.device_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(not_found)?;
    let seer = if d.is_owner(&auth.p) {
        Some(Seer::Owner)
    } else if auth.p.is_silicon() && s.silicon_id == auth.p.uuid() {
        (domain::access_of(state, &auth.world, &d, &auth.p).await? == Some(Access::Silicon)).then_some(Seer::Silicon)
    } else if auth.p.is_carbon()
        && state
            .accounts
            .directory
            .is_custodian(auth.p.uuid(), &s.silicon_id)
            .await
    {
        Some(Seer::Custodian)
    } else {
        None
    };
    let seer = seer.ok_or_else(not_found)?;
    Ok((s, d, seer))
}

fn not_session_owner(s: &SessionRow, shown: &str) -> AppError {
    AppError::new(
        ErrorCode::NotSessionOwner,
        format!("Session {} belongs to {shown}.", s.session_id),
    )
    .hint("The device's Carbon stops a session with `extend device stop <device_id>`.")
}

pub async fn get(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>) -> AppResult<Response> {
    let (s, d, seer) = visible_session(&state, &auth, &session_id).await?;
    let mut view = s.view_for(&state).await;
    let access = match seer {
        Seer::Owner => Some(Access::Owner),
        Seer::Silicon => Some(Access::Silicon),
        Seer::Custodian => None,
    };
    if let Some(access) = access {
        let device = domain::device_view(&state, &auth.world, &d, domain::Viewer::of(access, &auth.p), true).await;
        view.capabilities = device.capabilities.clone();
        view.commands = device.commands.clone();
        view.device = Some(Box::new(device));
    }
    Ok(ok("session", view))
}

/// Ends a session: its Silicon ends its own, and a Silicon's custodian can end it for them.
pub async fn end(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>) -> AppResult<Response> {
    let (s, _, seer) = visible_session(&state, &auth, &session_id).await?;
    // The device's Carbon who is also the Silicon's custodian ends it as its custodian.
    let custodian = seer == Seer::Custodian
        || (seer == Seer::Owner
            && state
                .accounts
                .directory
                .is_custodian(auth.p.uuid(), &s.silicon_id)
                .await);
    let (reason, extra) = match seer {
        Seer::Silicon => (EndReason::EndedBySilicon, serde_json::Value::Null),
        _ if custodian => (
            EndReason::StoppedByCarbon,
            serde_json::json!({"stopped_by": "custodian"}),
        ),
        _ => {
            let shown = state.accounts.directory.public_id(&s.silicon_id).await;
            return Err(not_session_owner(&s, &shown));
        }
    };
    let row = match domain::end_session_with(&state, &auth.world, &session_id, reason, &auth.p.actor(), extra).await? {
        Some(r) => r,
        None => s,
    };
    Ok(ok("session", row.view_for(&state).await))
}

fn takeover_of(s: &SessionRow) -> Option<Takeover> {
    s.takeover.clone().and_then(|t| serde_json::from_value(t).ok())
}

pub async fn takeover_start(
    State(state): State<Shared>,
    auth: Auth,
    Path(session_id): Path<String>,
    Body(input): Body<TakeoverCreate>,
) -> AppResult<Response> {
    let (s, d, seer) = visible_session(&state, &auth, &session_id).await?;
    if seer != Seer::Silicon {
        let shown = state.accounts.directory.public_id(&s.silicon_id).await;
        return Err(not_session_owner(&s, &shown));
    }
    auth.live(&state).await?;
    match s.state.as_str() {
        "ended" => return Err(session_ended(&s)),
        "paused" => {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "A takeover is already in progress in this session.",
            )
            .hint("See it with `extend takeover status`."));
        }
        _ => {}
    }
    let reason = input.reason.trim().to_owned();
    let n = reason.chars().count();
    if n == 0 || n > 300 {
        return Err(AppError::invalid(format!(
            "The takeover reason must be 1–300 characters; it is {n}."
        )));
    }
    let now = OffsetDateTime::now_utc();
    let t = Takeover {
        takeover_id: Uuid::now_v7(),
        session_id: s.session_id.parse().map_err(AppError::internal)?,
        reason: reason.clone(),
        started_at: now,
        expires_at: now + time::Duration::seconds(TAKEOVER_MAX_S),
    };
    sqlx::query(sql!(
        "UPDATE {} SET state = 'paused', takeover = $2, idle_ends_at = $3 WHERE session_id = $1 AND state = 'active'",
        auth.world.t("sessions")
    ))
    .bind(&session_id)
    .bind(serde_json::to_value(&t).map_err(AppError::internal)?)
    .bind(t.expires_at)
    .execute(&state.pool)
    .await?;
    let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
    state
        .hub
        .send(
            &d.route(&auth.world),
            ServiceFrame::Takeover {
                target,
                session_id: t.session_id.clone(),
                reason: reason.clone(),
                expires_at: t.expires_at,
            },
        )
        .await;
    domain::log(
        &state,
        &auth.world,
        &d.device_id,
        &auth.p.actor(),
        "takeover_started",
        Some(&session_id),
        serde_json::json!({"reason": reason}),
    )
    .await;
    Ok(super::created("takeover", t))
}

pub async fn takeover_get(
    State(state): State<Shared>,
    auth: Auth,
    Path(session_id): Path<String>,
) -> AppResult<Response> {
    let (s, _, _) = visible_session(&state, &auth, &session_id).await?;
    Ok(ok("takeover", if s.state == "paused" { takeover_of(&s) } else { None }))
}

/// Resumes a paused session. Used by the Silicon (`DELETE …/takeover`) and the device's Done button.
pub async fn release(
    state: &AppState,
    world: &World,
    session_id: &str,
    actor: &extend_protocol::model::Member,
) -> AppResult<bool> {
    let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
    let row: Option<(String,)> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'active', takeover = NULL, idle_ends_at = $2 WHERE session_id = $1 AND state = 'paused' RETURNING device_id",
        world.t("sessions")
    ))
    .bind(session_id)
    .bind(idle)
    .fetch_optional(&state.pool)
    .await?;
    let Some((device_id,)) = row else {
        return Ok(false);
    };
    if let Some(d) = domain::load_device(state, world, &device_id).await? {
        let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
        let _ = state
            .hub
            .send(
                &d.route(world),
                ServiceFrame::TakeoverEnded {
                    target,
                    session_id: session_id.parse().map_err(AppError::internal)?,
                },
            )
            .await;
    }
    domain::log(
        state,
        world,
        &device_id,
        actor,
        "takeover_released",
        Some(session_id),
        serde_json::json!({}),
    )
    .await;
    Ok(true)
}

pub async fn takeover_release(
    State(state): State<Shared>,
    auth: Auth,
    Path(session_id): Path<String>,
) -> AppResult<Response> {
    let (s, _, seer) = visible_session(&state, &auth, &session_id).await?;
    if seer == Seer::Custodian {
        let shown = state.accounts.directory.public_id(&s.silicon_id).await;
        return Err(not_session_owner(&s, &shown));
    }
    if !release(&state, &auth.world, &session_id, &auth.p.actor()).await? {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "No takeover is in progress in this session.",
        ));
    }
    Ok(no_content())
}

fn session_ended(s: &SessionRow) -> AppError {
    let reason = s.end_reason.as_deref().and_then(EndReason::parse);
    AppError::new(
        ErrorCode::SessionEnded,
        format!(
            "Session {} has ended: {}.",
            s.session_id,
            reason.map_or("unknown reason", EndReason::explain)
        ),
    )
    .hint(format!("Start a new one with `extend session new {}`.", s.device_id))
    .details(serde_json::json!({"end_reason": s.end_reason}))
}

/// Replaces typed text with `[redacted N chars]` in the activity log.
pub fn redact(spec: &capability::CommandSpec, args: &[String]) -> Vec<String> {
    if !spec.redact_text {
        return args.to_vec();
    }
    args.iter()
        .enumerate()
        .map(|(i, a)| {
            let keep = a.starts_with('-')
                || a.starts_with('@')
                || (spec.name == "clipboard" && i == 0)
                || (spec.name == "fill" && i == 0);
            if keep {
                a.clone()
            } else {
                format!("[redacted {} chars]", a.chars().count())
            }
        })
        .collect()
}

fn check_args(req: &CommandRequest) -> AppResult<&'static capability::CommandSpec> {
    let name = req.command.trim();
    if let Some(replacement) = capability::not_exposed(name) {
        let mut e = AppError::new(
            ErrorCode::UnknownCommand,
            format!("`{name}` is a device engine command Extend doesn't relay."),
        );
        if let Some(r) = replacement {
            e = e.hint(format!("Use `{r}` instead."));
        } else {
            e = e.hint(
                "Extend leaves out the device engine's tools for app developers (simulators, emulators, React Native, web).",
            );
        }
        return Err(e);
    }
    let spec = capability::command(name).ok_or_else(|| {
        AppError::new(ErrorCode::UnknownCommand, format!("`{name}` is not an Extend command."))
            .hint("See the commands for this device with `extend --help` while connected.")
    })?;
    for a in &req.args {
        let flag = a.split('=').next().unwrap_or_default();
        if RESERVED_FLAGS.contains(&flag) {
            return Err(AppError::invalid(format!(
                "`{flag}` picks a device or session inside the device engine; Extend picks those."
            ))
            .hint("Drop the flag; the session already names the device."));
        }
        if a.len() > 64 * 1024 {
            return Err(AppError::invalid("An argument is longer than 64 KiB."));
        }
    }
    if req.args.len() > 256 {
        return Err(AppError::invalid("A command takes at most 256 arguments."));
    }
    Ok(spec)
}

/// Refuses a command in a session that ended or is paused for a takeover.
async fn refuse_unless_active(state: &AppState, s: &SessionRow, d: &domain::DeviceRow) -> AppResult<()> {
    match s.state.as_str() {
        "ended" => Err(session_ended(s)),
        "paused" => {
            let reason = takeover_of(s).map(|t| t.reason).unwrap_or_default();
            let owner = state.accounts.directory.public_id(&d.owner_id).await;
            Err(AppError::new(
                ErrorCode::SessionPaused,
                format!("The session is paused while {owner} uses the device: {reason}"),
            )
            .hint("Wait for them to tap Done, or run `extend takeover release` if you started it and no longer need them."))
        }
        _ => Ok(()),
    }
}

/// Checks the sign-in, the current grant and session ownership before a device command.
async fn authorized_session(
    state: &Shared,
    auth: &Auth,
    session_id: &str,
) -> AppResult<(SessionRow, domain::DeviceRow)> {
    auth.require_silicon()?;
    let (s, d, seer) = visible_session(state, auth, session_id).await?;
    if seer != Seer::Silicon {
        let shown = state.accounts.directory.public_id(&s.silicon_id).await;
        return Err(not_session_owner(&s, &shown));
    }
    refuse_unless_active(state, &s, &d).await?;
    // A sign-out anywhere ends access at once, even in the middle of a session.
    auth.live(state).await?;
    // Access is re-checked on every command, not only at session start.
    if domain::access_of(state, &auth.world, &d, &auth.p).await? != Some(Access::Silicon) {
        domain::end_session(
            state,
            &auth.world,
            session_id,
            EndReason::AccessRemoved,
            &domain::system_member(),
        )
        .await?;
        return Err(AppError::new(
            ErrorCode::AccessRemoved,
            format!("Your access to {} was removed; session {session_id} has ended.", d.name),
        ));
    }
    Ok((s, d))
}

pub async fn command(
    State(state): State<Shared>,
    auth: Auth,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Body(req): Body<CommandRequest>,
) -> AppResult<Response> {
    let (s, d) = authorized_session(&state, &auth, &session_id).await?;
    let spec = check_args(&req)?;
    let online = domain::is_online(&state, &auth.world, &d).await;
    if !online {
        return Err(
            AppError::new(ErrorCode::DeviceOffline, format!("{} is offline right now.", d.name)).hint(offline_hint(&d)),
        );
    }
    let wake_hint = crate::wake::hint(&d, online);
    let caps = d.capabilities();
    let required = d.command_requirements(spec);
    if !required.iter().any(|c| caps.contains(c)) {
        let missing = d.missing();
        let why: Vec<String> = required
            .iter()
            .map(|c| {
                missing.iter().find(|m| m.capability == *c).map_or_else(
                    || format!("{} is not something a {} can do", c.as_str(), d.os().as_str()),
                    |m| format!("{}: {}", c.as_str(), m.reason),
                )
            })
            .collect();
        let mut hint = format!(
            "See what works with `extend device show {}` or `extend --help` while connected.",
            d.device_id
        );
        if let Some(w) = &wake_hint {
            hint = format!("{w} {hint}");
        }
        return Err(AppError::new(
            ErrorCode::UnsupportedOnDevice,
            format!(
                "`{}` doesn't work on {} right now. {}",
                spec.name,
                d.name,
                why.join("; ")
            ),
        )
        .hint(hint)
        .details(serde_json::json!({"needs_any_of": required, "missing": missing})));
    }
    let timeout_ms = req.timeout_ms.unwrap_or(COMMAND_TIMEOUT_DEFAULT_MS);
    if !(COMMAND_TIMEOUT_MIN_MS..=COMMAND_TIMEOUT_MAX_MS).contains(&timeout_ms) {
        return Err(AppError::invalid(format!(
            "timeout_ms must be 1000–300000; got {timeout_ms}."
        )));
    }
    let self_destruct = req.self_destruct_minutes.unwrap_or(SELF_DESTRUCT_DEFAULT_MIN);
    if !(1..=SELF_DESTRUCT_MAX_MIN).contains(&self_destruct) {
        return Err(AppError::invalid(format!(
            "self_destruct_minutes must be 1–43200 (30 days); got {self_destruct}."
        )));
    }
    let mut total_attach = 0usize;
    for a in &req.attachments {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&a.content_base64)
            .map_err(|_| AppError::invalid(format!("attachment {} is not valid base64.", a.name)))?;
        total_attach += bytes.len();
    }
    if total_attach > super::display_files::MAX_ATTACHMENT_BYTES
        || req.attachments.len() > super::display_files::MAX_ATTACHMENTS
    {
        return Err(AppError::invalid(
            "Attachments are limited to 8 files and 8 MiB in total.",
        ));
    }
    let _ = spec.origin == Origin::Extend;

    let lock = state
        .hub
        .session_lock((auth.world.schema.clone(), session_id.clone()))
        .await;
    let _guard = lock.lock().await;
    // The session may have ended or been paused while this command waited for the one before it.
    let now = domain::load_session(&state, &auth.world, &session_id)
        .await?
        .unwrap_or_else(|| s.clone());
    refuse_unless_active(&state, &now, &d).await?;
    let mut device_req = req.clone();
    let resolving_at = std::time::Instant::now();
    tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        super::display_files::resolve(&state, &auth, &mut device_req, total_attach),
    )
    .await
    .map_err(|_| {
        AppError::new(
            ErrorCode::CommandTimeout,
            "Reading the display file exceeded the command's timeout.",
        )
    })??;
    let timeout_ms = timeout_ms.saturating_sub(resolving_at.elapsed().as_millis() as u64);
    if timeout_ms == 0 {
        return Err(AppError::new(
            ErrorCode::CommandTimeout,
            "Reading the display file exhausted the command's timeout.",
        ));
    }
    // Reading private media may outlive a Stop/takeover that arrived during the read.
    if spec.name == "display" {
        let current = domain::load_session(&state, &auth.world, &session_id)
            .await?
            .unwrap_or_else(|| now.clone());
        refuse_unless_active(&state, &current, &d).await?;
    }
    state
        .session_principals
        .write()
        .await
        .insert((auth.world.schema.clone(), session_id.clone()), auth.p.clone());

    let command_id = Uuid::now_v7();
    let upload_ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
    let expires = OffsetDateTime::now_utc() + time::Duration::milliseconds(timeout_ms as i64 + 60_000);
    for u in &upload_ids {
        sqlx::query(sql!(
            "INSERT INTO {} (upload_id, device_id, command_id, expires_at) VALUES ($1, $2, $3, $4)",
            auth.world.t("uploads")
        ))
        .bind(u)
        .bind(d.host_device_id.as_ref().unwrap_or(&d.device_id))
        .bind(command_id)
        .bind(expires)
        .execute(&state.pool)
        .await?;
    }
    // A command in flight holds the idle timer (TECHNICAL.md section 5): until its deadline, plus
    // the idle window. The answer resets the window to 300 s from then.
    let held = OffsetDateTime::now_utc()
        + time::Duration::milliseconds(timeout_ms as i64 + 2_000)
        + time::Duration::seconds(SESSION_IDLE_S);
    sqlx::query(sql!(
        "UPDATE {} SET idle_ends_at = $2 WHERE session_id = $1 AND state = 'active'",
        auth.world.t("sessions")
    ))
    .bind(&session_id)
    .bind(held)
    .execute(&state.pool)
    .await?;
    let route = d.route(&auth.world);
    let started = OffsetDateTime::now_utc();
    let frame = ServiceFrame::Command(CommandFrame {
        id: command_id,
        session_id: session_id.parse().map_err(AppError::internal)?,
        target: d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok()),
        command: spec.name.to_owned(),
        args: device_req.args,
        attachments: device_req.attachments,
        timeout_ms,
        upload_ids: upload_ids.clone(),
    });
    // Wait for the device's answer, unless the session ends first (device removed, pair revoked,
    // access removed, Stop, logout...): then the Silicon hears why at once, not at the deadline.
    let relayed = {
        let answer = state
            .hub
            .command(&route, command_id, frame, Duration::from_millis(timeout_ms + 2_000));
        tokio::pin!(answer);
        tokio::select! {
            biased;
            outcome = &mut answer => Ok(outcome),
            ended = ended_while_running(&state, &auth.world, &session_id) => Err(ended),
        }
    };
    let duration_ms = (OffsetDateTime::now_utc() - started).whole_milliseconds() as i64;
    let redacted = redact(spec, &req.args);

    let log_command = |outcome_word: &'static str, files: Vec<Uuid>, error: Option<String>, warnings: Vec<String>| {
        let st = state.clone();
        let world = auth.world.clone();
        let device_id = d.device_id.clone();
        let sid = session_id.clone();
        let who = auth.p.actor();
        let args = redacted.clone();
        let cmd = spec.name.to_owned();
        let isi = auth.isi.clone();
        let instance = d.instance_id;
        async move {
            let mut details = serde_json::json!({"duration_ms": duration_ms, "error": error, "isi": isi});
            if !warnings.is_empty() {
                details["warnings"] = serde_json::json!(warnings);
            }
            let _ = sqlx::query(sql!(
                "INSERT INTO {} (id, device_id, actor_kind, actor_id, action, session_id, command, args, outcome, files, details)
                 VALUES ($1, $2, 'silicon', $3, 'command', $4, $5, $6, $7, $8, $9)",
                world.t("activity")
            ))
            .bind(command_id)
            .bind(&device_id)
            .bind(&who.id)
            .bind(&sid)
            .bind(&cmd)
            .bind(serde_json::to_value(&args).unwrap_or_default())
            .bind(outcome_word)
            .bind(serde_json::to_value(&files).unwrap_or_default())
            .bind(details)
            .execute(&st.pool)
            .await;
            let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
            let _ = sqlx::query(sql!(
                "UPDATE {} SET last_command_at = now(), idle_ends_at = $2, command_count = command_count + 1 WHERE session_id = $1 AND state = 'active'",
                world.t("sessions")
            ))
            .bind(&sid)
            .bind(idle)
            .execute(&st.pool)
            .await;
            // A pair lasts while the physical device has activity, through any pair.
            let _ = domain::bump_activity(&st.pool, &world, instance).await;
            let _ = sqlx::query(sql!(
                "UPDATE {} SET last_used_at = now() WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(&device_id)
            .execute(&st.pool)
            .await;
            idle
        }
    };

    let outcome = match relayed {
        Ok(Ok(o)) => o,
        Err(ended) => {
            // Tell the device to drop the command (apps also cancel on session_ended), and stop
            // waiting for an answer nobody will read.
            let _ = state.hub.send(&route, ServiceFrame::Cancel { id: command_id }).await;
            state.hub.resolve(&route, abandoned(command_id)).await;
            log_command("unknown", vec![], Some("session_ended".into()), vec![]).await;
            return Err(ended_mid_command(
                &session_id,
                ended.as_ref(),
                &d,
                spec.name,
                command_id,
            ));
        }
        Ok(Err(e)) => {
            // The socket may have closed because the pair ended; that is the precise answer.
            if let Ok(Some(now)) = domain::load_session(&state, &auth.world, &session_id).await
                && now.state == "ended"
            {
                log_command("unknown", vec![], Some("session_ended".into()), vec![]).await;
                return Err(ended_mid_command(&session_id, Some(&now), &d, spec.name, command_id));
            }
            match e {
                SendError::Timeout => {
                    log_command("timeout", vec![], Some("command_timeout".into()), vec![]).await;
                    return Err(AppError::new(
                        ErrorCode::CommandTimeout,
                        format!(
                            "{} did not answer `{}` within {} ms. It may still have run.",
                            d.name, spec.name, timeout_ms
                        ),
                    )
                    .hint("Check the screen with `extend snapshot` before retrying, or pass a longer --timeout."));
                }
                SendError::Offline | SendError::Dropped => {
                    log_command("unknown", vec![], Some("device_offline".into()), vec![]).await;
                    return Err(AppError::new(
                        ErrorCode::DeviceOffline,
                        format!(
                            "{} went offline while running `{}`; it may have run.",
                            d.name, spec.name
                        ),
                    )
                    .hint(offline_hint(&d)));
                }
            }
        }
    };

    // Store every file the device uploaded in Briefcase for the Silicon. A file that can't be
    // stored (or shared with the device's Carbon) is reported in `warnings`, never dropped quietly.
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let owner_id = if outcome.files.is_empty() {
        String::new()
    } else {
        state.accounts.directory.public_id(&d.owner_id).await
    };
    for f in &outcome.files {
        if !upload_ids.contains(&f.upload_id) {
            tracing::warn!(upload_id = %f.upload_id, name = f.name, "device reported a file under an upload id this command did not issue");
            warnings.push(format!(
                "{} was not stored: the device reported it under upload id {}, which Extend did not issue for this command. \
                 Run the command again; if it keeps happening, report it with `extend report`.",
                f.name, f.upload_id
            ));
            continue;
        }
        let path = state.cfg.data_dir.join("uploads").join(f.upload_id.to_string());
        let Ok(bytes) = tokio::fs::read(&path).await else {
            tracing::warn!(upload_id = %f.upload_id, name = f.name, "device listed a file it never uploaded");
            warnings.push(format!(
                "{} was not stored: {} made it but never uploaded it (the upload failed or was cut off). \
                 Run the command again; if it keeps happening, report it with `extend report`.",
                f.name, d.name
            ));
            continue;
        };
        let _ = tokio::fs::remove_file(&path).await;
        let size = bytes.len() as i64;
        let stored = state
            .files
            .store(
                &auth.p,
                NewFile {
                    operation_id: f.upload_id,
                    name: &f.name,
                    content_type: &f.content_type,
                    bytes,
                    owner_carbon: Recipient {
                        uuid: &d.owner_id,
                        id: &owner_id,
                    },
                },
            )
            .await;
        let stored = match stored {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(name = f.name, command_id = %command_id, error = %e.0.message, "storing a command file failed");
                warnings.push(format!(
                    "{} was not stored: {}{} Run the command again to make a new one.",
                    f.name,
                    e.0.message,
                    e.0.hint.as_deref().map(|h| format!(" {h}")).unwrap_or_default()
                ));
                continue;
            }
        };
        if let Some(why) = &stored.share_error {
            warnings.push(format!(
                "{} is stored, but not shared with {owner_id}, so they can't open it in Briefcase yet: {why}",
                f.name
            ));
        }
        let self_destruct_at =
            (!req.permanent).then(|| OffsetDateTime::now_utc() + time::Duration::minutes(i64::from(self_destruct)));
        let recorded = sqlx::query(sql!(
            "INSERT INTO {} (file_id, device_id, session_id, command_id, created_by, shared_with, name, kind, content_type, size_bytes, url, self_destruct_at, permanent)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            auth.world.t("files")
        ))
        .bind(stored.file_id)
        .bind(&d.device_id)
        .bind(&session_id)
        .bind(command_id)
        .bind(auth.p.uuid())
        .bind(&stored.shared_with)
        .bind(&f.name)
        .bind(f.kind.as_str())
        .bind(&f.content_type)
        .bind(size)
        .bind(&stored.url)
        .bind(self_destruct_at)
        .bind(req.permanent)
        .execute(&state.pool)
        .await;
        if let Err(e) = recorded {
            tracing::error!(file_id = %stored.file_id, url = stored.url, error = %e, "recording a stored file failed");
            warnings.push(format!(
                "{} is stored at {}, but Extend could not record it, so it won't be listed or self-destruct. \
                 Delete it in Briefcase when you no longer need it, and report this with `extend report`.",
                f.name, stored.url
            ));
            continue;
        }
        files.push(FileInfo {
            file_id: stored.file_id,
            name: f.name.clone(),
            kind: f.kind,
            content_type: f.content_type.clone(),
            size_bytes: size,
            url: stored.url,
            self_destruct_at,
            permanent: req.permanent,
            session_id: s.session_id.parse().ok(),
            device_id: d.device_id.parse().ok(),
            command_id: Some(command_id),
            created_by: Some(auth.p.public_id().to_owned()),
            shared_with: stored.shared_with.as_ref().map(|_| owner_id.clone()),
            created_at: Some(OffsetDateTime::now_utc()),
            team: None,
            created_by_uuid: Some(auth.p.uuid().to_owned()),
            shared_with_uuid: stored.shared_with.clone(),
        });
    }
    let _ = FileKind::Other;
    // A command that fails on a device known not to be awake says how to have it woken. Awake is
    // never a gate: the command ran.
    if !outcome.ok
        && let Some(w) = &wake_hint
    {
        warnings.push(w.clone());
    }
    if !crate::telemetry::opted_out(&headers) {
        crate::telemetry::record(
            &state,
            &auth.world,
            Some(auth.p.uuid()),
            serde_json::json!({
                "source": "service", "event": "command", "step": "session.command.relay", "success": outcome.ok,
                "duration_ms": duration_ms, "command": spec.name, "device_os": d.os().as_str(),
                "session_id": session_id, "command_id": command_id, "files": files.len(),
                "warnings": warnings.len(), "error_code": outcome.error.as_ref().map(|e| e.code.clone()),
            }),
        )
        .await;
    }
    let idle = log_command(
        if outcome.ok { "ok" } else { "failed" },
        files.iter().map(|f| f.file_id).collect(),
        outcome.error.as_ref().map(|e| e.code.clone()),
        warnings.clone(),
    )
    .await;
    Ok(ok(
        "command_result",
        CommandResult {
            command_id,
            session_id: s.session_id.parse().map_err(AppError::internal)?,
            command: spec.name.to_owned(),
            ok: outcome.ok,
            output: outcome.output,
            text: outcome.text,
            files,
            error: outcome.error.map(|e| CommandError {
                code: e.code,
                message: e.message,
                details: e.details,
            }),
            started_at: started,
            duration_ms,
            idle_ends_at: Some(idle),
            warnings,
        },
    ))
}

/// Resolves once the session has ended (checked in memory every 200 ms, and in the database every
/// 2 s). `None` when the session row is gone (its test environment was cleaned or removed).
async fn ended_while_running(state: &AppState, world: &World, session_id: &str) -> Option<SessionRow> {
    let key = (world.schema.clone(), session_id.to_owned());
    let mut ticks = 0u32;
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        ticks += 1;
        // `end_session` forgets the session's principal right after it commits the end.
        let forgotten = !state.session_principals.read().await.contains_key(&key);
        if forgotten || ticks.is_multiple_of(10) {
            match domain::load_session(state, world, session_id).await {
                Ok(Some(s)) if s.state == "ended" => return Some(s),
                Ok(None) => return None,
                _ => {}
            }
        }
    }
}

/// Clears the hub's wait for a command whose session ended (the waiter is already gone).
fn abandoned(id: Uuid) -> CommandOutcome {
    CommandOutcome {
        id,
        ok: false,
        output: serde_json::Value::Null,
        text: None,
        error: None,
        files: vec![],
    }
}

/// The answer to a command whose session ended while it ran: what ended it, and what to do.
fn ended_mid_command(
    session_id: &str,
    ended: Option<&SessionRow>,
    d: &domain::DeviceRow,
    command: &str,
    command_id: Uuid,
) -> AppError {
    let reason = ended.and_then(|s| s.end_reason.as_deref()).and_then(EndReason::parse);
    let why = match (ended, reason) {
        (_, Some(r)) => r.explain(),
        (None, None) => "its record is gone",
        (Some(_), None) => "unknown reason",
    };
    let device_id = &d.device_id;
    let hint = match reason {
        Some(EndReason::DeviceRemoved | EndReason::PairRevoked | EndReason::PairExpired) => format!(
            "{} is no longer paired. See the devices you can use with `extend device ls`.",
            d.name
        ),
        Some(EndReason::AccessRemoved) => {
            "The Carbon who paired the device took your access away, or signed out of Extend \
             (which ends the sessions of the Silicons they gave access to). See whether you still have access with \
             `extend device ls`; if not, ask them to grant it again."
                .to_owned()
        }
        // Any Carbon who paired the device can stop it; the one who did isn't named.
        Some(EndReason::StoppedByCarbon) => format!(
            "A Carbon who paired {} stopped it. Start a new session with `extend session new {device_id}` when they are done.",
            d.name
        ),
        Some(EndReason::SiliconLoggedOut) => {
            "Your sign-in ended. Sign in again: `silicon-accounts login --app extend -q | extend login --slt-stdin`."
                .to_owned()
        }
        Some(EndReason::DeviceOffline) => {
            format!("Start a new session with `extend session new {device_id}` once it is back online.")
        }
        _ => format!("Start a new session with `extend session new {device_id}`."),
    };
    AppError::new(
        ErrorCode::SessionEnded,
        format!(
            "Session {session_id} ended while `{command}` was running on {}: {why}. `{command}` may have run.",
            d.name
        ),
    )
    .hint(hint)
    .details(serde_json::json!({
        "end_reason": ended.and_then(|s| s.end_reason.clone()),
        "command_id": command_id,
        "may_have_run": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_typed_text() {
        let fill = capability::command("fill").unwrap();
        assert_eq!(
            redact(fill, &["@e3".into(), "hunter2".into()]),
            vec!["@e3".to_owned(), "[redacted 7 chars]".into()]
        );
        let clip = capability::command("clipboard").unwrap();
        assert_eq!(
            redact(clip, &["write".into(), "secret".into()]),
            vec!["write".to_owned(), "[redacted 6 chars]".into()]
        );
        let click = capability::command("click").unwrap();
        assert_eq!(redact(click, &["@e2".into()]), vec!["@e2".to_owned()]);
    }

    #[test]
    fn refuses_reserved_flags_and_hidden_commands() {
        let req = |c: &str, a: &[&str]| CommandRequest {
            command: c.into(),
            args: a.iter().map(|s| (*s).to_owned()).collect(),
            timeout_ms: None,
            self_destruct_minutes: None,
            permanent: false,
            attachments: vec![],
        };
        assert!(check_args(&req("click", &["@e2"])).is_ok());
        assert_eq!(
            check_args(&req("click", &["@e2", "--platform", "ios"]))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidInput
        );
        assert_eq!(
            check_args(&req("boot", &[])).unwrap_err().code(),
            ErrorCode::UnknownCommand
        );
        assert!(
            check_args(&req("devices", &[]))
                .unwrap_err()
                .0
                .hint
                .unwrap()
                .contains("extend device ls")
        );
    }
}
