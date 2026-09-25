//! Ending access when IAM says a member logged out, left a team, or was removed (TECHNICAL.md
//! section 9). A webhook is only a prompt: every decision here is re-checked with IAM first.

use bridge_protocol::model::{EndReason, MemberKind};

use crate::db::World;
use crate::domain;
use crate::error::AppResult;
use crate::iam::IamEvent;
use crate::state::AppState;

pub async fn apply(state: &AppState, world: &World, event: &IamEvent) -> AppResult<()> {
    state.auth_cache.forget(&event.members).await;
    if event.members.is_empty() && event.ends_access() {
        // An event that names nobody (a bulk revocation): drop every cached answer.
        state.auth_cache.forget(&[]).await;
    }
    for member in &event.members {
        recheck(state, world, member, event).await?;
    }
    Ok(())
}

async fn recheck(state: &AppState, world: &World, member: &str, event: &IamEvent) -> AppResult<()> {
    let kind = bridge_protocol::ids::member_kind(member);
    let logout = {
        let t = event.event_type.to_ascii_lowercase();
        t.contains("logout") || t.contains("logged_out") || t.contains("session")
    };
    if kind == Some(MemberKind::Silicon) {
        // Running sessions: re-authorize with the token the Silicon last used.
        let running: Vec<(String, String)> =
            sqlx::query_as(sql!("SELECT session_id, team FROM {} WHERE silicon_id = $1 AND state <> 'ended'", world.t("sessions")))
                .bind(member)
                .fetch_all(&state.pool)
                .await?;
        for (sid, team) in running {
            let stored = state.session_principals.read().await.get(&(world.schema.clone(), sid.clone())).cloned();
            let still_ok = match &stored {
                Some((p, sel)) => state.iam.authorize(&p.token, Some(&team), sel.as_ref()).await.is_ok(),
                None => state.iam.member_active(&team, member, None).await.unwrap_or(true),
            };
            if !still_ok {
                let active = state.iam.member_active(&team, member, stored.as_ref().and_then(|(_, s)| s.as_ref())).await.unwrap_or(true);
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
        // Access grants in teams it no longer belongs to.
        let grants: Vec<(String, String)> = sqlx::query_as(sql!(
            "SELECT a.device_id, d.team FROM {} a JOIN {} d USING (device_id) WHERE a.silicon_id = $1",
            world.t("device_access"),
            world.t("devices")
        ))
        .bind(member)
        .fetch_all(&state.pool)
        .await?;
        for (device_id, team) in grants {
            if !state.iam.member_active(&team, member, None).await.unwrap_or(true) {
                sqlx::query(sql!("DELETE FROM {} WHERE device_id = $1 AND silicon_id = $2", world.t("device_access")))
                    .bind(&device_id)
                    .bind(member)
                    .execute(&state.pool)
                    .await?;
                domain::log(state, world, &device_id, &domain::system_member(), "access_revoked", None, serde_json::json!({"silicon_id": member, "reason": "left_team"})).await;
            }
        }
    } else if kind == Some(MemberKind::Carbon) {
        // An owner who left a team takes their devices in that team with them.
        let owned: Vec<(String, String)> =
            sqlx::query_as(sql!("SELECT device_id, team FROM {} WHERE owner_id = $1 AND removed_at IS NULL AND host_device_id IS NULL", world.t("devices")))
                .bind(member)
                .fetch_all(&state.pool)
                .await?;
        for (device_id, team) in owned {
            if !state.iam.member_active(&team, member, None).await.unwrap_or(true) {
                domain::unpair(state, world, &device_id, EndReason::LeftTeam, &domain::system_member()).await?;
            }
        }
    }
    Ok(())
}
