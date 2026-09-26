//! Sessions: one Silicon at a time on a device, commands relayed to it, takeovers.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::Engine as _;
use bridge_protocol::capability::{self, Origin, RESERVED_FLAGS};
use bridge_protocol::frames::{CommandFrame, ServiceFrame};
use bridge_protocol::model::{CommandError, CommandRequest, CommandResult, EndReason, FileInfo, FileKind, SessionCreate, Takeover, TakeoverCreate};
use bridge_protocol::{
    COMMAND_TIMEOUT_DEFAULT_MS, COMMAND_TIMEOUT_MAX_MS, COMMAND_TIMEOUT_MIN_MS, ErrorCode, SELF_DESTRUCT_DEFAULT_MIN, SELF_DESTRUCT_MAX_MIN, SESSION_IDLE_S,
    SessionId, TAKEOVER_MAX_S,
};
use rand::Rng as _;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::db::World;
use crate::domain::{self, Access, SESSION_COLUMNS, SessionRow};
use crate::error::{AppError, AppResult};
use crate::files::NewFile;
use crate::hub::SendError;
use crate::state::{AppState, Auth, Shared};

fn in_use_error(d: &domain::DeviceRow) -> AppError {
    let since = d.in_use_since.map(|t| t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()).unwrap_or_default();
    AppError::new(
        ErrorCode::DeviceInUse,
        format!(
            "Device {} ({}) is being used by {} in session {} since {since}. Only one Silicon can use a device at a time.",
            d.device_id,
            d.name,
            d.in_use_silicon.clone().unwrap_or_default(),
            d.in_use_session.clone().unwrap_or_default()
        ),
    )
    .hint(format!("Ask for it with: bridge request send {} --reason \"<why, up to 300 characters>\"", d.device_id))
    .details(serde_json::json!({"in_use": {"silicon_id": d.in_use_silicon, "session_id": d.in_use_session, "since": since}}))
}

/// Picks an unused session id of the shortest length that still has one (TECHNICAL.md section 1).
async fn allocate_session_id(state: &AppState, world: &World, tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> AppResult<String> {
    let mut len = 3usize;
    loop {
        let used: i64 = sqlx::query_scalar(sql!("SELECT count(*) FROM {} WHERE length(session_id) = $1", world.t("session_ids")))
            .bind(len as i32)
            .fetch_one(&mut **tx)
            .await?;
        let space = SessionId::space(len) as i64;
        if used < space {
            // Random tries first; near the end of a length, scan for the gaps.
            for _ in 0..32 {
                let candidate = SessionId::from_parts(rand::rng().random_range(0..space as u64), len).to_string();
                let res = sqlx::query(sql!("INSERT INTO {} (session_id) VALUES ($1) ON CONFLICT DO NOTHING", world.t("session_ids")))
                    .bind(&candidate)
                    .execute(&mut **tx)
                    .await?;
                if res.rows_affected() == 1 {
                    return Ok(candidate);
                }
            }
            if len <= 4 {
                let taken: Vec<(String,)> = sqlx::query_as(sql!("SELECT session_id FROM {} WHERE length(session_id) = $1", world.t("session_ids")))
                    .bind(len as i32)
                    .fetch_all(&mut **tx)
                    .await?;
                let taken: std::collections::HashSet<String> = taken.into_iter().map(|(s,)| s).collect();
                if let Some(free) = (0..space as u64).map(|v| SessionId::from_parts(v, len).to_string()).find(|c| !taken.contains(c)) {
                    let res = sqlx::query(sql!("INSERT INTO {} (session_id) VALUES ($1) ON CONFLICT DO NOTHING", world.t("session_ids")))
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

pub async fn start(State(state): State<Shared>, auth: Auth, headers: HeaderMap, Body(input): Body<SessionCreate>) -> AppResult<Response> {
    auth.require_silicon()?;
    let team = auth.team()?.to_owned();
    let device_id = input.device_id.to_string();
    let d = domain::load_device(&state, &auth.world, &device_id).await?.filter(|d| d.team == team).ok_or_else(|| domain::device_not_found(&device_id))?;
    if domain::access_of(&state, &auth.world, &d, &auth.p).await? != Some(Access::Silicon) {
        return Err(AppError::new(ErrorCode::NoAccess, format!("{} has no access to {} ({device_id}).", auth.p.id(), d.name))
            .hint(format!("Ask {} (the Carbon who owns it) to grant access: bridge device access grant {device_id} {}", d.owner_id, auth.p.id())));
    }
    if !domain::is_online(&state, &auth.world, &d).await {
        let seen = d.last_seen_at.map(|t| format!(" It was last seen {t}.")).unwrap_or_default();
        return Err(AppError::new(ErrorCode::DeviceOffline, format!("{} is offline, so it can't be used right now.{seen}", d.name)).hint(offline_hint(&d)));
    }
    if d.state != "ready" {
        let left: Vec<String> = d.setup().steps.into_iter().filter(|s| s.status != bridge_protocol::model::StepStatus::Done).map(|s| s.title).collect();
        return Err(AppError::new(ErrorCode::DeviceNotReady, format!("{} hasn't finished setup. Steps left: {}.", d.name, left.join("; ")))
            .hint(format!("{} can finish them on the device; watch with `bridge device setup {device_id}`.", d.owner_id)));
    }
    if d.in_use_session.is_some() {
        return Err(in_use_error(&d));
    }
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let st = state.clone();
    let p = auth.p.clone();
    let sel = auth.sel.clone();
    let isi = auth.isi.clone();
    idempotent(&state, &auth.world, auth.p.id(), "sessions", &headers, &hash, || async move {
        let mut tx = st.pool.begin().await?;
        let sid = allocate_session_id(&st, &world, &mut tx).await?;
        let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
        sqlx::query(sql!(
            "INSERT INTO {} (session_id, device_id, silicon_id, team, state, idle_ends_at) VALUES ($1, $2, $3, $4, 'active', $5)",
            world.t("sessions")
        ))
        .bind(&sid)
        .bind(&device_id)
        .bind(p.id())
        .bind(&team)
        .bind(idle)
        .execute(&mut *tx)
        .await?;
        let locked = sqlx::query(sql!("INSERT INTO {} (device_id, session_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", world.t("device_locks")))
            .bind(&device_id)
            .bind(&sid)
            .execute(&mut *tx)
            .await?;
        if locked.rows_affected() == 0 {
            drop(tx);
            let d = domain::load_device(&st, &world, &device_id).await?.ok_or_else(|| domain::device_not_found(&device_id))?;
            return Err(in_use_error(&d));
        }
        sqlx::query(sql!("UPDATE {} SET last_used_at = now(), last_activity_at = now() WHERE device_id = $1", world.t("devices")))
            .bind(&device_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(sql!("UPDATE {} SET last_used_at = now() WHERE device_id = $1 AND silicon_id = $2", world.t("device_access")))
            .bind(&device_id)
            .bind(p.id())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        st.session_principals.write().await.insert((world.schema.clone(), sid.clone()), (p.clone(), sel.clone()));
        let row = domain::load_session(&st, &world, &sid).await?.ok_or_else(|| AppError::internal("session vanished"))?;
        let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
        st.hub
            .send(
                &d.route(&world),
                ServiceFrame::SessionStarted { target, session_id: sid.parse().map_err(AppError::internal)?, silicon_id: p.id().to_owned(), since: row.started_at },
            )
            .await;
        domain::log(&st, &world, &device_id, &p.member, "session_started", Some(&sid), serde_json::json!({"isi": isi})).await;
        tracing::info!(world = %world.schema, session_id = %sid, device_id, silicon = p.id(), "session started");
        Ok((StatusCode::CREATED, "session", serde_json::to_value(row.view()).map_err(AppError::internal)?))
    })
    .await
}

fn offline_hint(d: &domain::DeviceRow) -> String {
    match d.os() {
        bridge_protocol::DeviceOs::Android => "Usual causes: no Wi-Fi, the Bridge app was stopped, or battery optimisation paused it. Ask the device's Carbon to open the Bridge app.".into(),
        bridge_protocol::DeviceOs::AndroidTv => "Usual causes: the TV is off or asleep, or not on the network. Ask the device's Carbon to open Silicon Bridge TV.".into(),
        bridge_protocol::DeviceOs::Macos | bridge_protocol::DeviceOs::Windows | bridge_protocol::DeviceOs::Linux => {
            "Usual causes: the computer is asleep, off, or the Bridge app isn't running.".into()
        }
        _ => "It pairs through a computer: that computer must be online and on the same network as this device.".into(),
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    device_id: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

pub async fn list(State(state): State<Shared>, auth: Auth, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let team = auth.team()?.to_owned();
    let lim = limit(q.limit)?;
    let before = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let who = if auth.p.is_silicon() {
        "s.silicon_id = $2".to_owned()
    } else {
        format!("EXISTS (SELECT 1 FROM {} d WHERE d.device_id = s.device_id AND d.owner_id = $2)", auth.world.t("devices"))
    };
    let state_filter = match q.state.as_deref() {
        None => None,
        Some(s @ ("active" | "paused" | "ended")) => Some(s.to_owned()),
        Some(other) => return Err(AppError::invalid(format!("state must be active, paused or ended; got {other:?}."))),
    };
    let rows: Vec<SessionRow> = sqlx::query_as(sql!(
        "SELECT {} FROM {} s WHERE s.team = $1 AND {who}
           AND ($3::text IS NULL OR s.device_id = $3) AND ($4::text IS NULL OR s.state = $4)
           AND ($5::text IS NULL OR (s.started_at, s.session_id) < ((SELECT started_at FROM {} WHERE session_id = $5), $5))
         ORDER BY s.started_at DESC, s.session_id DESC LIMIT $6",
        SESSION_COLUMNS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", "),
        auth.world.t("sessions"),
        auth.world.t("sessions"),
    ))
    .bind(&team)
    .bind(auth.p.id())
    .bind(&q.device_id)
    .bind(&state_filter)
    .bind(&before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let items: Vec<_> = rows.iter().take(lim as usize).map(SessionRow::view).collect();
    let next = more.then(|| encode_cursor(&items.last().map(|s| s.session_id.to_string()).unwrap_or_default()));
    Ok(ok("sessions", serde_json::json!({"items": items, "next_cursor": next})))
}

/// Loads a session the caller may see: its Silicon, or the Carbon who owns its device.
async fn visible_session(state: &AppState, auth: &Auth, session_id: &str) -> AppResult<(SessionRow, domain::DeviceRow)> {
    if session_id.parse::<SessionId>().is_err() {
        return Err(AppError::invalid(format!("{session_id:?} is not a session id; session ids are 3 or more lowercase hexadecimal characters, like a3f.")));
    }
    let not_found = || AppError::new(ErrorCode::SessionNotFound, format!("No session {session_id} is visible to you.")).hint("List yours with `bridge session ls`.");
    let s = domain::load_session(state, &auth.world, session_id).await?.ok_or_else(not_found)?;
    let d: domain::DeviceRow = sqlx::query_as(sql!("{} WHERE d.device_id = $1", domain::device_select(&auth.world)))
        .bind(&s.device_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(not_found)?;
    let mine = auth.p.is_silicon() && s.silicon_id == auth.p.id();
    let owner = d.is_owner(&auth.p);
    if !(mine || owner) || auth.p.team.as_deref() != Some(s.team.as_str()) {
        return Err(not_found());
    }
    Ok((s, d))
}

pub async fn get(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>) -> AppResult<Response> {
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    let access = if d.is_owner(&auth.p) { Access::Owner } else { Access::Silicon };
    let device = domain::device_view(&state, &auth.world, &d, access, true).await;
    let mut view = s.view();
    view.capabilities = device.capabilities.clone();
    view.commands = device.commands.clone();
    view.device = Some(Box::new(device));
    Ok(ok("session", view))
}

pub async fn end(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>) -> AppResult<Response> {
    let (s, _) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() {
        return Err(AppError::new(ErrorCode::NotSessionOwner, format!("Session {session_id} belongs to {}.", s.silicon_id))
            .hint("The device's Carbon stops a session with `bridge device stop <device_id>`."));
    }
    let row = match domain::end_session(&state, &auth.world, &session_id, EndReason::EndedBySilicon, &auth.p.member).await? {
        Some(r) => r,
        None => s,
    };
    Ok(ok("session", row.view()))
}

fn takeover_of(s: &SessionRow) -> Option<Takeover> {
    s.takeover.clone().and_then(|t| serde_json::from_value(t).ok())
}

pub async fn takeover_start(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>, Body(input): Body<TakeoverCreate>) -> AppResult<Response> {
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() {
        return Err(AppError::new(ErrorCode::NotSessionOwner, format!("Session {session_id} belongs to {}.", s.silicon_id)));
    }
    match s.state.as_str() {
        "ended" => return Err(session_ended(&s)),
        "paused" => return Err(AppError::new(ErrorCode::Conflict, "A takeover is already in progress in this session.").hint("See it with `bridge takeover status`.")),
        _ => {}
    }
    let reason = input.reason.trim().to_owned();
    let n = reason.chars().count();
    if n == 0 || n > 300 {
        return Err(AppError::invalid(format!("The takeover reason must be 1–300 characters; it is {n}.")));
    }
    let now = OffsetDateTime::now_utc();
    let t = Takeover {
        takeover_id: Uuid::now_v7(),
        session_id: s.session_id.parse().map_err(AppError::internal)?,
        reason: reason.clone(),
        started_at: now,
        expires_at: now + time::Duration::seconds(TAKEOVER_MAX_S),
    };
    sqlx::query(sql!("UPDATE {} SET state = 'paused', takeover = $2, idle_ends_at = $3 WHERE session_id = $1 AND state = 'active'", auth.world.t("sessions")))
        .bind(&session_id)
        .bind(serde_json::to_value(&t).map_err(AppError::internal)?)
        .bind(t.expires_at)
        .execute(&state.pool)
        .await?;
    let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
    state
        .hub
        .send(&d.route(&auth.world), ServiceFrame::Takeover { target, session_id: t.session_id.clone(), reason: reason.clone(), expires_at: t.expires_at })
        .await;
    domain::log(&state, &auth.world, &d.device_id, &auth.p.member, "takeover_started", Some(&session_id), serde_json::json!({"reason": reason})).await;
    Ok(super::created("takeover", t))
}

pub async fn takeover_get(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>) -> AppResult<Response> {
    let (s, _) = visible_session(&state, &auth, &session_id).await?;
    Ok(ok("takeover", if s.state == "paused" { takeover_of(&s) } else { None }))
}

/// Resumes a paused session. Used by the Silicon (`DELETE …/takeover`) and the device's Done button.
pub async fn release(state: &AppState, world: &World, session_id: &str, actor: &bridge_protocol::model::Member) -> AppResult<bool> {
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
        let _ = state.hub.send(&d.route(world), ServiceFrame::TakeoverEnded { target, session_id: session_id.parse().map_err(AppError::internal)? }).await;
    }
    domain::log(state, world, &device_id, actor, "takeover_released", Some(session_id), serde_json::json!({})).await;
    Ok(true)
}

pub async fn takeover_release(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>) -> AppResult<Response> {
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() && !d.is_owner(&auth.p) {
        return Err(AppError::new(ErrorCode::NotSessionOwner, format!("Session {session_id} belongs to {}.", s.silicon_id)));
    }
    if !release(&state, &auth.world, &session_id, &auth.p.member).await? {
        return Err(AppError::new(ErrorCode::Conflict, "No takeover is in progress in this session."));
    }
    Ok(no_content())
}

fn session_ended(s: &SessionRow) -> AppError {
    let reason = s.end_reason.as_deref().and_then(EndReason::parse);
    AppError::new(
        ErrorCode::SessionEnded,
        format!("Session {} has ended: {}.", s.session_id, reason.map_or("unknown reason", EndReason::explain)),
    )
    .hint(format!("Start a new one with `bridge session new {}`.", s.device_id))
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
            let keep = a.starts_with('-') || a.starts_with('@') || (spec.name == "clipboard" && i == 0) || (spec.name == "fill" && i == 0);
            if keep { a.clone() } else { format!("[redacted {} chars]", a.chars().count()) }
        })
        .collect()
}

fn check_args(req: &CommandRequest) -> AppResult<&'static capability::CommandSpec> {
    let name = req.command.trim();
    if let Some(replacement) = capability::not_exposed(name) {
        let mut e = AppError::new(ErrorCode::UnknownCommand, format!("`{name}` is an agent-device command Bridge doesn't relay."));
        if let Some(r) = replacement {
            e = e.hint(format!("Use `{r}` instead."));
        } else {
            e = e.hint("Bridge leaves out agent-device's tools for app developers (simulators, emulators, React Native, web).");
        }
        return Err(e);
    }
    let spec = capability::command(name).ok_or_else(|| {
        AppError::new(ErrorCode::UnknownCommand, format!("`{name}` is not a Bridge command.")).hint("See the commands for this device with `bridge --help` while connected.")
    })?;
    for a in &req.args {
        let flag = a.split('=').next().unwrap_or_default();
        if RESERVED_FLAGS.contains(&flag) {
            return Err(AppError::invalid(format!("`{flag}` picks a device or session inside agent-device; Bridge picks those.")).hint("Drop the flag; the session already names the device."));
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

pub async fn command(State(state): State<Shared>, auth: Auth, Path(session_id): Path<String>, Body(req): Body<CommandRequest>) -> AppResult<Response> {
    auth.require_silicon()?;
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() {
        return Err(AppError::new(ErrorCode::NotSessionOwner, format!("Session {session_id} belongs to {}.", s.silicon_id)));
    }
    match s.state.as_str() {
        "ended" => return Err(session_ended(&s)),
        "paused" => {
            let reason = takeover_of(&s).map(|t| t.reason).unwrap_or_default();
            return Err(AppError::new(ErrorCode::SessionPaused, format!("The session is paused while {} uses the device: {reason}", d.owner_id))
                .hint("Wait for them to tap Done, or run `bridge takeover release` if you started it and no longer need them."));
        }
        _ => {}
    }
    // Access is re-checked on every command, not only at session start.
    if domain::access_of(&state, &auth.world, &d, &auth.p).await? != Some(Access::Silicon) {
        domain::end_session(&state, &auth.world, &session_id, EndReason::AccessRemoved, &domain::system_member()).await?;
        return Err(AppError::new(ErrorCode::AccessRemoved, format!("Your access to {} was removed; session {session_id} has ended.", d.name)));
    }
    let spec = check_args(&req)?;
    if !domain::is_online(&state, &auth.world, &d).await {
        return Err(AppError::new(ErrorCode::DeviceOffline, format!("{} is offline right now.", d.name)).hint(offline_hint(&d)));
    }
    let caps = d.capabilities();
    if !spec.any_of.iter().any(|c| caps.contains(c)) {
        let missing = d.missing();
        let why: Vec<String> = spec
            .any_of
            .iter()
            .map(|c| missing.iter().find(|m| m.capability == *c).map_or_else(|| format!("{} is not something a {} can do", c.as_str(), d.os().as_str()), |m| format!("{}: {}", c.as_str(), m.reason)))
            .collect();
        return Err(AppError::new(ErrorCode::UnsupportedOnDevice, format!("`{}` doesn't work on {} right now. {}", spec.name, d.name, why.join("; ")))
            .hint("See what works with `bridge device show <device_id>` or `bridge --help` while connected.")
            .details(serde_json::json!({"needs_any_of": spec.any_of, "missing": missing})));
    }
    let timeout_ms = req.timeout_ms.unwrap_or(COMMAND_TIMEOUT_DEFAULT_MS);
    if !(COMMAND_TIMEOUT_MIN_MS..=COMMAND_TIMEOUT_MAX_MS).contains(&timeout_ms) {
        return Err(AppError::invalid(format!("timeout_ms must be 1000–300000; got {timeout_ms}.")));
    }
    let self_destruct = req.self_destruct_minutes.unwrap_or(SELF_DESTRUCT_DEFAULT_MIN);
    if !(1..=SELF_DESTRUCT_MAX_MIN).contains(&self_destruct) {
        return Err(AppError::invalid(format!("self_destruct_minutes must be 1–43200 (30 days); got {self_destruct}.")));
    }
    let mut total_attach = 0usize;
    for a in &req.attachments {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&a.content_base64)
            .map_err(|_| AppError::invalid(format!("attachment {} is not valid base64.", a.name)))?;
        total_attach += bytes.len();
    }
    if total_attach > 8 << 20 || req.attachments.len() > 8 {
        return Err(AppError::invalid("Attachments are limited to 8 files and 8 MiB in total."));
    }
    let _ = spec.origin == Origin::Bridge;

    let lock = state.hub.session_lock((auth.world.schema.clone(), session_id.clone())).await;
    let _guard = lock.lock().await;
    state.session_principals.write().await.insert((auth.world.schema.clone(), session_id.clone()), (auth.p.clone(), auth.sel.clone()));

    let command_id = Uuid::now_v7();
    let upload_ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
    let expires = OffsetDateTime::now_utc() + time::Duration::milliseconds(timeout_ms as i64 + 60_000);
    for u in &upload_ids {
        sqlx::query(sql!("INSERT INTO {} (upload_id, device_id, command_id, expires_at) VALUES ($1, $2, $3, $4)", auth.world.t("uploads")))
            .bind(u)
            .bind(d.host_device_id.as_ref().unwrap_or(&d.device_id))
            .bind(command_id)
            .bind(expires)
            .execute(&state.pool)
            .await?;
    }
    let started = OffsetDateTime::now_utc();
    let frame = ServiceFrame::Command(CommandFrame {
        id: command_id,
        session_id: session_id.parse().map_err(AppError::internal)?,
        target: d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok()),
        command: spec.name.to_owned(),
        args: req.args.clone(),
        attachments: req.attachments.clone(),
        timeout_ms,
        upload_ids: upload_ids.clone(),
    });
    let outcome = state.hub.command(&d.route(&auth.world), command_id, frame, Duration::from_millis(timeout_ms + 2_000)).await;
    let duration_ms = (OffsetDateTime::now_utc() - started).whole_milliseconds() as i64;
    let redacted = redact(spec, &req.args);

    let log_command = |outcome_word: &'static str, files: Vec<Uuid>, error: Option<String>| {
        let st = state.clone();
        let world = auth.world.clone();
        let device_id = d.device_id.clone();
        let sid = session_id.clone();
        let who = auth.p.member.clone();
        let args = redacted.clone();
        let cmd = spec.name.to_owned();
        let isi = auth.isi.clone();
        async move {
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
            .bind(serde_json::json!({"duration_ms": duration_ms, "error": error, "isi": isi}))
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
            let _ = sqlx::query(sql!("UPDATE {} SET last_activity_at = now(), last_used_at = now() WHERE device_id = $1", world.t("devices")))
                .bind(&device_id)
                .execute(&st.pool)
                .await;
            idle
        }
    };

    let outcome = match outcome {
        Ok(o) => o,
        Err(SendError::Timeout) => {
            log_command("timeout", vec![], Some("command_timeout".into())).await;
            return Err(AppError::new(ErrorCode::CommandTimeout, format!("{} did not answer `{}` within {} ms. It may still have run.", d.name, spec.name, timeout_ms))
                .hint("Check the screen with `bridge snapshot` before retrying, or pass a longer --timeout."));
        }
        Err(SendError::Offline | SendError::Dropped) => {
            log_command("unknown", vec![], Some("device_offline".into())).await;
            return Err(AppError::new(ErrorCode::DeviceOffline, format!("{} went offline while running `{}`; it may have run.", d.name, spec.name)).hint(offline_hint(&d)));
        }
    };

    // Store every file the device uploaded in Briefcase for the Silicon.
    let mut files = Vec::new();
    for f in &outcome.files {
        if !upload_ids.contains(&f.upload_id) {
            continue;
        }
        let path = state.cfg.data_dir.join("uploads").join(f.upload_id.to_string());
        let Ok(bytes) = tokio::fs::read(&path).await else {
            tracing::warn!(upload_id = %f.upload_id, "device listed a file it never uploaded");
            continue;
        };
        let _ = tokio::fs::remove_file(&path).await;
        let size = bytes.len() as i64;
        let stored = state
            .files
            .store(&auth.p, NewFile { name: &f.name, content_type: &f.content_type, bytes, owner_carbon: &d.owner_id }, auth.sel.as_ref())
            .await;
        let stored = match stored {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e.0.message, "storing a command file failed");
                continue;
            }
        };
        let self_destruct_at = (!req.permanent).then(|| OffsetDateTime::now_utc() + time::Duration::minutes(i64::from(self_destruct)));
        sqlx::query(sql!(
            "INSERT INTO {} (file_id, team, device_id, session_id, command_id, created_by, shared_with, name, kind, content_type, size_bytes, url, self_destruct_at, permanent)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            auth.world.t("files")
        ))
        .bind(stored.file_id)
        .bind(&s.team)
        .bind(&d.device_id)
        .bind(&session_id)
        .bind(command_id)
        .bind(auth.p.id())
        .bind(&stored.shared_with)
        .bind(&f.name)
        .bind(f.kind.as_str())
        .bind(&f.content_type)
        .bind(size)
        .bind(&stored.url)
        .bind(self_destruct_at)
        .bind(req.permanent)
        .execute(&state.pool)
        .await?;
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
            created_by: Some(auth.p.id().to_owned()),
            shared_with: stored.shared_with,
            created_at: Some(OffsetDateTime::now_utc()),
        });
    }
    let _ = FileKind::Other;
    crate::telemetry::record(
        &state,
        &auth.world,
        Some(auth.p.id()),
        serde_json::json!({
            "source": "service", "event": "command", "step": "session.command.relay", "success": outcome.ok,
            "duration_ms": duration_ms, "command": spec.name, "device_os": d.os().as_str(), "session_id": session_id,
            "command_id": command_id, "files": files.len(), "error_code": outcome.error.as_ref().map(|e| e.code.clone()),
        }),
    )
    .await;
    let idle = log_command(
        if outcome.ok { "ok" } else { "failed" },
        files.iter().map(|f| f.file_id).collect(),
        outcome.error.as_ref().map(|e| e.code.clone()),
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
            error: outcome.error.map(|e| CommandError { code: e.code, message: e.message, details: e.details }),
            started_at: started,
            duration_ms,
            idle_ends_at: Some(idle),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_typed_text() {
        let fill = capability::command("fill").unwrap();
        assert_eq!(redact(fill, &["@e3".into(), "hunter2".into()]), vec!["@e3".to_owned(), "[redacted 7 chars]".into()]);
        let clip = capability::command("clipboard").unwrap();
        assert_eq!(redact(clip, &["write".into(), "secret".into()]), vec!["write".to_owned(), "[redacted 6 chars]".into()]);
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
        assert_eq!(check_args(&req("click", &["@e2", "--platform", "ios"])).unwrap_err().code(), ErrorCode::InvalidInput);
        assert_eq!(check_args(&req("boot", &[])).unwrap_err().code(), ErrorCode::UnknownCommand);
        assert!(check_args(&req("devices", &[])).unwrap_err().0.hint.unwrap().contains("bridge device ls"));
    }
}
