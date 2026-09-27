//! Background work: idle sessions, offline devices, pair expiry, pairing-code rotation, file
//! self-destruct, request delivery retries and stale enrollments.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use extend_protocol::SESSION_OFFLINE_GRACE_S;
use extend_protocol::frames::EnrollmentFrame;
use extend_protocol::model::{EndReason, Member, MemberKind};
use uuid::Uuid;

use crate::db::World;
use crate::domain;
use crate::iam::{Principal, TestingSelection};
use crate::state::{AppState, Shared};

/// How many times Extend tries to hand a request to Ting before marking it failed.
pub const REQUEST_ATTEMPTS: i32 = 6;

pub fn spawn(state: Shared) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut n: u64 = 0;
        let mut deletions = Backoff::default();
        loop {
            tick.tick().await;
            n += 1;
            if let Err(e) = rotate_codes(&state).await {
                tracing::warn!(error = %e, "rotating pairing codes failed");
            }
            for world in worlds(&state).await {
                // A test world that is being cleaned, disabled or removed is left alone, and
                // holding its fence keeps such a change from starting while this pass writes.
                let Some(_fence) = state.world_open(&world).await else {
                    continue;
                };
                if let Err(e) = sessions(&state, &world).await {
                    tracing::warn!(world = %world.schema, error = %e, "session upkeep failed");
                }
                if n.is_multiple_of(15) {
                    if let Err(e) = crate::telemetry::export(&state, &world).await {
                        tracing::warn!(world = %world.schema, error = %e, "telemetry export failed");
                    }
                    if let Err(e) = slow(&state, &world, &mut deletions).await {
                        tracing::warn!(world = %world.schema, error = %e, "device upkeep failed");
                    }
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

async fn worlds(state: &AppState) -> Vec<World> {
    let mut v = vec![World::production()];
    let envs: Vec<(Uuid,)> =
        sqlx::query_as("SELECT environment_id FROM extend_global.test_environments WHERE state = 'ready'")
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default();
    v.extend(envs.into_iter().map(|(id,)| World::test(id)));
    v
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

/// A login Extend holds for acting in `team` in `world`, by `member` when given, that `pick`
/// accepts: the most recently authorized one, else the one behind a running session. Its token may
/// since have expired; callers treat an IAM refusal as "try again later". In a test world the
/// environment's selection (with its secret) is needed too, or nothing is returned, so a test
/// world's work is never done with production credentials.
pub async fn latest_principal(
    state: &AppState,
    world: &World,
    member: Option<&str>,
    team: &str,
    pick: impl Fn(&Principal) -> bool,
) -> Option<(Principal, Option<TestingSelection>)> {
    let fits = |p: &Principal| {
        member.is_none_or(|m| p.id() == m)
            && (p.team.as_deref() == Some(team) || p.teams.iter().any(|t| t == team))
            && pick(p)
    };
    let cached = state.auth_cache.latest(world.environment_id, &fits).await;
    let sel = match world.environment_id {
        None => Some(None),
        Some(env) => state
            .selections
            .read()
            .await
            .values()
            .map(|(_, s)| s)
            .find(|s| s.environment_id == env)
            .cloned()
            .map(Some),
    };
    let found = match (cached, sel) {
        (Some(p), Some(sel)) => Some((p, sel)),
        _ => state
            .session_principals
            .read()
            .await
            .iter()
            .find(|((schema, _), (p, _))| schema == &world.schema && fits(p))
            .map(|(_, found)| found.clone()),
    };
    found.map(|(mut p, sel)| {
        p.team = Some(team.to_owned());
        (p, sel)
    })
}

/// A principal with no login, for work that needs none (local files). Anything that must act on
/// the member's behalf refuses it and says why.
fn no_login(member: &str, team: &str) -> Principal {
    Principal {
        member: Member {
            kind: extend_protocol::ids::member_kind(member).unwrap_or(MemberKind::Silicon),
            id: member.to_owned(),
            display_name: None,
        },
        team: Some(team.to_owned()),
        teams: vec![team.to_owned()],
        role: None,
        token: String::new(),
    }
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

/// Deletes files whose self-destruct time passed, acting as the Silicon that made them with the
/// latest login Extend holds for it (the Silicon need not have a running session). A file's row
/// stays until the store confirms it is gone (Briefcase answering 404 counts), so a failed
/// deletion is retried with [`Backoff`] and never forgotten. Returns how many were deleted.
pub async fn self_destruct(state: &AppState, world: &World, backoff: &mut Backoff) -> crate::error::AppResult<usize> {
    backoff.prune();
    let waiting = backoff.waiting(world);
    let due: Vec<(Uuid, String, String)> = sqlx::query_as(sql!(
        "SELECT file_id, created_by, team FROM {} WHERE NOT permanent AND self_destruct_at <= now() AND NOT (file_id = ANY($1))
         ORDER BY self_destruct_at LIMIT 100",
        world.t("files")
    ))
    .bind(&waiting)
    .fetch_all(&state.pool)
    .await?;
    let mut deleted = 0;
    for (file_id, creator, team) in due {
        let (who, sel) = latest_principal(state, world, Some(&creator), &team, |_| true)
            .await
            .unwrap_or_else(|| (no_login(&creator, &team), None));
        match state.files.destroy(&who, file_id, sel.as_ref()).await {
            Ok(()) => {
                sqlx::query(sql!("DELETE FROM {} WHERE file_id = $1", world.t("files")))
                    .bind(file_id)
                    .execute(&state.pool)
                    .await?;
                backoff.succeeded(world, file_id);
                deleted += 1;
                tracing::info!(world = %world.schema, file_id = %file_id, created_by = creator, "file self-destructed");
            }
            Err(e) => {
                let (tries, wait) = backoff.failed(world, file_id);
                tracing::warn!(
                    world = %world.schema,
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

/// Hands requests Ting hasn't accepted yet to Ting again, as the requesting Silicon with the latest
/// login Extend holds for it (it need not have a running session). Each pass is one attempt; after
/// [`REQUEST_ATTEMPTS`] the request is marked failed, with `last_error` saying why.
pub async fn retry_requests(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    let pending: Vec<(Uuid, String, String, String, String, String, Option<String>, String, i32)> = sqlx::query_as(sql!(
        "SELECT r.request_id, r.device_id, d.name, r.team, r.from_id, r.to_id, r.session_id, r.reason, r.attempts FROM {} r JOIN {} d USING (device_id)
         WHERE r.delivery = 'pending' AND r.attempts < $1 ORDER BY r.created_at LIMIT 200",
        world.t("requests"),
        world.t("devices")
    ))
    .bind(REQUEST_ATTEMPTS)
    .fetch_all(&state.pool)
    .await?;
    for (id, device_id, name, team, from, to, session, reason, attempts) in pending {
        // Ting delivers only to recipients that registered with their own login; register the
        // recipient again when Extend holds its login (a no-op once registered).
        if let Some((recipient, rsel)) = latest_principal(state, world, Some(&to), &team, |_| true).await
            && let Err(e) = state.notifier.register_recipient(&recipient, rsel.as_ref()).await
        {
            tracing::debug!(request_id = %id, recipient = to, error = %e.0.message, "registering the recipient with Ting failed");
        }
        let ting = crate::ting::DeviceRequestTing {
            request_id: id,
            device_id: &device_id,
            device_name: &name,
            from: &from,
            to: &to,
            session_id: session.as_deref(),
            reason: &reason,
        };
        let result = match latest_principal(state, world, Some(&from), &team, |_| true).await {
            Some((p, sel)) => state
                .notifier
                .device_request(&p, &ting, sel.as_ref())
                .await
                .map_err(|e| match &e.0.hint {
                    Some(h) => format!("{} {h}", e.0.message),
                    None => e.0.message.clone(),
                }),
            None => Err(format!(
                "Extend holds no signed-in login for {from} to send this request through Ting with; it is sent when {from} next uses Extend."
            )),
        };
        let last = attempts + 1 >= REQUEST_ATTEMPTS;
        let (delivery, error) = match result {
            Ok(()) => ("delivered", None),
            Err(why) if last => (
                "failed",
                Some(format!(
                    "{why} Extend gave up after {REQUEST_ATTEMPTS} attempts; send the request again with `extend request send {device_id} --reason \"...\"`."
                )),
            ),
            Err(why) => ("pending", Some(why)),
        };
        sqlx::query(sql!(
            "UPDATE {} SET delivery = $2, attempts = attempts + 1, last_error = $3 WHERE request_id = $1",
            world.t("requests")
        ))
        .bind(id)
        .bind(delivery)
        .bind(&error)
        .execute(&state.pool)
        .await?;
        match delivery {
            "delivered" => {
                tracing::info!(world = %world.schema, request_id = %id, "request delivered through Ting on retry")
            }
            "failed" => {
                tracing::warn!(world = %world.schema, request_id = %id, error = ?error, "request could not be delivered through Ting; marked failed");
                domain::log(
                    state,
                    world,
                    &device_id,
                    &domain::system_member(),
                    "request_failed",
                    session.as_deref(),
                    serde_json::json!({"request_id": id, "from": from, "to": to, "error": error}),
                )
                .await;
            }
            _ => tracing::debug!(world = %world.schema, request_id = %id, error = ?error, "request still pending"),
        }
    }
    Ok(())
}
