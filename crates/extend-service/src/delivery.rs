//! Sending Tings with an actor chain, and what Extend records about Ting per Team: which of its
//! types Ting doesn't know there (`ting_type_status`), and whether each member's registration
//! reaches them there (`ting_recipients`).
//!
//! A Ting goes as the first actor in its chain that Extend holds a login for (see crate::ting for
//! who may be an actor). An attempt with no login for any actor doesn't count against the Ting's
//! attempts: it waits for one of those members' next call to Extend (the Auth hook in
//! crate::state), or for the scheduler.

use serde_json::Value;
use time::OffsetDateTime;

use crate::db::World;
use crate::iam::{Principal, TestingSelection};
use crate::state::AppState;
use crate::ting;

/// A member who may send a Ting.
#[derive(Debug, Clone)]
pub enum Actor {
    /// A login the request being handled just used.
    Fresh(Principal, Option<TestingSelection>),
    /// A member whose latest login Extend holds, if any.
    Member(String),
}

/// What one attempt at a Ting did.
#[derive(Debug, Clone, Default)]
pub struct Attempt {
    pub delivered: bool,
    /// Whether an actor with a login tried. An attempt without one doesn't count.
    pub tried: bool,
    /// What went wrong, plainly (message and hint).
    pub error: Option<String>,
    /// Ting answered `recipient_not_registered`: stop retrying until the recipient registers.
    pub not_registered: bool,
    /// Ting doesn't know the type in the Ting's Team: retried every 10 minutes, and shown.
    pub missing_type: bool,
}

/// Seconds before the next try after `attempts` counted attempts: 30 s, then 1, 2, 4 and 8 minutes,
/// then every 8 minutes.
pub fn backoff(attempts: i32) -> time::Duration {
    let secs = match attempts {
        i32::MIN..=1 => 30,
        2 => 60,
        3 => 120,
        4 => 240,
        _ => 480,
    };
    time::Duration::seconds(secs)
}

/// How long a Ting whose type Ting doesn't know waits before it is tried again.
pub const MISSING_TYPE_RETRY: time::Duration = time::Duration::minutes(10);

/// The last error of a Ting that couldn't go because nobody's login was at hand.
pub fn no_login(members: &[&str]) -> String {
    format!(
        "Extend holds no login for {} right now to send it with; it goes at their next use of Extend.",
        members.join(" or ")
    )
}

fn explain(e: &crate::error::AppError) -> String {
    match &e.0.hint {
        Some(h) => format!("{} {h}", e.0.message),
        None => e.0.message.clone(),
    }
}

/// Sends a frozen `tings.send` body as the first actor of `chain` that can, and records what Ting
/// said about the type and the recipient in the body's Team.
pub async fn send(state: &AppState, world: &World, chain: &[Actor], body: &Value) -> Attempt {
    let (team, ty, recipient) = ting::body_parts(body);
    let mut attempt = Attempt::default();
    let mut members = Vec::new();
    for actor in chain {
        let found = match actor {
            Actor::Fresh(p, sel) => {
                members.push(p.id().to_owned());
                // A login acts in a Team it reaches; the Ting's Team must be one of them.
                (p.team.as_deref() == Some(team.as_str()) || p.teams.contains(&team)).then(|| {
                    let mut p = p.clone();
                    p.team = Some(team.clone());
                    (p, sel.clone())
                })
            }
            Actor::Member(m) => {
                members.push(m.clone());
                crate::scheduler::latest_principal(state, world, Some(m), &team, |_| true).await
            }
        };
        let Some((p, sel)) = found else { continue };
        if p.id() == recipient && state.notifier.self_send_refused() {
            continue;
        }
        attempt.tried = true;
        match state.notifier.send_frozen(&p, body, sel.as_ref()).await {
            Ok(()) => {
                attempt.delivered = true;
                attempt.error = None;
                type_known(state, world, &team, &ty).await;
                reached(state, world, &recipient, &team).await;
                return attempt;
            }
            Err(e) => {
                attempt.error = Some(explain(&e));
                if let Some(missing) = ting::missing_type(&e) {
                    attempt.missing_type = true;
                    type_missing(state, world, &team, &missing, &explain(&e)).await;
                    return attempt;
                }
                if ting::not_registered(&e) {
                    attempt.not_registered = true;
                    refused(state, world, &recipient, &team).await;
                    return attempt;
                }
                if ting::self_send_refusal(&e) {
                    // Try the next actor in the chain.
                    continue;
                }
                return attempt;
            }
        }
    }
    if !attempt.tried {
        let names: Vec<&str> = members.iter().map(String::as_str).collect();
        attempt.error = Some(no_login(&names));
        state.ting_waiting(world, &members).await;
    }
    attempt
}

async fn type_known(state: &AppState, world: &World, team: &str, ty: &str) {
    let _ = sqlx::query(sql!(
        "DELETE FROM {} WHERE team = $1 AND ting_type = $2",
        world.t("ting_type_status")
    ))
    .bind(team)
    .bind(ty)
    .execute(&state.pool)
    .await;
}

async fn type_missing(state: &AppState, world: &World, team: &str, ty: &str, error: &str) {
    let res = sqlx::query(sql!(
        "INSERT INTO {} (team, ting_type, missing_since, last_checked_at, last_error) VALUES ($1, $2, now(), now(), $3)
         ON CONFLICT (team, ting_type) DO UPDATE SET last_checked_at = now(), last_error = EXCLUDED.last_error",
        world.t("ting_type_status")
    ))
    .bind(team)
    .bind(ty)
    .bind(error)
    .execute(&state.pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, team, ty, "recording a missing Ting type failed");
    }
    tracing::warn!(world = %world.schema, team, ty, "Ting refused an app type on a send to this Team; a Ting manager in the app owner's Team must register it");
}

/// A successful Ting to `member` in `team`: they receive Extend's Tings there.
async fn reached(state: &AppState, world: &World, member: &str, team: &str) {
    let _ = sqlx::query(sql!(
        "UPDATE {} SET refused_at = NULL, last_error = NULL WHERE member_id = $1 AND team = $2 AND refused_at IS NOT NULL",
        world.t("ting_recipients")
    ))
    .bind(member)
    .bind(team)
    .execute(&state.pool)
    .await;
}

/// Ting refused a Ting to `member` in `team` as not registered. It counts as the member turning
/// Extend off only when Extend had registered them there; a member Extend never registered stays
/// pending, with why.
async fn refused(state: &AppState, world: &World, member: &str, team: &str) {
    let _ = sqlx::query(sql!(
        "INSERT INTO {t} (member_id, team, last_error) VALUES ($1, $2, $3)
         ON CONFLICT (member_id, team) DO UPDATE SET
             refused_at = CASE WHEN {t}.registered_at IS NOT NULL THEN now() ELSE {t}.refused_at END,
             last_error = CASE WHEN {t}.registered_at IS NOT NULL
                               THEN 'Turned off in Ting: Extend''s notifications are refused.'
                               ELSE EXCLUDED.last_error END",
        t = world.t("ting_recipients")
    ))
    .bind(member)
    .bind(team)
    .bind(format!("Sign in to Extend for {team}"))
    .execute(&state.pool)
    .await;
}

/// Registers `member` (a login acting in `team`) with Ting and records the result. `force` is
/// "Turn on": it registers again even when Extend has a record.
pub async fn register(
    state: &AppState,
    world: &World,
    member: &Principal,
    sel: Option<&TestingSelection>,
    force: bool,
) -> crate::error::AppResult<()> {
    let team = member.team()?.to_owned();
    match state.notifier.register_recipient(member, force, sel).await {
        Ok(()) => {
            sqlx::query(sql!(
                "INSERT INTO {} (member_id, team, registered_at) VALUES ($1, $2, now())
                 ON CONFLICT (member_id, team) DO UPDATE SET registered_at = now(), refused_at = NULL, last_error = NULL",
                world.t("ting_recipients")
            ))
            .bind(member.id())
            .bind(&team)
            .execute(&state.pool)
            .await?;
            // Tings that stopped because this member wasn't registered go again now.
            retry_now_for(state, world, member.id(), &team).await;
            Ok(())
        }
        Err(e) => {
            // Only an answer from Ting is recorded: an outage leaves no row, so the next chance to
            // register (a grant, a claim, a first call) tries again.
            if e.code() != extend_protocol::ErrorCode::ServiceUnavailable {
                let _ = sqlx::query(sql!(
                    "INSERT INTO {} (member_id, team, last_error) VALUES ($1, $2, $3)
                     ON CONFLICT (member_id, team) DO UPDATE SET last_error = EXCLUDED.last_error",
                    world.t("ting_recipients")
                ))
                .bind(member.id())
                .bind(&team)
                .bind(explain(&e))
                .execute(&state.pool)
                .await;
            }
            Err(e)
        }
    }
}

/// Registers a Carbon in `team` only when Extend has no record for them there, so a Carbon who
/// turned Extend off in Ting stays off. Runs in the background; failures are logged.
pub fn register_carbon_if_new(state: &crate::state::Shared, world: &World, carbon: &Principal, team: &str) {
    let (state, world, mut carbon, team) = (state.clone(), world.clone(), carbon.clone(), team.to_owned());
    tokio::spawn(async move {
        if !(carbon.team.as_deref() == Some(team.as_str()) || carbon.teams.contains(&team)) {
            return;
        }
        carbon.team = Some(team.clone());
        let Some(_fence) = state.world_open(&world).await else {
            return;
        };
        let known: Option<(i32,)> = sqlx::query_as(sql!(
            "SELECT 1 FROM {} WHERE member_id = $1 AND team = $2",
            world.t("ting_recipients")
        ))
        .bind(carbon.id())
        .bind(&team)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or(None);
        if known.is_some() {
            return;
        }
        let sel = crate::scheduler::selection_of(&state, &world).await;
        if let Err(e) = register(&state, &world, &carbon, sel.as_ref(), false).await {
            tracing::warn!(carbon = carbon.id(), team, error = %e.0.message, "registering the Carbon with Ting failed");
        }
    });
}

/// Registers a Silicon (a login acting in its Team) in the background, as at a session start.
pub fn register_silicon(
    state: &crate::state::Shared,
    world: &World,
    silicon: &Principal,
    sel: Option<&TestingSelection>,
) {
    let (state, world, silicon, sel) = (state.clone(), world.clone(), silicon.clone(), sel.cloned());
    tokio::spawn(async move {
        let Some(_fence) = state.world_open(&world).await else {
            return;
        };
        if let Err(e) = register(&state, &world, &silicon, sel.as_ref(), false).await {
            tracing::warn!(
                silicon = silicon.id(),
                error = %e.0.message,
                hint = ?e.0.hint,
                "registering the Silicon to receive Tings failed; Tings to it stay pending and are retried"
            );
        }
    });
}

/// Makes every Ting to `member` in `team` that is waiting due now (after a registration).
pub async fn retry_now_for(state: &AppState, world: &World, member: &str, team: &str) {
    let _ = sqlx::query(sql!(
        "UPDATE {} SET ting_next_at = now()
         WHERE delivery = 'pending' AND ting_team = $2
           AND ((routed_to = 'holder' AND to_id = $1) OR (routed_to = 'carbon' AND routed_to_id = $1))",
        world.t("requests")
    ))
    .bind(member)
    .bind(team)
    .execute(&state.pool)
    .await;
    let _ = sqlx::query(sql!(
        "UPDATE {} SET ting_next_at = now() WHERE ting_delivery = 'pending' AND to_id = $1 AND team = $2",
        world.t("wake_requests")
    ))
    .bind(member)
    .bind(team)
    .execute(&state.pool)
    .await;
    let _ = sqlx::query(sql!(
        "UPDATE {} SET answer_ting_next_at = now() WHERE answer_ting = 'pending' AND from_id = $1 AND team = $2",
        world.t("wake_requests")
    ))
    .bind(member)
    .bind(team)
    .execute(&state.pool)
    .await;
}

/// Extend's types Ting answered unknown in `team`, by full name.
pub async fn missing_types(state: &AppState, world: &World, team: &str) -> Vec<String> {
    sqlx::query_scalar(sql!(
        "SELECT ting_type FROM {} WHERE team = $1 ORDER BY ting_type",
        world.t("ting_type_status")
    ))
    .bind(team)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
}

/// A member's Ting registration in a Team, as `GET /ting-registration` shows it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RecipientRow {
    pub registered_at: Option<OffsetDateTime>,
    pub refused_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
}

impl RecipientRow {
    /// pending: never registered (or refused while never registered); off: refused after the
    /// last registration; on: otherwise.
    pub fn status(row: Option<&RecipientRow>) -> extend_protocol::model::TingStatus {
        use extend_protocol::model::TingStatus;
        match row {
            None => TingStatus::Pending,
            Some(r) => match (r.registered_at, r.refused_at) {
                (None, _) => TingStatus::Pending,
                (Some(reg), Some(refused)) if refused > reg => TingStatus::Off,
                _ => TingStatus::On,
            },
        }
    }
}

pub async fn recipient(state: &AppState, world: &World, member: &str, team: &str) -> Option<RecipientRow> {
    sqlx::query_as(sql!(
        "SELECT registered_at, refused_at, last_error FROM {} WHERE member_id = $1 AND team = $2",
        world.t("ting_recipients")
    ))
    .bind(member)
    .bind(team)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_stays_at_eight_minutes() {
        let secs: Vec<i64> = (1..=7).map(|n| backoff(n).whole_seconds()).collect();
        assert_eq!(secs, vec![30, 60, 120, 240, 480, 480, 480]);
    }

    #[test]
    fn registration_status() {
        use extend_protocol::model::TingStatus;
        let t = OffsetDateTime::now_utc();
        let row = |reg: Option<OffsetDateTime>, refused: Option<OffsetDateTime>| RecipientRow {
            registered_at: reg,
            refused_at: refused,
            last_error: None,
        };
        assert_eq!(RecipientRow::status(None), TingStatus::Pending);
        assert_eq!(RecipientRow::status(Some(&row(None, Some(t)))), TingStatus::Pending);
        assert_eq!(RecipientRow::status(Some(&row(Some(t), None))), TingStatus::On);
        assert_eq!(
            RecipientRow::status(Some(&row(Some(t), Some(t + time::Duration::seconds(1))))),
            TingStatus::Off
        );
        assert_eq!(
            RecipientRow::status(Some(&row(Some(t + time::Duration::seconds(1)), Some(t)))),
            TingStatus::On
        );
    }
}
