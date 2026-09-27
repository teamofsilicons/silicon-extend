//! Ending access when IAM says a member logged out, left a team, or was removed (TECHNICAL.md
//! section 9). A webhook is only a prompt: every decision here is re-checked with IAM first, so a
//! late or repeated event can't end access IAM still grants. Only when IAM can't say does a
//! removal the signed event reports decide (routes/webhook.rs has already dropped events older
//! than one applied for the same aggregate).
//!
//! - A Silicon that left a Team loses its grants in that Team (grants carry the Silicon's Team).
//! - A Carbon who left a Team loses the grants they gave there, on all their pairs, and those
//!   sessions end. Their devices stay paired: a device belongs to the Carbon, not to a Team.
//! - A Carbon who logged out of Extend ends the running sessions of the Silicons they gave access
//!   to, on their own pairs only (Carbon decision, 2026-09-27). A Silicon's logout ends its own.

use extend_protocol::model::{EndReason, MemberKind};

use crate::db::World;
use crate::domain::{self, GrantEnd, RevokeScope};
use crate::error::AppResult;
use crate::iam::{IamEvent, Membership, Principal, TestingSelection};
use crate::state::AppState;

fn is_logout(event: &IamEvent) -> bool {
    let t = event.event_type.to_ascii_lowercase();
    t.contains("logout") || t.contains("logged_out") || t.contains("session")
}

pub async fn apply(state: &AppState, world: &World, event: &IamEvent) -> AppResult<()> {
    // The logins Extend saw each named Carbon use, before the cache forgets them: after a logout
    // event, IAM's answer for them says whether the Carbon still has a live Extend login.
    let mut carbon_tokens = Vec::new();
    if is_logout(event) {
        for m in &event.members {
            if extend_protocol::ids::member_kind(m) == Some(MemberKind::Carbon) {
                let tokens = state.auth_cache.tokens_of(m, world.environment_id).await;
                if !tokens.is_empty() {
                    carbon_tokens.push((m.clone(), tokens));
                }
            }
        }
    }
    state.auth_cache.forget(&event.members).await;
    crate::membership::forget(state, &event.members).await;
    if event.members.is_empty() && event.ends_access() {
        // An event that names nobody (a bulk revocation): drop every cached answer.
        state.auth_cache.forget(&[]).await;
        crate::membership::forget(state, &[]).await;
    }
    for member in &event.members {
        recheck(state, world, member, event).await?;
    }
    for (carbon, tokens) in carbon_tokens {
        if logged_out(state, world, &tokens).await {
            let ended = domain::end_carbon_side(
                state,
                world,
                &carbon,
                None,
                EndReason::AccessRemoved,
                &domain::system_member(),
            )
            .await?;
            if !ended.is_empty() {
                tracing::info!(world = %world.schema, carbon, sessions = ?ended, "the Carbon logged out; the sessions of the Silicons they gave access to ended");
            }
        }
    }
    Ok(())
}

/// Whether IAM refuses every one of these logins (none left live). Unsure answers count as live.
async fn logged_out(state: &AppState, world: &World, tokens: &[String]) -> bool {
    let sel = crate::scheduler::selection_of(state, world).await;
    if world.is_test() && sel.is_none() {
        return false;
    }
    for token in tokens {
        match state.iam.authorize(token, None, sel.as_ref()).await {
            Ok(_) => return false,
            Err(e) if crate::iam::refuses_token(&e) => {}
            Err(_) => return false,
        }
    }
    true
}

async fn recheck(state: &AppState, world: &World, member: &str, event: &IamEvent) -> AppResult<()> {
    let kind = extend_protocol::ids::member_kind(member);
    let logout = is_logout(event);
    if kind == Some(MemberKind::Silicon) {
        // Running sessions: re-authorize with the token the Silicon last used.
        let running: Vec<(String, String)> = sqlx::query_as(sql!(
            "SELECT session_id, team FROM {} WHERE silicon_id = $1 AND state <> 'ended'",
            world.t("sessions")
        ))
        .bind(member)
        .fetch_all(&state.pool)
        .await?;
        for (sid, team) in running {
            let stored = state
                .session_principals
                .read()
                .await
                .get(&(world.schema.clone(), sid.clone()))
                .cloned();
            // IAM's live answer for the login the Silicon is using decides; IAM being unreachable
            // falls back to the membership check below.
            let still_ok = match &stored {
                Some((p, sel)) => match state.iam.authorize(&p.token, Some(&team), sel.as_ref()).await {
                    Ok(_) => true,
                    Err(e) if crate::iam::refuses_token(&e) => false,
                    Err(_) => still_member(state, world, event, &team, member, None).await,
                },
                None => still_member(state, world, event, &team, member, None).await,
            };
            if !still_ok {
                let active = still_member(state, world, event, &team, member, stored.as_ref()).await;
                let reason = if !active {
                    EndReason::LeftTeam
                } else if logout {
                    EndReason::SiliconLoggedOut
                } else {
                    EndReason::AccessRemoved
                };
                domain::end_session(state, world, &sid, reason, &domain::system_member()).await?;
            }
        }
        // Grants in Teams it no longer belongs to (each grant carries the Silicon's own Team).
        let teams: Vec<(String,)> = sqlx::query_as(sql!(
            "SELECT DISTINCT team FROM {} WHERE silicon_id = $1",
            world.t("device_access")
        ))
        .bind(member)
        .fetch_all(&state.pool)
        .await?;
        for (team,) in teams {
            if !still_member(state, world, event, &team, member, None).await {
                domain::revoke_grants(
                    state,
                    world,
                    RevokeScope::Silicon {
                        silicon_id: member,
                        team: &team,
                    },
                    GrantEnd::LeftTeam,
                    &domain::system_member(),
                )
                .await?;
            }
        }
    } else if kind == Some(MemberKind::Carbon) {
        // A Carbon who left a Team takes the access they gave there with them; the devices stay.
        let teams: Vec<(String,)> = sqlx::query_as(sql!(
            "SELECT DISTINCT team FROM {} WHERE granted_by = $1",
            world.t("device_access")
        ))
        .bind(member)
        .fetch_all(&state.pool)
        .await?;
        for (team,) in teams {
            if !still_member(state, world, event, &team, member, None).await {
                domain::revoke_grants(
                    state,
                    world,
                    RevokeScope::Carbon {
                        carbon_id: member,
                        team: &team,
                    },
                    GrantEnd::LeftTeam,
                    &domain::system_member(),
                )
                .await?;
                domain::end_carbon_side(
                    state,
                    world,
                    member,
                    Some(&team),
                    EndReason::LeftTeam,
                    &domain::system_member(),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Whether the signed event itself reports `member` removed from `team`.
fn removed(event: &IamEvent, member: &str, team: &str) -> bool {
    event.removed.iter().any(|(m, t)| m == member && t == team)
}

/// Whether `member` is still an active member of `team`. IAM is asked first, with a login Extend
/// holds for that team (the member's own, or another member's). Only when IAM can't say (a 403, an
/// error, or nobody to ask) does the signed event decide: a removal it reports ends access,
/// anything else leaves access as it is.
async fn still_member(
    state: &AppState,
    world: &World,
    event: &IamEvent,
    team: &str,
    member: &str,
    own: Option<&(Principal, Option<TestingSelection>)>,
) -> bool {
    let reader = match own {
        Some(found) => Some(found.clone()),
        None => reader(state, world, team, member).await,
    };
    let (p, sel) = match reader {
        Some((p, sel)) => (Some(p), sel),
        None => (None, crate::scheduler::selection_of(state, world).await),
    };
    match state.iam.membership(team, member, p.as_ref(), sel.as_ref()).await {
        Membership::Active => true,
        Membership::Gone => false,
        Membership::Unknown => {
            let reported = removed(event, member, team);
            tracing::info!(
                member,
                team,
                reported_removed = reported,
                "IAM couldn't confirm a membership; using the signed event"
            );
            !reported
        }
    }
}

/// A signed-in member of `team` (other than `member`) whose login can read the team's directory:
/// a running session's Silicon, else the most recent cached authorization.
pub async fn reader(
    state: &AppState,
    world: &World,
    team: &str,
    member: &str,
) -> Option<(Principal, Option<TestingSelection>)> {
    reader_excluding(state, world, team, &[member]).await
}

/// [`reader`], excluding several members (the member asked about, and a first reader whose answer
/// a second one should confirm).
pub async fn reader_excluding(
    state: &AppState,
    world: &World,
    team: &str,
    exclude: &[&str],
) -> Option<(Principal, Option<TestingSelection>)> {
    let usable = |p: &Principal| !exclude.contains(&p.id()) && p.teams.iter().any(|t| t == team);
    let from_sessions = state
        .session_principals
        .read()
        .await
        .iter()
        .find(|((schema, _), (p, _))| schema == &world.schema && usable(p))
        .map(|(_, found)| found.clone());
    if let Some((mut p, sel)) = from_sessions {
        p.team = Some(team.to_owned());
        return Some((p, sel));
    }
    let mut p = state.auth_cache.latest(world.environment_id, usable).await?;
    p.team = Some(team.to_owned());
    let sel = match world.environment_id {
        None => None,
        Some(_) => Some(crate::scheduler::selection_of(state, world).await?),
    };
    Some((p, sel))
}
