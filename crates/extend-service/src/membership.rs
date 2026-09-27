//! Acting on Team membership only when IAM gives a definite answer.
//!
//! A grant needs the Silicon, and the Carbon who gave it, to be active members of the Silicon's
//! Team. Extend learns that one of them left in four ways: the IAM webhook (crate::revocation), the
//! Silicon's refused login on session routes, the owner-active check at every use (here), and the
//! sweep (here). Nothing is deleted on a 403, an error or a missing reader ([`Membership::Unknown`]).
//! A definite "not a member" ([`Membership::Gone`]) refuses the call and ends the running sessions
//! at once, because access ends immediately; the grants themselves are deleted only once a second
//! reader confirms, or, in the sweep, after two such answers at least 10 minutes apart. One
//! Silicon's view of IAM can therefore never wipe a Carbon's grants.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use extend_protocol::ErrorCode;
use extend_protocol::model::{EndReason, WakeEndReason};
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::db::World;
use crate::domain::{self, GrantEnd, RevokeScope};
use crate::error::{AppError, AppResult};
use crate::iam::{Membership, Principal, TestingSelection};
use crate::state::{AppState, Shared};

/// How long a Silicon reader's "gone" counts as "can't tell" after a second reader contradicted it.
pub const CONTRADICTION_WINDOW: time::Duration = time::Duration::minutes(10);
/// How far apart the sweep's two "gone" answers must be before it deletes grants.
pub const SWEEP_CONFIRM_AFTER: time::Duration = time::Duration::minutes(10);

/// Positive owner-active answers, per (world schema, Team, Carbon). Gone and Unknown answers are
/// never kept, so a missed webhook can't keep a session going past the next use plus this TTL.
#[derive(Default)]
pub struct OwnerCache {
    entries: RwLock<HashMap<(String, String, String), Instant>>,
    /// The sweep's first reader per (world schema, member, Team) that said "gone", so the second
    /// answer can come from another reader when there is one.
    sweep_readers: RwLock<HashMap<(String, String, String), String>>,
}

impl OwnerCache {
    async fn fresh(&self, key: &(String, String, String), ttl: Duration) -> bool {
        self.entries.read().await.get(key).is_some_and(|at| at.elapsed() < ttl)
    }
    async fn put(&self, key: (String, String, String)) {
        let mut e = self.entries.write().await;
        if e.len() > 50_000 {
            e.clear();
        }
        e.insert(key, Instant::now());
    }
}

/// Forgets cached answers about these members (IAM named them in an event).
pub async fn forget(state: &AppState, members: &[String]) {
    let mut e = state.owner_cache.entries.write().await;
    if members.is_empty() {
        e.clear();
    } else {
        e.retain(|(_, _, m), _| !members.contains(m));
    }
}

/// Forgets every cached answer in one world (a test environment's clean, restore or purge).
pub async fn forget_world(state: &AppState, world: &World) {
    state
        .owner_cache
        .entries
        .write()
        .await
        .retain(|(schema, _, _), _| *schema != world.schema);
    state
        .owner_cache
        .sweep_readers
        .write()
        .await
        .retain(|(schema, _, _), _| *schema != world.schema);
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct CheckRow {
    state: String,
    gone_since: Option<OffsetDateTime>,
    contradicted_at: Option<OffsetDateTime>,
}

async fn check_row(state: &AppState, world: &World, member: &str, team: &str) -> Option<CheckRow> {
    sqlx::query_as(sql!(
        "SELECT state, gone_since, contradicted_at FROM {} WHERE member_id = $1 AND team = $2",
        world.t("membership_checks")
    ))
    .bind(member)
    .bind(team)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
}

async fn record(state: &AppState, world: &World, member: &str, team: &str, answer: Membership, now: OffsetDateTime) {
    let res = sqlx::query(sql!(
        "INSERT INTO {t} (member_id, team, checked_at, state, gone_since) VALUES ($1, $2, $3, $4, CASE WHEN $4 = 'gone' THEN $3 END)
         ON CONFLICT (member_id, team) DO UPDATE SET
             checked_at = EXCLUDED.checked_at,
             state = CASE WHEN EXCLUDED.state = 'unknown' AND {t}.state = 'gone' THEN 'gone' ELSE EXCLUDED.state END,
             gone_since = CASE WHEN EXCLUDED.state = 'gone' THEN COALESCE({t}.gone_since, EXCLUDED.checked_at)
                               WHEN EXCLUDED.state = 'active' THEN NULL ELSE {t}.gone_since END",
        t = world.t("membership_checks")
    ))
    .bind(member)
    .bind(team)
    .bind(now)
    .bind(answer.as_str())
    .execute(&state.pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, member, team, "recording a membership answer failed");
    }
}

/// The refusal a Silicon hears when the Carbon who gave it access left its Team.
pub fn owner_gone(owner: &str, device_name: &str, team: &str) -> AppError {
    AppError::new(
        ErrorCode::NoAccess,
        format!(
            "{owner}, who gave you access to {device_name}, is no longer an active member of {team}, so the access \
             {owner} gave in {team} has ended."
        ),
    )
    .hint(format!("Ask a Carbon in {team} who owns a device to give you access."))
}

/// Whether the Carbon who owns a pair is still an active member of `team`, checked at a session
/// start, every command, a wake request and a request send by a Silicon acting in `team` through
/// that pair (while `EXTEND_OWNER_CHECK_AT_USE` is on). The calling Silicon is the reader.
///
/// - Active: the call goes on; the answer is reused for `EXTEND_OWNER_CHECK_CACHE_S`.
/// - Unknown: the call goes on; the sweep checks later.
/// - Gone: the call is refused, the Carbon's running sessions in `team` end through any of their
///   pairs, their open wake requests there are withdrawn, and a second reader is asked in the
///   background before their grants in `team` are deleted. While the answer stays "gone", every
///   check refuses without asking again.
pub async fn owner_active(
    state: &Shared,
    world: &World,
    team: &str,
    owner: &str,
    device_name: &str,
    reader: &Principal,
    sel: Option<&TestingSelection>,
) -> AppResult<()> {
    if !state.cfg.tuning.owner_check_at_use {
        return Ok(());
    }
    let key = (world.schema.clone(), team.to_owned(), owner.to_owned());
    let ttl = Duration::from_secs(state.cfg.tuning.owner_check_cache_s);
    let row = check_row(state, world, owner, team).await;
    if row.as_ref().is_some_and(|r| r.state == "gone") {
        return Err(owner_gone(owner, device_name, team));
    }
    if state.owner_cache.fresh(&key, ttl).await {
        return Ok(());
    }
    let now = OffsetDateTime::now_utc();
    let mut answer = state.iam.membership(team, owner, Some(reader), sel).await;
    if answer == Membership::Gone
        && reader.is_silicon()
        && row
            .as_ref()
            .and_then(|r| r.contradicted_at)
            .is_some_and(|c| now - c < CONTRADICTION_WINDOW)
    {
        tracing::error!(
            world = %world.schema, team, owner, reader = reader.id(),
            "IAM told a Silicon reader that the Carbon behind its grant is gone, but another reader said active within the last 10 minutes; treating it as unknown (does IAM hide Carbons from Silicons? EXTEND_OWNER_CHECK_AT_USE=false turns this check off)"
        );
        answer = Membership::Unknown;
    }
    match answer {
        Membership::Active => {
            record(state, world, owner, team, answer, now).await;
            state.owner_cache.put(key).await;
            Ok(())
        }
        Membership::Unknown => {
            tracing::info!(world = %world.schema, team, owner, reader = reader.id(), "IAM couldn't say whether the Carbon behind a grant is still in the Team; the call goes on");
            record(state, world, owner, team, answer, now).await;
            Ok(())
        }
        Membership::Gone => {
            record(state, world, owner, team, answer, now).await;
            carbon_gone(state, world, owner, team).await;
            let (state, world, owner, team, first) = (
                state.clone(),
                world.clone(),
                owner.to_owned(),
                team.to_owned(),
                reader.id().to_owned(),
            );
            tokio::spawn(async move {
                let Some(_fence) = state.world_open(&world).await else {
                    return;
                };
                confirm_gone(&state, &world, &owner, &team, &first).await;
            });
            Err(owner_gone(&key.2, device_name, &key.1))
        }
    }
}

/// What happens at once when a Carbon is definitely gone from a Team: the sessions of the Silicons
/// they gave access to there end, and those Silicons' open wake requests there are withdrawn.
async fn carbon_gone(state: &AppState, world: &World, carbon: &str, team: &str) {
    if let Err(e) = domain::end_carbon_side(
        state,
        world,
        carbon,
        Some(team),
        EndReason::LeftTeam,
        &domain::system_member(),
    )
    .await
    {
        tracing::error!(error = %e, carbon, team, "ending the sessions of a Carbon who left the Team failed");
    }
    crate::wake::withdraw(
        state,
        world,
        crate::wake::Withdraw::CarbonTeam {
            carbon_id: carbon,
            team,
        },
        WakeEndReason::LeftTeam,
    )
    .await;
}

/// Asks a second reader (not the first reader, not the member) whether `member` left `team`:
/// - gone again: their grants in `team` are deleted (a Carbon's: the grants they gave there, on all
///   their pairs; a Silicon's: its grants there);
/// - active: the first answer is contradicted; for the next 10 minutes a Silicon reader's "gone"
///   for them counts as "can't tell";
/// - can't tell, or no second reader: the grants stay, and the sweep decides.
pub async fn confirm_gone(state: &AppState, world: &World, member: &str, team: &str, first_reader: &str) {
    let Some((reader, sel)) = crate::revocation::reader_excluding(state, world, team, &[member, first_reader]).await
    else {
        tracing::info!(world = %world.schema, member, team, "no second reader to confirm that a member left the Team; the grants stay until the sweep decides");
        return;
    };
    let answer = state.iam.membership(team, member, Some(&reader), sel.as_ref()).await;
    let now = OffsetDateTime::now_utc();
    match answer {
        Membership::Gone => {
            record(state, world, member, team, answer, now).await;
            revoke_left(state, world, member, team).await;
        }
        Membership::Active => {
            tracing::error!(
                world = %world.schema, member, team, first_reader, second_reader = reader.id(),
                "two IAM readers disagree about a membership: the first said gone, the second active; nothing was deleted"
            );
            let _ = sqlx::query(sql!(
                "UPDATE {} SET state = 'active', gone_since = NULL, contradicted_at = $3, checked_at = $3
                 WHERE member_id = $1 AND team = $2",
                world.t("membership_checks")
            ))
            .bind(member)
            .bind(team)
            .bind(now)
            .execute(&state.pool)
            .await;
        }
        Membership::Unknown => {
            tracing::info!(world = %world.schema, member, team, "the second reader couldn't say whether the member left; the sweep decides");
        }
    }
}

/// Deletes the grants of a member IAM confirmed left `team`.
async fn revoke_left(state: &AppState, world: &World, member: &str, team: &str) {
    let scope = match extend_protocol::ids::member_kind(member) {
        Some(extend_protocol::model::MemberKind::Carbon) => RevokeScope::Carbon {
            carbon_id: member,
            team,
        },
        _ => RevokeScope::Silicon {
            silicon_id: member,
            team,
        },
    };
    match domain::revoke_grants(state, world, scope, GrantEnd::LeftTeam, &domain::system_member()).await {
        Ok(gone) => {
            tracing::info!(world = %world.schema, member, team, grants = gone.len(), "IAM confirmed the member left the Team; its grants there ended")
        }
        Err(e) => tracing::error!(error = %e, member, team, "deleting the grants of a member who left the Team failed"),
    }
}

/// Re-checks, with IAM, every Silicon with a grant and every Carbon who gave one, per Team (the
/// scheduler runs it every `EXTEND_MEMBERSHIP_SWEEP_HOURS`). Grants are deleted only after two
/// "gone" answers at least 10 minutes apart, from different readers when there are two. `now` is
/// a seam for tests.
pub async fn sweep(state: &AppState, world: &World, now: OffsetDateTime) -> AppResult<()> {
    let pairs: Vec<(String, String)> = sqlx::query_as(sql!(
        "SELECT silicon_id, team FROM {a} UNION SELECT granted_by, team FROM {a}",
        a = world.t("device_access")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (member, team) in pairs {
        let key = (world.schema.clone(), member.clone(), team.clone());
        let first = state.owner_cache.sweep_readers.read().await.get(&key).cloned();
        // A second answer comes from another reader than the first one, when there is one.
        let reader = match &first {
            Some(f) => match crate::revocation::reader_excluding(state, world, &team, &[&member, f]).await {
                Some(r) => Some(r),
                None => crate::revocation::reader_excluding(state, world, &team, &[&member]).await,
            },
            None => crate::revocation::reader_excluding(state, world, &team, &[&member]).await,
        };
        let Some((reader, sel)) = reader else {
            continue;
        };
        let answer = state.iam.membership(&team, &member, Some(&reader), sel.as_ref()).await;
        match answer {
            Membership::Gone => {
                let row = check_row(state, world, &member, &team).await;
                let since = row.as_ref().and_then(|r| r.gone_since);
                record(state, world, &member, &team, answer, now).await;
                match since {
                    Some(since) if now - since >= SWEEP_CONFIRM_AFTER => {
                        state.owner_cache.sweep_readers.write().await.remove(&key);
                        revoke_left(state, world, &member, &team).await;
                        let _ = sqlx::query(sql!(
                            "DELETE FROM {} WHERE member_id = $1 AND team = $2",
                            world.t("membership_checks")
                        ))
                        .bind(&member)
                        .bind(&team)
                        .execute(&state.pool)
                        .await;
                    }
                    _ => {
                        state
                            .owner_cache
                            .sweep_readers
                            .write()
                            .await
                            .insert(key, reader.id().to_owned());
                    }
                }
            }
            Membership::Active => {
                state.owner_cache.sweep_readers.write().await.remove(&key);
                record(state, world, &member, &team, answer, now).await;
            }
            Membership::Unknown => record(state, world, &member, &team, answer, now).await,
        }
    }
    Ok(())
}
