//! Wake requests: a Silicon asks the Carbon who gave it access to wake a device (UNDERSTANDING.md,
//! Waking a device). Extend never wakes a device itself; awake is information plus this flow, never
//! a gate.
//!
//! - One open request per (physical device, Silicon). Asking again after 5 minutes refreshes it;
//!   it expires 30 minutes after the last ask.
//! - The device shows it where it can (a `wake_request` frame on the pair it was made through; a
//!   carried device's host only probes). The frame leaves out the Silicon and the reason while
//!   another side holds the device or its lock group, and carries the request's side tag so the app
//!   can hide it before a session of another side runs its first command.
//! - The Carbon who gave access gets `wake_requested` through Ting (when Ting is on).
//! - When the device wakes with evidence (or a Carbon answers "It's awake", a fact about the whole
//!   device), every open request on it ends, and each asking Silicon gets its own `woken` Ting,
//!   naming no one. "Decline" answers only the Carbon's own pair.

use extend_protocol::frames::{ServiceFrame, WakeEnd};
use extend_protocol::model::{DeviceNotice, HostDevice, TingDelivery, WakeEndReason, WakeRequest, WakeState};
use extend_protocol::ting::{WakeNow, WokenBy};
use serde_json::json;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::accounts::Principal;
use crate::db::World;
use crate::delivery;
use crate::domain::{self, Access, DeviceRow, Viewer};
use crate::state::AppState;

#[derive(Debug, Clone, FromRow)]
pub struct WakeRow {
    pub wake_id: Uuid,
    pub device_id: String,
    pub instance_id: Uuid,
    pub from_id: String,
    pub to_id: String,
    pub reason: String,
    pub created_at: OffsetDateTime,
    pub last_asked_at: OffsetDateTime,
    pub asks: i32,
    pub expires_at: OffsetDateTime,
    pub state: String,
    pub ended_at: Option<OffsetDateTime>,
    pub end_reason: Option<String>,
    pub wake_detectable: bool,
    pub device_notice: String,
    pub device_notice_note: Option<String>,
    pub ting_delivery: Option<String>,
    pub ting_covered_by: Option<Uuid>,
    pub ting_attempts: i32,
    pub ting_last_error: Option<String>,
    pub ting_sent_at: Option<OffsetDateTime>,
    pub ting_body: Option<serde_json::Value>,
    pub answer_ting: Option<String>,
    pub answer_ting_body: Option<serde_json::Value>,
    pub answer_ting_attempts: i32,
    pub answer_ting_last_error: Option<String>,
}

pub const WAKE_COLUMNS: &str = "wake_id, device_id, instance_id, from_id, to_id, reason, created_at, last_asked_at, asks,
    expires_at, state, ended_at, end_reason, wake_detectable, device_notice, device_notice_note, ting_delivery, ting_covered_by,
    ting_attempts, ting_last_error, ting_sent_at, ting_body, answer_ting, answer_ting_body, answer_ting_attempts, answer_ting_last_error";

impl WakeRow {
    /// The request as the pair's owner sees it (`owner`), or as the asking Silicon does: a Silicon
    /// sees a deferred Ting as pending, so other Silicons' asks stay invisible to it. `from` and
    /// `to` are uuids here; [`WakeRow::view_for`] shows current ids.
    pub fn view(&self, owner: bool, host: Option<HostDevice>) -> Option<WakeRequest> {
        let mut w = WakeRequest::new(
            self.wake_id,
            self.device_id.parse().ok()?,
            String::new(),
            self.from_id.clone(),
            self.to_id.clone(),
            self.reason.clone(),
            self.created_at,
            self.expires_at,
        );
        w.last_asked_at = self.last_asked_at;
        w.asks = i64::from(self.asks);
        w.state = WakeState::parse(&self.state);
        w.ended_at = self.ended_at;
        w.end_reason = self.end_reason.as_deref().map(WakeEndReason::parse);
        w.wake_detectable = self.wake_detectable;
        w.device_notice = DeviceNotice::parse(&self.device_notice);
        w.device_notice_note = self.device_notice_note.clone();
        w.ting = match (self.ting_delivery.as_deref(), self.ting_covered_by) {
            (None, Some(_)) => Some(TingDelivery::Covered),
            (None, None) => None,
            (Some("deferred"), _) if !owner => Some(TingDelivery::Pending),
            (Some(t), _) => Some(TingDelivery::parse(t)),
        };
        w.ting_covered_by = self.ting_covered_by;
        w.ting_last_error = self.ting_last_error.clone();
        w.answer_ting = self.answer_ting.as_deref().map(TingDelivery::parse);
        w.answer_ting_last_error = self.answer_ting_last_error.clone();
        w.host = host;
        w.from_uuid = Some(self.from_id.clone());
        w.to_uuid = Some(self.to_id.clone());
        Some(w)
    }

    /// [`WakeRow::view`] with the Silicon and the Carbon shown by their current public ids.
    pub async fn view_for(&self, state: &AppState, owner: bool, host: Option<HostDevice>) -> Option<WakeRequest> {
        let mut w = self.view(owner, host)?;
        w.from = state.accounts.directory.public_id(&self.from_id).await;
        w.to = state.accounts.directory.public_id(&self.to_id).await;
        Some(w)
    }
}

/// The computer a carried pair goes through (the same Carbon's pair of it).
pub async fn host_of(state: &AppState, world: &World, d: &DeviceRow) -> Option<HostDevice> {
    let host = d.host_device_id.as_deref()?;
    let h = domain::load_device(state, world, host).await.ok()??;
    let online = domain::is_online(state, world, &h).await;
    Some(HostDevice::new(h.device_id.parse().ok()?, h.name.clone(), online))
}

pub async fn load(state: &AppState, world: &World, wake_id: Uuid) -> Option<WakeRow> {
    sqlx::query_as(sql!(
        "SELECT {WAKE_COLUMNS} FROM {} WHERE wake_id = $1",
        world.t("wake_requests")
    ))
    .bind(wake_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
}

/// The open requests made through a pair that `viewer` may see: all of them for the owner, its
/// own for a Silicon.
pub async fn open_views(state: &AppState, world: &World, d: &DeviceRow, viewer: Viewer<'_>) -> Vec<WakeRequest> {
    let owner = viewer.access == Access::Owner;
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "SELECT {WAKE_COLUMNS} FROM {} WHERE device_id = $1 AND state = 'open' AND ($2 OR from_id = $3) ORDER BY created_at",
        world.t("wake_requests")
    ))
    .bind(&d.device_id)
    .bind(owner)
    .bind(viewer.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let host = host_of(state, world, d).await;
    let mut out = Vec::new();
    for r in &rows {
        if let Some(v) = r.view_for(state, owner, host.clone()).await {
            out.push(v);
        }
    }
    out
}

// ───────────── Frames to the device ─────────────

/// Who holds a lock group, by side: the Carbon of the pair each session runs through.
async fn group_holders(state: &AppState, world: &World, members: &[Uuid]) -> Vec<String> {
    sqlx::query_scalar(sql!(
        "SELECT d.owner_id FROM {} l JOIN {} s ON s.session_id = l.session_id JOIN {} d ON d.device_id = s.device_id
         WHERE l.instance_id = ANY($1)",
        world.t("device_locks"),
        world.t("sessions"),
        world.t("devices")
    ))
    .bind(members)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
}

/// The `wake_request` frame for a request on pair `pair`. It carries the request's side tag, and
/// leaves out the Silicon and the reason for a carried device (its host only probes) and while
/// another side holds the device or anything in its lock group.
pub async fn frame(state: &AppState, world: &World, w: &WakeRow, pair: &DeviceRow, alert: bool) -> ServiceFrame {
    let group = domain::lock_group(&state.pool, world, pair.instance_id)
        .await
        .map(|g| g.members)
        .unwrap_or_else(|_| vec![pair.instance_id]);
    let holders = group_holders(state, world, &group).await;
    let other_side_holds = holders.iter().any(|carbon| *carbon != pair.owner_id);
    let target = pair.host_device_id.as_ref().and_then(|_| pair.device_id.parse().ok());
    let redacted = target.is_some() || other_side_holds;
    let side = domain::side_of(state, world, pair).await.ok();
    let silicon_id = if redacted {
        None
    } else {
        Some(state.accounts.directory.public_id(&w.from_id).await)
    };
    ServiceFrame::WakeRequest {
        target,
        wake_id: w.wake_id,
        silicon_id,
        reason: (!redacted).then(|| w.reason.clone()),
        side,
        alert,
        created_at: w.created_at,
        expires_at: w.expires_at,
    }
}

/// Sends every open request's frame again to its pair, across a lock group, so what the devices
/// show follows the side holding the group (after a session starts or ends).
pub async fn resend_group(state: &AppState, world: &World, instance: Uuid) {
    let Ok(group) = domain::lock_group(&state.pool, world, instance).await else {
        return;
    };
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "SELECT {WAKE_COLUMNS} FROM {} WHERE instance_id = ANY($1) AND state = 'open'",
        world.t("wake_requests")
    ))
    .bind(&group.members)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for w in rows {
        if let Ok(Some(pair)) = domain::load_device(state, world, &w.device_id).await {
            let f = frame(state, world, &w, &pair, false).await;
            let _ = state.hub.send(&pair.route(world), f).await;
        }
    }
}

/// The open requests a pair's connection is told about when it connects: its own, and those of
/// the devices it carries.
pub async fn greeting(state: &AppState, world: &World, device_id: &str) -> Vec<ServiceFrame> {
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "SELECT {WAKE_COLUMNS} FROM {w} WHERE state = 'open' AND device_id IN (
             SELECT device_id FROM {d} WHERE (device_id = $1 OR host_device_id = $1) AND removed_at IS NULL)
         ORDER BY created_at",
        w = world.t("wake_requests"),
        d = world.t("devices")
    ))
    .bind(device_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    for w in rows {
        if let Ok(Some(pair)) = domain::load_device(state, world, &w.device_id).await {
            out.push(frame(state, world, &w, &pair, false).await);
        }
    }
    out
}

fn wake_end(state: &str) -> WakeEnd {
    match state {
        "woken" => WakeEnd::Woken,
        "expired" => WakeEnd::Expired,
        "declined" => WakeEnd::Declined,
        _ => WakeEnd::Withdrawn,
    }
}

async fn send_ended(state: &AppState, world: &World, w: &WakeRow) {
    if let Ok(Some(pair)) = domain::load_device(state, world, &w.device_id).await {
        let target = pair.host_device_id.as_ref().and_then(|_| pair.device_id.parse().ok());
        let _ = state
            .hub
            .send(
                &pair.route(world),
                ServiceFrame::WakeRequestEnded {
                    target,
                    wake_id: w.wake_id,
                    reason: wake_end(&w.state),
                },
            )
            .await;
    }
}

// ───────────── Ending requests ─────────────

/// Which open requests [`withdraw`] ends.
#[derive(Debug, Clone, Copy)]
pub enum Withdraw<'a> {
    /// A Silicon's requests through one pair (its grant ended).
    Asker { device_id: &'a str, silicon_id: &'a str },
    /// Every request through one pair (the pair ended, or its Carbon turned wake requests off).
    Pair { device_id: &'a str },
    /// One Silicon's requests through one pair (its Carbon muted it).
    Muted { device_id: &'a str, silicon_id: &'a str },
    /// Every request of a Silicon (it signed out, removed Extend's access, or was deleted).
    Silicon { silicon_id: &'a str },
    /// A Silicon's request on a physical device (it started a session there).
    Session { instance: Uuid, silicon_id: &'a str },
    /// One request (the Silicon, or its custodian, cancelled it).
    One { wake_id: Uuid },
}

/// Ends open requests as withdrawn: the device forgets them, a Carbon Ting that hadn't gone
/// becomes failed ("the request ended before delivery"), and each pair's log says so.
pub async fn withdraw(state: &AppState, world: &World, which: Withdraw<'_>, reason: WakeEndReason) -> Vec<WakeRow> {
    let (cond, a, b, c, i): (&str, Option<&str>, Option<&str>, Option<&str>, Option<Uuid>) = match which {
        Withdraw::Asker { device_id, silicon_id } | Withdraw::Muted { device_id, silicon_id } => (
            "device_id = $1 AND from_id = $2",
            Some(device_id),
            Some(silicon_id),
            None,
            None,
        ),
        Withdraw::Pair { device_id } => ("device_id = $1", Some(device_id), None, None, None),
        Withdraw::Silicon { silicon_id } => ("from_id = $1", Some(silicon_id), None, None, None),
        Withdraw::Session { instance, silicon_id } => (
            "instance_id = $4 AND from_id = $1",
            Some(silicon_id),
            None,
            None,
            Some(instance),
        ),
        Withdraw::One { wake_id } => ("wake_id = $4", None, None, None, Some(wake_id)),
    };
    // Every parameter is mentioned (the always-true terms) so PostgreSQL can type the ones `cond`
    // doesn't use.
    let rows: Vec<WakeRow> = match sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE {} SET state = 'withdrawn', ended_at = now(), end_reason = $5,
                ting_delivery = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'failed' ELSE ting_delivery END,
                ting_last_error = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'The request ended before delivery.'
                                       ELSE ting_last_error END,
                ting_next_at = NULL
         WHERE state = 'open' AND ($1::text IS NULL OR $1 IS NOT NULL) AND ($2::text IS NULL OR $2 IS NOT NULL)
           AND ($3::text IS NULL OR $3 IS NOT NULL) AND ($4::uuid IS NULL OR $4 IS NOT NULL) AND {cond}
         RETURNING {WAKE_COLUMNS}",
        world.t("wake_requests")
    )))
    .bind(a)
    .bind(b)
    .bind(c)
    .bind(i)
    .bind(reason.as_str())
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(world = %world.schema, error = %e, "withdrawing wake requests failed");
            return vec![];
        }
    };
    let action = match reason {
        WakeEndReason::Cancelled => "wake_cancelled",
        _ => "wake_withdrawn",
    };
    for w in &rows {
        send_ended(state, world, w).await;
        let actor = if reason == WakeEndReason::Cancelled {
            crate::accounts::actor(extend_protocol::model::MemberKind::Silicon, &w.from_id)
        } else {
            domain::system_member()
        };
        let from = state.accounts.directory.public_id(&w.from_id).await;
        domain::log(
            state,
            world,
            &w.device_id,
            &actor,
            action,
            None,
            json!({"wake_id": w.wake_id, "from": from, "from_uuid": w.from_id, "reason": reason.as_str()}),
        )
        .await;
    }
    rows
}

/// Expires open requests whose time ran out.
pub async fn expire(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'expired', ended_at = now(), end_reason = 'expired',
                ting_delivery = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'failed' ELSE ting_delivery END,
                ting_last_error = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'The request ended before delivery.'
                                       ELSE ting_last_error END,
                ting_next_at = NULL
         WHERE state = 'open' AND expires_at <= now() RETURNING {WAKE_COLUMNS}",
        world.t("wake_requests")
    ))
    .fetch_all(&state.pool)
    .await?;
    for w in &rows {
        send_ended(state, world, w).await;
        let from = state.accounts.directory.public_id(&w.from_id).await;
        domain::log(
            state,
            world,
            &w.device_id,
            &domain::system_member(),
            "wake_expired",
            None,
            json!({"wake_id": w.wake_id, "from": from, "from_uuid": w.from_id}),
        )
        .await;
    }
    Ok(())
}

/// How the device came to be awake, for [`resolve_woken`].
#[derive(Debug, Clone)]
pub enum WokenHow {
    /// The device reported itself awake with an unlock or real input.
    Device,
    /// A Carbon who paired the device answered "It's awake" (on their pair).
    Carbon { carbon: Box<Principal>, pair: String },
}

/// Ends every open request on a physical device as woken, in every pair, and sends each asking
/// Silicon its own `woken` Ting. Returns the ended rows.
pub async fn resolve_woken(state: &AppState, world: &World, instance: Uuid, how: WokenHow) -> Vec<WakeRow> {
    let reason = match how {
        WokenHow::Device => WakeEndReason::WokenOnDevice,
        WokenHow::Carbon { .. } => WakeEndReason::ConfirmedByCarbon,
    };
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'woken', ended_at = now(), end_reason = $2, answer_ting = 'pending', answer_ting_next_at = now(),
                ting_delivery = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'failed' ELSE ting_delivery END,
                ting_last_error = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'The request ended before delivery.'
                                       ELSE ting_last_error END,
                ting_next_at = NULL
         WHERE instance_id = $1 AND state = 'open' RETURNING {WAKE_COLUMNS}",
        world.t("wake_requests")
    ))
    .bind(instance)
    .bind(reason.as_str())
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for w in &rows {
        send_ended(state, world, w).await;
        let (action, actor, by) = match &how {
            WokenHow::Device => ("woken", domain::system_member(), WokenBy::Device),
            // Only the answering Carbon's own pair names them; other pairs' logs say Extend.
            WokenHow::Carbon { carbon, pair } if *pair == w.device_id => {
                ("wake_confirmed", carbon.actor(), WokenBy::Carbon)
            }
            WokenHow::Carbon { .. } => ("wake_confirmed", domain::system_member(), WokenBy::Carbon),
        };
        let from = state.accounts.directory.public_id(&w.from_id).await;
        domain::log(
            state,
            world,
            &w.device_id,
            &actor,
            action,
            None,
            json!({"wake_id": w.wake_id, "from": from, "from_uuid": w.from_id, "woken_by": by.as_str()}),
        )
        .await;
        let body = woken_body(state, world, w, by).await;
        answer_attempt(state, world, w, body).await;
    }
    rows
}

/// Ends the given open requests of one pair as declined, and tells each asking Silicon.
pub async fn decline(
    state: &AppState,
    world: &World,
    device_id: &str,
    wake_ids: Option<&[Uuid]>,
    carbon: &Principal,
) -> Vec<WakeRow> {
    let rows: Vec<WakeRow> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'declined', ended_at = now(), end_reason = 'declined', answer_ting = 'pending', answer_ting_next_at = now(),
                ting_delivery = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'failed' ELSE ting_delivery END,
                ting_next_at = NULL
         WHERE device_id = $1 AND state = 'open' AND ($2::uuid[] IS NULL OR wake_id = ANY($2)) RETURNING {WAKE_COLUMNS}",
        world.t("wake_requests")
    ))
    .bind(device_id)
    .bind(wake_ids)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for w in &rows {
        send_ended(state, world, w).await;
        let from = state.accounts.directory.public_id(&w.from_id).await;
        domain::log(
            state,
            world,
            &w.device_id,
            &carbon.actor(),
            "wake_declined",
            None,
            json!({"wake_id": w.wake_id, "from": from, "from_uuid": w.from_id}),
        )
        .await;
        let body = declined_body(state, world, w, carbon.public_id()).await;
        answer_attempt(state, world, w, body).await;
    }
    rows
}

/// One attempt at a request's answer Ting (woken or declined), with the body frozen on the row.
async fn answer_attempt(state: &AppState, world: &World, w: &WakeRow, body: serde_json::Value) {
    let attempt = delivery::send(state, world, &body).await;
    record_answer(state, world, w, &body, &attempt).await;
}

/// Records an answer Ting's attempt: delivered; stopped until the Silicon registers; or retried
/// with backoff. It gives up 30 minutes after the request ended.
pub async fn record_answer(
    state: &AppState,
    world: &World,
    w: &WakeRow,
    body: &serde_json::Value,
    attempt: &delivery::Attempt,
) {
    let counted = w.answer_ting_attempts + i32::from(attempt.tried);
    let wait = if attempt.missing_type {
        delivery::MISSING_TYPE_RETRY
    } else {
        delivery::backoff(counted.max(1))
    };
    let _ = sqlx::query(sql!(
        "UPDATE {} SET answer_ting = CASE WHEN $2 THEN 'delivered' WHEN $8 THEN 'failed'
                                         WHEN ended_at < now() - interval '30 minutes' THEN 'failed' ELSE 'pending' END,
                answer_ting_body = COALESCE(answer_ting_body, $3),
                answer_ting_attempts = $4,
                answer_ting_next_at = CASE WHEN $2 OR $8 THEN NULL WHEN $5 THEN 'infinity'::timestamptz ELSE now() + $6 END,
                answer_ting_last_error = CASE WHEN $2 THEN NULL ELSE $7 END
         WHERE wake_id = $1",
        world.t("wake_requests")
    ))
    .bind(w.wake_id)
    .bind(attempt.delivered)
    .bind(body)
    .bind(counted)
    .bind(attempt.not_registered)
    .bind(wait)
    .bind(&attempt.error)
    .bind(attempt.disabled)
    .execute(&state.pool)
    .await;
}

// ───────────── Ting bodies ─────────────

fn link(state: &AppState, device_id: &str) -> String {
    format!("{}/devices/{device_id}", state.cfg.website_url.trim_end_matches('/'))
}

fn stamp(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// `wake_requested`, to the Carbon who gave the Silicon access.
pub async fn requested_body(state: &AppState, world: &World, w: &WakeRow, pair: &DeviceRow) -> serde_json::Value {
    let host = host_of(state, world, pair).await;
    let from = state.accounts.directory.public_id(&w.from_id).await;
    let to = state.accounts.directory.public_id(&w.to_id).await;
    let mut summary = format!(
        "{from} asks you to wake {} ({}): {}.",
        pair.name,
        pair.device_id,
        w.reason.trim_end_matches('.')
    );
    if let Some(h) = &host {
        summary.push_str(&format!(" {}, which it pairs through, must be awake too.", h.name));
    }
    if !w.wake_detectable {
        summary.push_str(" Extend can't tell when it wakes; say so with the answer command or on the website.");
    }
    let data = json!({
        "wake_id": w.wake_id,
        "device_id": pair.device_id,
        "device_name": pair.name,
        "device_os": pair.os().as_str(),
        "from": from,
        "from_uuid": w.from_id,
        "reason": w.reason,
        "created_at": stamp(w.created_at),
        "expires_at": stamp(w.expires_at),
        "device_notice": w.device_notice,
        "wake_detectable": w.wake_detectable,
        "host": host,
        "summary": summary,
        "link": link(state, &pair.device_id),
        "answer": format!("extend device wake-requests answer {} woken|declined", pair.device_id),
    });
    crate::ting::body(
        state.notifier.app_id(),
        crate::ting::WAKE_REQUESTED,
        &w.to_id,
        &to,
        &extend_protocol::ting::wake_requested_key(w.wake_id, i64::from(w.asks)),
        data,
    )
}

/// What the asking Silicon can do now, from its own view of the device: never who holds it.
async fn now_for(state: &AppState, world: &World, w: &WakeRow) -> (WakeNow, String) {
    let held: Option<(String, String)> = sqlx::query_as(sql!(
        "SELECT s.session_id, s.silicon_id FROM {} l JOIN {} s ON s.session_id = l.session_id WHERE l.instance_id = $1",
        world.t("device_locks"),
        world.t("sessions")
    ))
    .bind(w.instance_id)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);
    match held {
        None => (WakeNow::Free, format!("extend session new {}", w.device_id)),
        Some((sid, silicon)) if silicon == w.from_id => (WakeNow::Yours, format!("extend session connect {sid}")),
        Some(_) => (
            WakeNow::InUse,
            format!("extend request send {} --reason \"...\"", w.device_id),
        ),
    }
}

/// `woken`, to the asking Silicon. It never names another Silicon or Carbon, nor which Carbon
/// confirmed.
pub async fn woken_body(state: &AppState, world: &World, w: &WakeRow, by: WokenBy) -> serde_json::Value {
    let name = device_name(state, world, &w.device_id).await;
    let (now, next) = now_for(state, world, w).await;
    let summary = match now {
        WakeNow::Free => format!("{name} ({}) is awake. Start using it: {next}", w.device_id),
        WakeNow::Yours => format!("{name} ({}) is awake. Your session is still on it: {next}", w.device_id),
        _ => format!(
            "{name} ({}) is awake, but another Silicon is using it now. Ask for it: {next}",
            w.device_id
        ),
    };
    let data = json!({
        "wake_id": w.wake_id,
        "device_id": w.device_id,
        "device_name": name,
        "woken_at": stamp(w.ended_at.unwrap_or_else(OffsetDateTime::now_utc)),
        "woken_by": by.as_str(),
        "now": now.as_str(),
        "next": next,
        "summary": summary,
    });
    let from = state.accounts.directory.public_id(&w.from_id).await;
    crate::ting::body(
        state.notifier.app_id(),
        crate::ting::WOKEN,
        &w.from_id,
        &from,
        &extend_protocol::ting::woken_key(w.wake_id),
        data,
    )
}

/// `wake_declined`, to the asking Silicon. It names the Silicon's own Carbon.
pub async fn declined_body(state: &AppState, world: &World, w: &WakeRow, carbon: &str) -> serde_json::Value {
    let name = device_name(state, world, &w.device_id).await;
    let data = json!({
        "wake_id": w.wake_id,
        "device_id": w.device_id,
        "device_name": name,
        "declined_at": stamp(w.ended_at.unwrap_or_else(OffsetDateTime::now_utc)),
        "summary": format!("{carbon} turned down your request to wake {name} ({}).", w.device_id),
    });
    let from = state.accounts.directory.public_id(&w.from_id).await;
    crate::ting::body(
        state.notifier.app_id(),
        crate::ting::WAKE_DECLINED,
        &w.from_id,
        &from,
        &extend_protocol::ting::declined_key(w.wake_id),
        data,
    )
}

async fn device_name(state: &AppState, world: &World, device_id: &str) -> String {
    sqlx::query_scalar(sql!("SELECT name FROM {} WHERE device_id = $1", world.t("devices")))
        .bind(device_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| device_id.to_owned())
}

/// The hint for a Silicon when a device is known not to be awake. It never refuses anything by
/// itself.
pub fn hint(d: &DeviceRow, online: bool) -> Option<String> {
    let state = match (online, d.awake, d.sleep()) {
        (true, Some(false), s) => s,
        (false, _, Some(s)) => Some(s),
        _ => return None,
    };
    let what = state.map_or("not awake", |s| s.label());
    let since = d
        .awake_changed_at
        .map(|t| format!(" since {}", stamp(t)))
        .unwrap_or_default();
    Some(format!(
        "{} isn't awake ({what}{since}). Its Carbon can wake it: extend device wake {} --reason \"...\"",
        d.name, d.device_id
    ))
}
