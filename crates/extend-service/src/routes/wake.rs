//! Wake requests (`/devices/{id}/wake-requests`, `/wake-settings`) and Ting registration
//! (`/ting-registration`). The rules are in crate::wake.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use extend_protocol::model::{
    DeviceNotice, MutedSilicon, TingRegistration, TingStatus, WakeAnswer, WakeAnswerKind, WakeAnswered, WakeCreate,
    WakeEndReason, WakeSettings, WakeSettingsView,
};
use extend_protocol::{ErrorCode, WAKE_ASK_AGAIN_AFTER_S, WAKE_REQUEST_TTL_S, WAKE_TING_EVERY_S};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::db::World;
use crate::delivery::{self, Actor};
use crate::domain::{self, Access, DeviceRow};
use crate::error::{AppError, AppResult};
use crate::iam::{Principal, TestingSelection};
use crate::state::{AppState, Auth, Shared};
use crate::wake::{self, WAKE_COLUMNS, WakeRow, Withdraw};

/// Carbon wake Tings sent to `carbon` in the last hour, across their pairs and Teams.
pub async fn carbon_tings_last_hour(state: &AppState, world: &World, carbon: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} WHERE to_id = $1 AND ting_sent_at > now() - interval '1 hour'",
        world.t("wake_requests")
    ))
    .bind(carbon)
    .fetch_one(&state.pool)
    .await?)
}

/// The request whose Carbon Ting went (or is going) for this pair and Team in the last 15 minutes,
/// if any: a later ask's Ting is covered by it.
pub async fn covering(
    state: &AppState,
    world: &World,
    device_id: &str,
    team: &str,
    except: Option<Uuid>,
) -> AppResult<Option<Uuid>> {
    Ok(sqlx::query_scalar(sql!(
        "SELECT wake_id FROM {} WHERE device_id = $1 AND team = $2 AND ($3::uuid IS NULL OR wake_id <> $3)
           AND ((ting_delivery = 'delivered' AND ting_sent_at > now() - make_interval(secs => $4))
             OR (ting_delivery = 'pending' AND state = 'open'))
         ORDER BY COALESCE(ting_sent_at, last_asked_at) DESC LIMIT 1",
        world.t("wake_requests")
    ))
    .bind(device_id)
    .bind(team)
    .bind(except)
    .bind(WAKE_TING_EVERY_S as f64)
    .fetch_optional(&state.pool)
    .await?)
}

/// One attempt at a request's Carbon Ting, as the asking Silicon (`fresh` is the login it just
/// used; retries use the latest one Extend holds). The body is frozen at the first attempt of each
/// ask.
pub async fn send_carbon_ting(
    state: &AppState,
    world: &World,
    w: &WakeRow,
    fresh: Option<(Principal, Option<TestingSelection>)>,
) -> delivery::Attempt {
    let body = match &w.ting_body {
        Some(b) => b.clone(),
        None => match domain::load_device(state, world, &w.device_id).await {
            Ok(Some(pair)) => wake::requested_body(state, world, w, &pair).await,
            _ => return delivery::Attempt::default(),
        },
    };
    let mut chain = Vec::new();
    if let Some((p, sel)) = fresh {
        chain.push(Actor::Fresh(p, sel));
    }
    chain.push(Actor::Member(w.from_id.clone()));
    let attempt = delivery::send(state, world, &chain, &body).await;
    let counted = w.ting_attempts + i32::from(attempt.tried);
    let wait = if attempt.missing_type {
        delivery::MISSING_TYPE_RETRY
    } else {
        delivery::backoff(counted.max(1))
    };
    let _ = sqlx::query(sql!(
        "UPDATE {} SET ting_delivery = CASE WHEN $2 THEN 'delivered' ELSE 'pending' END,
                ting_sent_at = CASE WHEN $2 THEN now() ELSE ting_sent_at END,
                ting_body = COALESCE(ting_body, $3), ting_key = $3->>'key', ting_attempts = $4,
                ting_next_at = CASE WHEN $2 THEN NULL WHEN $5 THEN 'infinity'::timestamptz ELSE now() + $6 END,
                ting_last_error = CASE WHEN $2 THEN NULL ELSE $7 END
         WHERE wake_id = $1 AND state = 'open'",
        world.t("wake_requests")
    ))
    .bind(w.wake_id)
    .bind(attempt.delivered)
    .bind(&body)
    .bind(counted)
    .bind(attempt.not_registered)
    .bind(wait)
    .bind(&attempt.error)
    .execute(&state.pool)
    .await;
    attempt
}

/// Who holds a lock group: `(instance, session, Silicon, Team, the Carbon of the pair it runs through)`.
async fn group_sessions<'c>(
    db: impl sqlx::PgExecutor<'c>,
    world: &World,
    members: &[Uuid],
) -> AppResult<Vec<(Uuid, String, String, String, String)>> {
    Ok(sqlx::query_as(sql!(
        "SELECT l.instance_id, s.session_id, s.silicon_id, s.team, d.owner_id FROM {} l
           JOIN {} s ON s.session_id = l.session_id JOIN {} d ON d.device_id = s.device_id
         WHERE l.instance_id = ANY($1)",
        world.t("device_locks"),
        world.t("sessions"),
        world.t("devices")
    ))
    .bind(members)
    .fetch_all(db)
    .await?)
}

fn muted_error(d: &DeviceRow) -> AppError {
    AppError::new(
        ErrorCode::Conflict,
        format!("{} has turned off wake requests for {}.", d.owner_id, d.name),
    )
    .hint(format!(
        "Ask {} to turn them on again on the device's page, or wait until someone wakes it.",
        d.owner_id
    ))
}

pub async fn create(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Body(input): Body<WakeCreate>,
) -> AppResult<Response> {
    auth.require_silicon()?;
    let team = auth.team()?.to_owned();
    let (d, access) = domain::visible_device(&state, &auth.world, &device_id, &auth.p).await?;
    if access != Access::Silicon {
        return Err(AppError::new(
            ErrorCode::NoAccess,
            format!("{} has no access to {}.", auth.p.id(), d.name),
        )
        .hint(format!(
            "Ask {} (the Carbon who owns it) to grant access: extend device access grant {device_id} {} --team {team}",
            d.owner_id,
            auth.p.id()
        )));
    }
    crate::membership::owner_active(
        &state,
        &auth.world,
        &team,
        &d.owner_id,
        &d.name,
        &auth.p,
        auth.sel.as_ref(),
    )
    .await?;
    super::devices::check_reason(&input.reason)?;
    let grant_muted: Option<bool> = sqlx::query_scalar(sql!(
        "SELECT wake_muted FROM {} WHERE device_id = $1 AND team = $2 AND silicon_id = $3",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&team)
    .bind(auth.p.id())
    .fetch_optional(&state.pool)
    .await?;
    if d.wake_muted || grant_muted == Some(true) {
        return Err(muted_error(&d));
    }
    let hash = hash_json(&input);
    let (st, world, p, sel) = (state.clone(), auth.world.clone(), auth.p.clone(), auth.sel.clone());
    idempotent(
        &state,
        &auth.world,
        auth.p.id(),
        &format!("wake-requests:{team}:{device_id}"),
        &headers,
        &hash,
        || async move { ask(&st, &world, &d, &team, &p, sel, input.reason).await },
    )
    .await
}

/// A new ask, or a refresh of the caller's open request, in the lock order.
async fn ask(
    state: &Shared,
    world: &World,
    d: &DeviceRow,
    team: &str,
    p: &Principal,
    sel: Option<TestingSelection>,
    reason: String,
) -> AppResult<(StatusCode, &'static str, serde_json::Value)> {
    let me = p.id();
    let online = domain::is_online(state, world, d).await;
    let connected = state.hub.is_connected(&d.route(world)).await;
    let mut tx = state.pool.begin().await?;
    let group = domain::lock_group(&mut *tx, world, d.instance_id).await?;
    domain::lock_instances(&mut tx, world, &group.members).await?;
    // Another Silicon holds the device, or (for a computer) a device it carries.
    let holders = group_sessions(&mut *tx, world, &group.members).await?;
    for (instance, _, silicon, _, _) in &holders {
        let relevant = *instance == d.instance_id || (d.host_device_id.is_none() && group.host == d.instance_id);
        if relevant && silicon != me {
            drop(tx);
            let fresh = domain::load_device(state, world, &d.device_id)
                .await?
                .ok_or_else(|| domain::device_not_found(&d.device_id))?;
            return Err(if *instance == d.instance_id {
                super::sessions::in_use_error(&fresh, team, me)
            } else {
                super::sessions::group_in_use_error(&fresh, team)
            });
        }
    }
    let (awake, last_alert): (Option<bool>, Option<OffsetDateTime>) = sqlx::query_as(sql!(
        "SELECT awake, last_wake_alert_at FROM {} WHERE instance_id = $1",
        world.t("device_instances")
    ))
    .bind(d.instance_id)
    .fetch_one(&mut *tx)
    .await?;
    if online && awake == Some(true) {
        return Err(
            AppError::new(ErrorCode::Conflict, format!("{} is already awake.", d.name)).hint(format!(
                "Start using it: extend --team {team} session new {}",
                d.device_id
            )),
        );
    }
    let now = OffsetDateTime::now_utc();
    let open: Option<WakeRow> = sqlx::query_as(sql!(
        "SELECT {WAKE_COLUMNS} FROM {} WHERE instance_id = $1 AND team = $2 AND from_id = $3 AND state = 'open'",
        world.t("wake_requests")
    ))
    .bind(d.instance_id)
    .bind(team)
    .bind(me)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(w) = &open
        && w.device_id != d.device_id
    {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "You already asked to wake this device through {} (wake request {}); it is the same physical device.",
                w.device_id, w.wake_id
            ),
        )
        .hint(format!(
            "Wait for it, or withdraw it first: extend --team {team} device wake {} --cancel",
            w.device_id
        ))
        .details(serde_json::json!({"wake_request": w.view(false, None)})));
    }
    let too_soon = |at: OffsetDateTime, w: &WakeRow| {
        let wait = WAKE_ASK_AGAIN_AFTER_S - (now - at).whole_seconds();
        AppError::new(
            ErrorCode::RateLimited,
            format!(
                "You asked to wake {} less than 5 minutes ago. You can ask again in {wait} seconds.",
                d.name
            ),
        )
        .hint(format!(
            "Your request is still open until {}. Check it with: extend --team {team} device show {}",
            w.expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            d.device_id
        ))
        .details(serde_json::json!({"retry_after_s": wait.max(1), "wake_request": w.view(false, None)}))
    };
    if let Some(w) = &open
        && (now - w.last_asked_at).whole_seconds() < WAKE_ASK_AGAIN_AFTER_S
    {
        return Err(too_soon(w.last_asked_at, w));
    }
    if open.is_none() {
        let last: Option<WakeRow> = sqlx::query_as(sql!(
            "SELECT {WAKE_COLUMNS} FROM {} WHERE instance_id = $1 AND team = $2 AND from_id = $3
             ORDER BY last_asked_at DESC LIMIT 1",
            world.t("wake_requests")
        ))
        .bind(d.instance_id)
        .bind(team)
        .bind(me)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(w) = &last
            && w.state != "woken"
            && (now - w.last_asked_at).whole_seconds() < WAKE_ASK_AGAIN_AFTER_S
        {
            return Err(too_soon(w.last_asked_at, w));
        }
    }
    let app = if d.host_device_id.is_some() {
        d.host_app_version.as_deref()
    } else {
        d.app_version.as_deref()
    };
    let new_app = app.is_some_and(|v| super::enroll::version_at_least(v, "1.1.0"));
    let notice = if d.host_device_id.is_some() || !new_app {
        DeviceNotice::Unsupported
    } else if !connected {
        DeviceNotice::Offline
    } else {
        DeviceNotice::Sent
    };
    // The device sounds at most once every 15 minutes, whatever pair or Team asks.
    let alert = notice == DeviceNotice::Sent
        && last_alert.is_none_or(|t| (now - t).whole_seconds() >= extend_protocol::WAKE_ALERT_EVERY_S);
    if alert {
        sqlx::query(sql!(
            "UPDATE {} SET last_wake_alert_at = now() WHERE instance_id = $1",
            world.t("device_instances")
        ))
        .bind(d.instance_id)
        .execute(&mut *tx)
        .await?;
    }
    // One Carbon Ting per pair and Team every 15 minutes; at most 6 per Carbon an hour, beyond
    // which the ask is still accepted and its Ting waits.
    let still_pending = open
        .as_ref()
        .is_some_and(|w| matches!(w.ting_delivery.as_deref(), Some("pending" | "deferred")));
    let mut cover = if still_pending {
        None
    } else {
        covering(state, world, &d.device_id, team, None).await?
    };
    // A refresh whose own Ting went less than 15 minutes ago keeps it, and sends nothing new.
    let keep = still_pending || (open.is_some() && cover == open.as_ref().map(|w| w.wake_id));
    if keep {
        cover = open.as_ref().and_then(|w| w.ting_covered_by);
    }
    let ting: Option<&str> = if keep {
        open.as_ref().and_then(|w| w.ting_delivery.as_deref())
    } else if cover.is_some() {
        None
    } else if carbon_tings_last_hour(state, world, &d.owner_id).await?
        >= extend_protocol::WAKE_TINGS_PER_CARBON_PER_HOUR
    {
        Some("deferred")
    } else {
        Some("pending")
    };
    let expires = now + time::Duration::seconds(WAKE_REQUEST_TTL_S);
    let refreshed = open.is_some();
    let row: WakeRow = match &open {
        Some(w) => {
            sqlx::query_as(sql!(
                "UPDATE {} SET reason = $2, last_asked_at = $3, asks = asks + 1, expires_at = $4, device_notice = $5,
                        device_notice_note = NULL, wake_detectable = $6,
                        ting_delivery = $7, ting_covered_by = $8,
                        ting_body = CASE WHEN $9 THEN ting_body ELSE NULL END,
                        ting_next_at = CASE WHEN $9 THEN ting_next_at ELSE NULL END
                 WHERE wake_id = $1 RETURNING {WAKE_COLUMNS}",
                world.t("wake_requests")
            ))
            .bind(w.wake_id)
            .bind(&reason)
            .bind(now)
            .bind(expires)
            .bind(notice.as_str())
            .bind(d.wake_detectable())
            .bind(ting)
            .bind(cover)
            .bind(keep)
            .fetch_one(&mut *tx)
            .await?
        }
        None => {
            sqlx::query_as(sql!(
            "INSERT INTO {} (wake_id, device_id, instance_id, team, from_id, to_id, reason, created_at, last_asked_at,
                                 expires_at, wake_detectable, device_notice, ting_delivery, ting_covered_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $9, $10, $11, $12, $13) RETURNING {WAKE_COLUMNS}",
            world.t("wake_requests")
        ))
            .bind(Uuid::now_v7())
            .bind(&d.device_id)
            .bind(d.instance_id)
            .bind(team)
            .bind(me)
            .bind(&d.owner_id)
            .bind(&reason)
            .bind(now)
            .bind(expires)
            .bind(d.wake_detectable())
            .bind(notice.as_str())
            .bind(ting)
            .bind(cover)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    let f = wake::frame(state, world, &row, d, alert).await;
    let _ = state.hub.send(&d.route(world), f).await;
    // The woken and declined Tings go to the Silicon, which Ting delivers only once it registered.
    delivery::register_silicon(state, world, p, sel.as_ref());
    if row.ting_delivery.as_deref() == Some("pending") && !keep {
        send_carbon_ting(state, world, &row, Some((p.clone(), sel.clone()))).await;
    }
    domain::log_in(
        state,
        world,
        &d.device_id,
        &p.member,
        if refreshed { "wake_refreshed" } else { "wake_requested" },
        None,
        Some(team),
        serde_json::json!({"wake_id": row.wake_id, "reason": reason}),
    )
    .await;
    let row = wake::load(state, world, row.wake_id).await.unwrap_or(row);
    let view = row
        .view(false, wake::host_of(state, world, d).await)
        .ok_or_else(|| AppError::internal("wake request has an unreadable device id"))?;
    Ok((
        if refreshed { StatusCode::OK } else { StatusCode::CREATED },
        "wake_request",
        serde_json::to_value(view).map_err(AppError::internal)?,
    ))
}

#[derive(Deserialize)]
pub struct ListQuery {
    state: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

pub async fn list(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Query(q): Query<ListQuery>,
) -> AppResult<Response> {
    let (d, access) = domain::readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let owner = access == Access::Owner;
    let open_only = match q.state.as_deref() {
        None | Some("all") => false,
        Some("open") => true,
        Some(other) => return Err(AppError::invalid(format!("state must be open or all; got {other:?}."))),
    };
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "SELECT {WAKE_COLUMNS} FROM {} WHERE device_id = $1 AND ($2 OR (from_id = $3 AND team = $4))
           AND (NOT $5 OR state = 'open') AND ($6::uuid IS NULL OR wake_id < $6)
         ORDER BY wake_id DESC LIMIT $7",
        auth.world.t("wake_requests")
    ))
    .bind(&device_id)
    .bind(owner)
    .bind(auth.p.id())
    .bind(auth.p.team.as_deref().unwrap_or_default())
    .bind(open_only)
    .bind(before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let host = wake::host_of(&state, &auth.world, &d).await;
    let items: Vec<_> = rows
        .iter()
        .take(lim as usize)
        .filter_map(|r| r.view(owner, host.clone()))
        .collect();
    let next = more.then(|| encode_cursor(&items.last().map(|w| w.wake_id.to_string()).unwrap_or_default()));
    Ok(ok(
        "wake_requests",
        serde_json::json!({"items": items, "next_cursor": next}),
    ))
}

pub async fn cancel(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, wake_id)): Path<(String, String)>,
) -> AppResult<Response> {
    let not_found = || {
        AppError::new(
            ErrorCode::RequestNotFound,
            format!("No wake request {wake_id} of yours is on device {device_id}."),
        )
        .hint("List yours with `extend device show <device_id>`.")
    };
    let id: Uuid = wake_id.parse().map_err(|_| not_found())?;
    let w = wake::load(&state, &auth.world, id).await.ok_or_else(not_found)?;
    if !auth.p.is_silicon()
        || w.from_id != auth.p.id()
        || Some(w.team.as_str()) != auth.p.team.as_deref()
        || w.device_id != device_id
    {
        return Err(not_found());
    }
    if w.state != "open" {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "Wake request {id} already ended ({}).",
                w.end_reason.as_deref().unwrap_or(&w.state)
            ),
        ));
    }
    wake::withdraw(
        &state,
        &auth.world,
        Withdraw::One { wake_id: id },
        WakeEndReason::Cancelled,
    )
    .await;
    Ok(no_content())
}

pub async fn answer(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Body(input): Body<WakeAnswer>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let hash = hash_json(&input);
    let (st, world, p, sel) = (state.clone(), auth.world.clone(), auth.p.clone(), auth.sel.clone());
    idempotent(
        &state,
        &auth.world,
        auth.p.id(),
        &format!("wake-answer:{device_id}"),
        &headers,
        &hash,
        || async move {
            let open: Vec<WakeRow> = sqlx::query_as(sql!(
                "SELECT {WAKE_COLUMNS} FROM {} WHERE instance_id = $1 AND state = 'open'",
                world.t("wake_requests")
            ))
            .bind(d.instance_id)
            .fetch_all(&st.pool)
            .await?;
            let ended = match input.answer {
                WakeAnswerKind::Woken => {
                    if open.is_empty() {
                        return Err(AppError::new(
                            ErrorCode::Conflict,
                            format!("No wake request is open on {}.", d.name),
                        ));
                    }
                    wake::resolve_woken(
                        &st,
                        &world,
                        d.instance_id,
                        wake::WokenHow::Carbon {
                            carbon: Box::new(p.clone()),
                            sel: sel.clone(),
                            pair: d.device_id.clone(),
                        },
                    )
                    .await
                }
                WakeAnswerKind::Declined => {
                    let mine: Vec<&WakeRow> = open.iter().filter(|w| w.device_id == d.device_id).collect();
                    if let Some(ids) = &input.wake_ids
                        && let Some(bad) = ids.iter().find(|id| !mine.iter().any(|w| w.wake_id == **id))
                    {
                        return Err(AppError::invalid(format!(
                            "Wake request {bad} isn't open on {} ({}).",
                            d.name, d.device_id
                        ))
                        .hint("List the open ones with `extend device wake-requests ls <device_id>`."));
                    }
                    if mine.is_empty() {
                        return Err(AppError::new(
                            ErrorCode::Conflict,
                            format!("No wake request is open on your {}.", d.name),
                        ));
                    }
                    wake::decline(&st, &world, &d.device_id, input.wake_ids.as_deref(), &p, sel.as_ref()).await
                }
                WakeAnswerKind::Other => {
                    return Err(AppError::invalid("answer must be woken or declined."));
                }
            };
            // Only this Carbon's own pair's requests are listed; other Carbons' are never shown.
            let host = wake::host_of(&st, &world, &d).await;
            let mut views = Vec::new();
            for w in ended.iter().filter(|w| w.device_id == d.device_id) {
                let fresh = wake::load(&st, &world, w.wake_id).await.unwrap_or_else(|| w.clone());
                if let Some(v) = fresh.view(true, host.clone()) {
                    views.push(v);
                }
            }
            let answered = WakeAnswered::new(input.answer, views);
            Ok((
                StatusCode::OK,
                "wake_answer",
                serde_json::to_value(answered).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

pub async fn settings(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Body(input): Body<WakeSettings>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let world = &auth.world;
    match &input.silicon_id {
        None => {
            sqlx::query(sql!(
                "UPDATE {} SET wake_muted = $2 WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(&device_id)
            .bind(input.muted)
            .execute(&state.pool)
            .await?;
            if input.muted {
                wake::withdraw(
                    &state,
                    world,
                    Withdraw::Pair { device_id: &device_id },
                    WakeEndReason::Muted,
                )
                .await;
            }
        }
        Some(silicon) => {
            let changed = sqlx::query(sql!(
                "UPDATE {} SET wake_muted = $4 WHERE device_id = $1 AND silicon_id = $2 AND ($3::text IS NULL OR team = $3)",
                world.t("device_access")
            ))
            .bind(&device_id)
            .bind(silicon)
            .bind(&input.team)
            .bind(input.muted)
            .execute(&state.pool)
            .await?
            .rows_affected();
            if changed == 0 {
                return Err(AppError::invalid(format!(
                    "{silicon} has no access to {}{}.",
                    d.name,
                    input.team.as_deref().map(|t| format!(" in {t}")).unwrap_or_default()
                ))
                .hint(format!(
                    "See who has access with `extend device access ls {device_id}`."
                )));
            }
            if input.muted {
                wake::withdraw(
                    &state,
                    world,
                    Withdraw::Muted {
                        device_id: &device_id,
                        silicon_id: silicon,
                        team: input.team.as_deref(),
                    },
                    WakeEndReason::Muted,
                )
                .await;
            }
        }
    }
    domain::log(
        &state,
        world,
        &device_id,
        &auth.p.member,
        if input.muted { "wake_muted" } else { "wake_unmuted" },
        None,
        serde_json::json!({"silicon_id": input.silicon_id, "team": input.team}),
    )
    .await;
    Ok(ok("wake_settings", settings_view(&state, world, &device_id).await?))
}

async fn settings_view(state: &AppState, world: &World, device_id: &str) -> AppResult<WakeSettingsView> {
    let muted: bool = sqlx::query_scalar(sql!(
        "SELECT wake_muted FROM {} WHERE device_id = $1",
        world.t("devices")
    ))
    .bind(device_id)
    .fetch_one(&state.pool)
    .await?;
    let silicons: Vec<(String, String)> = sqlx::query_as(sql!(
        "SELECT silicon_id, team FROM {} WHERE device_id = $1 AND wake_muted ORDER BY team, silicon_id",
        world.t("device_access")
    ))
    .bind(device_id)
    .fetch_all(&state.pool)
    .await?;
    let mut view = WakeSettingsView::new(device_id.parse().map_err(AppError::internal)?, muted);
    view.silicons_muted = silicons.into_iter().map(|(s, t)| MutedSilicon::new(s, t)).collect();
    Ok(view)
}

// ───────────── Ting registration ─────────────

#[derive(Deserialize)]
pub struct TingQuery {
    team: Option<String>,
}

async fn registration(state: &AppState, world: &World, member: &str, team: &str) -> TingRegistration {
    let row = delivery::recipient(state, world, member, team).await;
    let mut r = TingRegistration::new(team, member, delivery::RecipientRow::status(row.as_ref()));
    if let Some(row) = &row {
        r.registered_at = row.registered_at;
        r.refused_at = row.refused_at;
        r.last_error = row.last_error.clone();
    }
    if r.status == TingStatus::Pending && r.last_error.is_none() {
        r.last_error = Some(format!("Sign in to Extend for {team}"));
    }
    r.missing_types = delivery::missing_types(state, world, team).await;
    r
}

/// Where the Carbon manages Ting types in `team`, registers Extend's missing types there with
/// their own login (Carbon decision, 2026-09-27). Where they don't, Ting refuses and the types stay
/// listed with the command for the Team's Ting manager.
async fn register_missing_types(
    state: &AppState,
    world: &World,
    p: &Principal,
    sel: Option<&TestingSelection>,
    team: &str,
) {
    if !p.is_carbon() {
        return;
    }
    let mut p = p.clone();
    p.team = Some(team.to_owned());
    for ty in delivery::missing_types(state, world, team).await {
        let Some(t) = extend_protocol::ting::find(&ty) else {
            continue;
        };
        match state.notifier.register_type(&p, t, sel).await {
            Ok(()) => {
                let _ = sqlx::query(sql!(
                    "DELETE FROM {} WHERE team = $1 AND ting_type = $2",
                    world.t("ting_type_status")
                ))
                .bind(team)
                .bind(&ty)
                .execute(&state.pool)
                .await;
                tracing::info!(world = %world.schema, team, ty, carbon = p.id(), "registered one of Extend's Ting types with the Carbon's login");
            }
            Err(e) => {
                tracing::debug!(team, ty, carbon = p.id(), error = %e.0.message, "the Carbon can't register Ting types in this Team")
            }
        }
    }
}

fn team_query(q: &TingQuery) -> AppResult<&str> {
    q.team
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AppError::invalid("Say which Team: ?team=<handle>, or ?team=any (Carbons)."))
}

fn unreachable_team(p: &Principal, team: &str) -> AppError {
    AppError::new(
        ErrorCode::NotATeamMember,
        format!("{}'s Extend login doesn't reach {team}.", p.id()),
    )
    .hint(format!(
        "Sign in to Extend again and select {team} (approve Extend for {team} in Silicon IAM), then retry."
    ))
}

pub async fn ting_get(State(state): State<Shared>, auth: Auth, Query(q): Query<TingQuery>) -> AppResult<Response> {
    let team = team_query(&q)?;
    let world = &auth.world;
    if team == "any" {
        auth.require_carbon()?;
        let mut teams: Vec<String> = auth.p.teams.clone();
        let granted: Vec<String> = sqlx::query_scalar(sql!(
            "SELECT DISTINCT team FROM {} WHERE granted_by = $1",
            world.t("device_access")
        ))
        .bind(auth.p.id())
        .fetch_all(&state.pool)
        .await?;
        teams.extend(granted);
        teams.sort();
        teams.dedup();
        let mut items = Vec::new();
        for t in teams {
            let reachable = auth.p.teams.contains(&t);
            if reachable {
                register_missing_types(&state, world, &auth.p, auth.sel.as_ref(), &t).await;
            }
            let mut r = registration(&state, world, auth.p.id(), &t).await;
            if !reachable {
                r.last_error = Some(format!("Sign in to Extend for {t}"));
            }
            items.push(r);
        }
        return Ok(ok("ting_registrations", serde_json::json!({"items": items})));
    }
    if !auth.p.teams.iter().any(|t| t == team) {
        return Err(unreachable_team(&auth.p, team));
    }
    register_missing_types(&state, world, &auth.p, auth.sel.as_ref(), team).await;
    Ok(ok(
        "ting_registration",
        registration(&state, world, auth.p.id(), team).await,
    ))
}

/// "Turn on": registers the caller with Ting in `team` again, with their own login for that Team.
pub async fn ting_turn_on(State(state): State<Shared>, auth: Auth, Query(q): Query<TingQuery>) -> AppResult<Response> {
    let team = team_query(&q)?.to_owned();
    if team == "any" || !auth.p.teams.contains(&team) {
        return Err(unreachable_team(&auth.p, &team));
    }
    let token = auth.p.token.clone();
    let mut p = state
        .authorize(&token, Some(&team), auth.sel.as_ref())
        .await
        .map_err(|_| unreachable_team(&auth.p, &team))?;
    p.team = Some(team.clone());
    delivery::register(&state, &auth.world, &p, auth.sel.as_ref(), true).await?;
    register_missing_types(&state, &auth.world, &p, auth.sel.as_ref(), &team).await;
    // The Tings that waited for this registration go now.
    let (st, world, member) = (state.clone(), auth.world.clone(), p.id().to_owned());
    tokio::spawn(async move {
        let Some(_fence) = st.world_open(&world).await else {
            return;
        };
        crate::scheduler::deliver_for(&st, &world, &member).await;
    });
    Ok(ok(
        "ting_registration",
        registration(&state, &auth.world, p.id(), &team).await,
    ))
}

/// The wake hint for a Silicon when the device is known not to be awake.
pub async fn hint_for(state: &AppState, world: &World, d: &DeviceRow, team: &str) -> Option<String> {
    let online = domain::is_online(state, world, d).await;
    wake::hint(d, online, team)
}
