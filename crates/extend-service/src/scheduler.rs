//! Background work: idle sessions, offline devices, pair expiry, pairing-code rotation, file
//! self-destruct, Ting retries for requests and wake requests, wake request expiry, and stale
//! enrollments.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use extend_protocol::SESSION_OFFLINE_GRACE_S;
use extend_protocol::frames::EnrollmentFrame;
use extend_protocol::model::EndReason;
use uuid::Uuid;

use crate::db::World;
use crate::domain;
use crate::state::{AppState, Shared};

/// How many counted attempts Extend makes to hand a request to Ting before marking it failed.
pub const REQUEST_ATTEMPTS: i32 = 6;
/// How long a request may wait for Ting before it is marked failed, whatever its attempts.
pub const REQUEST_GIVE_UP: time::Duration = time::Duration::hours(24);

pub fn spawn(state: Shared) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut n: u64 = 0;
        let mut deletions = Backoff::default();
        let world = World::production();
        loop {
            tick.tick().await;
            n += 1;
            if let Err(e) = rotate_codes(&state).await {
                tracing::warn!(error = %e, "rotating pairing codes failed");
            }
            if let Err(e) = sessions(&state, &world).await {
                tracing::warn!(error = %e, "session upkeep failed");
            }
            if n.is_multiple_of(15) {
                if let Err(e) = crate::telemetry::export(&state, &world).await {
                    tracing::warn!(error = %e, "telemetry export failed");
                }
                if let Err(e) = slow(&state, &world, &mut deletions).await {
                    tracing::warn!(error = %e, "device upkeep failed");
                }
            }
            if n.is_multiple_of(30) {
                let _ = sqlx::query(
                    "DELETE FROM extend_global.enrollments
                     WHERE (paired_device_id IS NULL AND last_seen_at < now() - interval '1 hour')
                        OR (paired_device_id IS NOT NULL AND last_seen_at < now() - interval '10 minutes')",
                )
                .execute(&state.pool)
                .await;
            }
        }
    });
}

/// Pushes a fresh code to every enrollment whose code expired while it's still waiting.
async fn rotate_codes(state: &AppState) -> crate::error::AppResult<()> {
    let due: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT enrollment_id FROM extend_global.enrollments WHERE code_expires_at <= now() AND paired_device_id IS NULL
           AND last_seen_at > now() - interval '15 minutes'",
    )
    .fetch_all(&state.pool)
    .await?;
    for (id,) in due {
        if !state.hub.enrollment_connected(id).await {
            continue;
        }
        if let Some((code, expires, true)) = crate::routes::enroll::rotate_if_due(state, id).await? {
            state
                .hub
                .send_enrollment(
                    id,
                    EnrollmentFrame::Code {
                        pairing_code: code,
                        code_expires_at: expires,
                    },
                )
                .await;
        }
    }
    Ok(())
}

async fn sessions(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    let actor = domain::system_member();
    // Idle (and takeovers that ran out). A command in flight pushes idle_ends_at past its deadline,
    // so a long command never counts as idle.
    let idle: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT session_id FROM {} WHERE state <> 'ended' AND idle_ends_at <= now()",
        world.t("sessions")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (sid,) in idle {
        domain::end_session(state, world, &sid, EndReason::IdleTimeout, &actor).await?;
    }
    // Devices that stayed offline during a session.
    let running: Vec<(String, String)> = sqlx::query_as(sql!(
        "SELECT session_id, device_id FROM {} WHERE state <> 'ended'",
        world.t("sessions")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (sid, device_id) in running {
        let Some(d) = domain::load_device(state, world, &device_id).await? else {
            continue;
        };
        if domain::is_online(state, world, &d).await {
            continue;
        }
        let seen = d.last_seen_at.unwrap_or(d.paired_at);
        if (domain::now() - seen).whole_seconds() > SESSION_OFFLINE_GRACE_S {
            domain::end_session(state, world, &sid, EndReason::DeviceOffline, &actor).await?;
        }
    }
    Ok(())
}

async fn slow(state: &AppState, world: &World, deletions: &mut Backoff) -> crate::error::AppResult<()> {
    let actor = domain::system_member();
    // Pairs that went unused for longer than their owner allowed.
    let expired: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT device_id FROM {} WHERE removed_at IS NULL AND last_activity_at + make_interval(days => pair_ttl_days) <= now()",
        world.t("devices")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (id,) in expired {
        tracing::info!(world = %world.schema, device_id = id, "pair expired after inactivity");
        domain::unpair(state, world, &id, EndReason::PairExpired, &actor).await?;
    }
    self_destruct(state, world, deletions).await?;
    retry_requests(state, world).await?;
    wake_upkeep(state, world).await?;
    // Uploads nobody claimed.
    let stale: Vec<(Uuid,)> = sqlx::query_as(sql!(
        "DELETE FROM {} WHERE expires_at < now() - interval '10 minutes' RETURNING upload_id",
        world.t("uploads")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (u,) in stale {
        let _ = tokio::fs::remove_file(state.cfg.data_dir.join("uploads").join(u.to_string())).await;
    }
    Ok(())
}

/// When to try failed file deletions again: 1 minute after the first failure, doubling up to an
/// hour. Kept in memory; after a restart every kept row is simply tried again.
#[derive(Debug)]
pub struct Backoff {
    first: Duration,
    max: Duration,
    next: HashMap<(String, Uuid), (u32, Instant)>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), Duration::from_secs(3600))
    }
}

impl Backoff {
    pub fn new(first: Duration, max: Duration) -> Self {
        Self {
            first,
            max,
            next: HashMap::new(),
        }
    }

    /// Files in `world` whose next try is still in the future.
    pub fn waiting(&self, world: &World) -> Vec<Uuid> {
        let now = Instant::now();
        self.next
            .iter()
            .filter(|((schema, _), (_, at))| schema == &world.schema && *at > now)
            .map(|((_, id), _)| *id)
            .collect()
    }

    /// Records a failed try and returns (tries so far, wait before the next).
    pub fn failed(&mut self, world: &World, id: Uuid) -> (u32, Duration) {
        let entry = self
            .next
            .entry((world.schema.clone(), id))
            .or_insert((0, Instant::now()));
        entry.0 += 1;
        let wait = self
            .first
            .saturating_mul(2u32.saturating_pow(entry.0 - 1))
            .min(self.max);
        entry.1 = Instant::now() + wait;
        (entry.0, wait)
    }

    pub fn succeeded(&mut self, world: &World, id: Uuid) {
        self.next.remove(&(world.schema.clone(), id));
    }

    /// Tries recorded so far for a file.
    pub fn tries(&self, world: &World, id: Uuid) -> u32 {
        self.next.get(&(world.schema.clone(), id)).map_or(0, |(n, _)| *n)
    }

    /// Forgets files that have not failed for a while (kept, deleted elsewhere, or cleaned away).
    fn prune(&mut self) {
        let stale = self.max * 2;
        self.next.retain(|_, (_, at)| at.elapsed() < stale);
    }
}

/// Deletes files whose self-destruct time passed, as the Silicon that made them, with the proof
/// Extend keeps for it (the Silicon need not be using Extend). A file's row stays until the store
/// confirms it is gone (Briefcase answering 404 counts), so a failed deletion is retried with
/// [`Backoff`] and never forgotten. Returns how many were deleted.
pub async fn self_destruct(state: &AppState, world: &World, backoff: &mut Backoff) -> crate::error::AppResult<usize> {
    backoff.prune();
    let waiting = backoff.waiting(world);
    let due: Vec<(Uuid, String)> = sqlx::query_as(sql!(
        "SELECT file_id, created_by FROM {} WHERE NOT permanent AND self_destruct_at <= now() AND NOT (file_id = ANY($1))
         ORDER BY self_destruct_at LIMIT 100",
        world.t("files")
    ))
    .bind(&waiting)
    .fetch_all(&state.pool)
    .await?;
    let mut deleted = 0;
    for (file_id, creator) in due {
        match state.files.destroy(&creator, file_id).await {
            Ok(()) => {
                sqlx::query(sql!("DELETE FROM {} WHERE file_id = $1", world.t("files")))
                    .bind(file_id)
                    .execute(&state.pool)
                    .await?;
                backoff.succeeded(world, file_id);
                deleted += 1;
                tracing::info!(file_id = %file_id, created_by = creator, "file self-destructed");
            }
            Err(e) => {
                let (tries, wait) = backoff.failed(world, file_id);
                tracing::warn!(
                    file_id = %file_id,
                    created_by = creator,
                    tries,
                    retry_in_s = wait.as_secs(),
                    error = %e.0.message,
                    hint = ?e.0.hint,
                    "self-destruct could not delete the file yet; its record stays and it is tried again"
                );
            }
        }
    }
    Ok(deleted)
}

/// Hands requests Ting hasn't accepted yet to Ting again, each with its frozen first body. Retries
/// back off (30 s, then 1, 2, 4 and 8 minutes, then every 8 minutes). A request is marked failed
/// after [`REQUEST_ATTEMPTS`] counted attempts or [`REQUEST_GIVE_UP`], with `last_error` saying why,
/// and at once while Ting is off.
pub async fn retry_requests(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    #[derive(sqlx::FromRow)]
    struct Row {
        request_id: Uuid,
        device_id: String,
        from_id: String,
        to_id: String,
        session_id: Option<String>,
        attempts: i32,
        created_at: time::OffsetDateTime,
        routed_to: String,
        ting_body: Option<serde_json::Value>,
    }
    let pending: Vec<Row> = sqlx::query_as(sql!(
        "SELECT r.request_id, r.device_id, r.from_id, r.to_id, r.session_id, r.attempts, r.created_at, r.routed_to, r.ting_body
         FROM {} r
         WHERE r.delivery = 'pending'
           AND (r.ting_next_at IS NULL OR r.ting_next_at <= now() OR r.created_at < now() - interval '24 hours')
         ORDER BY r.created_at LIMIT 200",
        world.t("requests")
    ))
    .fetch_all(&state.pool)
    .await?;
    for r in pending {
        let carbon = r.routed_to == "carbon";
        let attempt = match &r.ting_body {
            Some(body) => crate::delivery::send(state, world, body).await,
            // Requests first sent before 4.0 kept a body addressed to a Team; it can't be sent now.
            None => crate::delivery::Attempt {
                tried: true,
                error: Some("Not delivered: Extend moved to Silicon Accounts before it was delivered.".into()),
                disabled: true,
                ..crate::delivery::Attempt::default()
            },
        };
        let counted = r.attempts + i32::from(attempt.tried);
        let expired = domain::now() - r.created_at >= REQUEST_GIVE_UP;
        let (delivery, error) = if attempt.delivered {
            ("delivered", None)
        } else if attempt.disabled {
            ("failed", attempt.error.clone())
        } else if counted >= REQUEST_ATTEMPTS || expired {
            let why = attempt.error.clone().unwrap_or_default();
            (
                "failed",
                Some(format!(
                    "{why} Extend gave up after {counted} attempts{}; send the request again with `extend request send {} --reason \"...\"`.",
                    if expired { " and 24 hours" } else { "" },
                    r.device_id
                )),
            )
        } else {
            ("pending", attempt.error.clone())
        };
        let wait = if attempt.missing_type {
            crate::delivery::MISSING_TYPE_RETRY
        } else {
            crate::delivery::backoff(counted.max(1))
        };
        sqlx::query(sql!(
            "UPDATE {} SET delivery = $2, attempts = $3, last_error = $4,
                    ting_next_at = CASE WHEN $2 <> 'pending' THEN NULL
                                        WHEN $5 THEN 'infinity'::timestamptz
                                        ELSE now() + $6 END
             WHERE request_id = $1",
            world.t("requests")
        ))
        .bind(r.request_id)
        .bind(delivery)
        .bind(counted)
        .bind(&error)
        .bind(attempt.not_registered && delivery == "pending")
        .bind(wait)
        .execute(&state.pool)
        .await?;
        match delivery {
            "delivered" => tracing::info!(request_id = %r.request_id, "request delivered through Ting on retry"),
            "failed" => {
                tracing::warn!(request_id = %r.request_id, error = ?error, "request could not be delivered through Ting; marked failed");
                // Logged on the requester's pair, as the requester may see it.
                let from = state.accounts.directory.public_id(&r.from_id).await;
                let to = if carbon {
                    extend_protocol::REQUEST_TO_HIDDEN.to_owned()
                } else {
                    state.accounts.directory.public_id(&r.to_id).await
                };
                domain::log(
                    state,
                    world,
                    &r.device_id,
                    &domain::system_member(),
                    "request_failed",
                    (!carbon).then_some(r.session_id.as_deref()).flatten(),
                    serde_json::json!({"request_id": r.request_id, "from": from, "to": to, "error": error}),
                )
                .await;
            }
            _ => tracing::debug!(request_id = %r.request_id, error = ?error, "request still pending"),
        }
    }
    Ok(())
}

/// Wake requests: expire them, send Carbon Tings held back by the hourly limit once its window
/// frees (oldest first, if still open and not covered by then), and retry Carbon and answer Tings.
pub async fn wake_upkeep(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    crate::wake::expire(state, world).await?;
    // Deferred Carbon Tings, oldest first.
    let deferred: Vec<crate::wake::WakeRow> = sqlx::query_as(sql!(
        "SELECT {} FROM {} WHERE state = 'open' AND ting_delivery = 'deferred' ORDER BY last_asked_at LIMIT 100",
        crate::wake::WAKE_COLUMNS,
        world.t("wake_requests")
    ))
    .fetch_all(&state.pool)
    .await?;
    for w in deferred {
        if crate::routes::wake::carbon_tings_last_hour(state, world, &w.to_id).await?
            >= extend_protocol::WAKE_TINGS_PER_CARBON_PER_HOUR
        {
            continue;
        }
        if let Some(cover) = crate::routes::wake::covering(state, world, &w.device_id, Some(w.wake_id)).await? {
            sqlx::query(sql!(
                "UPDATE {} SET ting_delivery = NULL, ting_covered_by = $2 WHERE wake_id = $1",
                world.t("wake_requests")
            ))
            .bind(w.wake_id)
            .bind(cover)
            .execute(&state.pool)
            .await?;
            continue;
        }
        crate::routes::wake::send_carbon_ting(state, world, &w).await;
    }
    // Carbon Tings to retry.
    let due: Vec<crate::wake::WakeRow> = sqlx::query_as(sql!(
        "SELECT {} FROM {} WHERE state = 'open' AND ting_delivery = 'pending' AND (ting_next_at IS NULL OR ting_next_at <= now())
         ORDER BY last_asked_at LIMIT 100",
        crate::wake::WAKE_COLUMNS,
        world.t("wake_requests")
    ))
    .fetch_all(&state.pool)
    .await?;
    for w in due {
        crate::routes::wake::send_carbon_ting(state, world, &w).await;
    }
    // Answer Tings (woken, declined) to retry; they give up 30 minutes after the request ended.
    let answers: Vec<crate::wake::WakeRow> = sqlx::query_as(sql!(
        "SELECT {} FROM {} WHERE answer_ting = 'pending' AND (answer_ting_next_at IS NULL OR answer_ting_next_at <= now())
         ORDER BY ended_at LIMIT 100",
        crate::wake::WAKE_COLUMNS,
        world.t("wake_requests")
    ))
    .fetch_all(&state.pool)
    .await?;
    for w in answers {
        let body = match w.answer_ting_body.clone() {
            Some(b) => b,
            None if w.state == "declined" => {
                let carbon = state.accounts.directory.public_id(&w.to_id).await;
                crate::wake::declined_body(state, world, &w, &carbon).await
            }
            None => {
                let by = if w.end_reason.as_deref() == Some("confirmed_by_carbon") {
                    extend_protocol::ting::WokenBy::Carbon
                } else {
                    extend_protocol::ting::WokenBy::Device
                };
                crate::wake::woken_body(state, world, &w, by).await
            }
        };
        let attempt = crate::delivery::send(state, world, &body).await;
        crate::wake::record_answer(state, world, &w, &body, &attempt).await;
    }
    Ok(())
}
