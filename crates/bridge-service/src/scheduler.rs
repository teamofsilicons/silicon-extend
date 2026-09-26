//! Background work: idle sessions, offline devices, pair expiry, pairing-code rotation, file
//! self-destruct, request delivery retries and stale enrollments.

use std::time::Duration;

use bridge_protocol::frames::EnrollmentFrame;
use bridge_protocol::model::EndReason;
use bridge_protocol::SESSION_OFFLINE_GRACE_S;
use uuid::Uuid;

use crate::db::World;
use crate::domain;
use crate::state::{AppState, Shared};

pub fn spawn(state: Shared) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            n += 1;
            if let Err(e) = rotate_codes(&state).await {
                tracing::warn!(error = %e, "rotating pairing codes failed");
            }
            for world in worlds(&state).await {
                if let Err(e) = sessions(&state, &world).await {
                    tracing::warn!(world = %world.schema, error = %e, "session upkeep failed");
                }
                if n.is_multiple_of(15) {
                    if let Err(e) = crate::telemetry::export(&state, &world).await {
                        tracing::warn!(world = %world.schema, error = %e, "telemetry export failed");
                    }
                    if let Err(e) = slow(&state, &world).await {
                        tracing::warn!(world = %world.schema, error = %e, "device upkeep failed");
                    }
                }
            }
            if n.is_multiple_of(30) {
                let _ = sqlx::query(
                    "DELETE FROM bridge_global.enrollments
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
    let envs: Vec<(Uuid,)> = sqlx::query_as("SELECT environment_id FROM bridge_global.test_environments WHERE state = 'ready'")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    v.extend(envs.into_iter().map(|(id,)| World::test(id)));
    v
}

/// Pushes a fresh code to every enrollment whose code expired while it's still waiting.
async fn rotate_codes(state: &AppState) -> crate::error::AppResult<()> {
    let due: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT enrollment_id FROM bridge_global.enrollments WHERE code_expires_at <= now() AND paired_device_id IS NULL
           AND last_seen_at > now() - interval '15 minutes'",
    )
    .fetch_all(&state.pool)
    .await?;
    for (id,) in due {
        if !state.hub.enrollment_connected(id).await {
            continue;
        }
        if let Some((code, expires, true)) = crate::routes::enroll::rotate_if_due(state, id).await? {
            state.hub.send_enrollment(id, EnrollmentFrame::Code { pairing_code: code, code_expires_at: expires }).await;
        }
    }
    Ok(())
}

async fn sessions(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    let actor = domain::system_member();
    // Idle (and takeovers that ran out).
    let idle: Vec<(String,)> = sqlx::query_as(sql!("SELECT session_id FROM {} WHERE state <> 'ended' AND idle_ends_at <= now()", world.t("sessions")))
        .fetch_all(&state.pool)
        .await?;
    for (sid,) in idle {
        domain::end_session(state, world, &sid, EndReason::IdleTimeout, &actor).await?;
    }
    // Devices that stayed offline during a session.
    let running: Vec<(String, String)> = sqlx::query_as(sql!("SELECT session_id, device_id FROM {} WHERE state <> 'ended'", world.t("sessions")))
        .fetch_all(&state.pool)
        .await?;
    for (sid, device_id) in running {
        let Some(d) = domain::load_device(state, world, &device_id).await? else { continue };
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

async fn slow(state: &AppState, world: &World) -> crate::error::AppResult<()> {
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
    // Files whose self-destruct time passed.
    let due: Vec<(Uuid, String)> = sqlx::query_as(sql!(
        "SELECT file_id, created_by FROM {} WHERE NOT permanent AND self_destruct_at <= now() LIMIT 100",
        world.t("files")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (file_id, creator) in due {
        let who = state.session_principals.read().await.values().find(|(p, _)| p.id() == creator).cloned();
        let result = match &who {
            Some((p, sel)) => state.files.destroy(p, file_id, sel.as_ref()).await.map_err(|e| e.0.message),
            None => {
                // No live token for the Silicon: remove Bridge's record; Briefcase keeps the file until
                // the Silicon's next session lets Bridge delete it. Local files go now.
                let dummy = crate::iam::Principal {
                    member: bridge_protocol::model::Member { kind: bridge_protocol::model::MemberKind::Silicon, id: creator.clone(), display_name: None },
                    team: None,
                    teams: vec![],
                    role: None,
                    token: String::new(),
                };
                state.files.destroy(&dummy, file_id, None).await.map_err(|e| e.0.message)
            }
        };
        if let Err(e) = &result {
            tracing::warn!(file_id = %file_id, error = %e, "self-destruct delete failed");
        }
        sqlx::query(sql!("DELETE FROM {} WHERE file_id = $1", world.t("files"))).bind(file_id).execute(&state.pool).await?;
    }
    // Requests Ting hasn't accepted yet.
    let pending: Vec<(Uuid, String, String, String, String, Option<String>, String, i32)> = sqlx::query_as(sql!(
        "SELECT r.request_id, r.device_id, d.name, r.from_id, r.to_id, r.session_id, r.reason, r.attempts FROM {} r JOIN {} d USING (device_id)
         WHERE r.delivery = 'pending' AND r.attempts < 6",
        world.t("requests"),
        world.t("devices")
    ))
    .fetch_all(&state.pool)
    .await?;
    for (id, device_id, name, from, to, session, reason, _attempts) in pending {
        let who = state.session_principals.read().await.values().find(|(p, _)| p.id() == from).cloned();
        let Some((p, sel)) = who else {
            continue;
        };
        let ting = crate::ting::DeviceRequestTing { request_id: id, device_id: &device_id, device_name: &name, from: &from, to: &to, session_id: session.as_deref(), reason: &reason };
        let (delivery, err) = match state.notifier.device_request(&p, &ting, sel.as_ref()).await {
            Ok(()) => ("delivered", None),
            Err(e) => ("pending", Some(e.0.message)),
        };
        sqlx::query(sql!(
            "UPDATE {} SET delivery = CASE WHEN $2 = 'delivered' THEN 'delivered' WHEN attempts + 1 >= 6 THEN 'failed' ELSE 'pending' END,
                    attempts = attempts + 1, last_error = $3 WHERE request_id = $1",
            world.t("requests")
        ))
        .bind(id)
        .bind(delivery)
        .bind(err)
        .execute(&state.pool)
        .await?;
    }
    // Uploads nobody claimed.
    let stale: Vec<(Uuid,)> = sqlx::query_as(sql!("DELETE FROM {} WHERE expires_at < now() - interval '10 minutes' RETURNING upload_id", world.t("uploads")))
        .fetch_all(&state.pool)
        .await?;
    for (u,) in stale {
        let _ = tokio::fs::remove_file(state.cfg.data_dir.join("uploads").join(u.to_string())).await;
    }
    Ok(())
}
