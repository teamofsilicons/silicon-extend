//! Signed Silicon IAM events.
//!
//! - The signature is verified over the exact raw body before anything else is trusted.
//! - A test delivery goes to its own test environment, only while that environment is open or
//!   being prepared; for any other environment (cleaning, disabled, removed, unknown) it is
//!   acknowledged and dropped, and never recreates a removed environment's data.
//! - Events for one aggregate apply in order: one older than the newest already applied is
//!   dropped (IAM numbers each aggregate's events).
//! - An event is recorded as applied only after its effects succeed, in the same transaction that
//!   holds its lock, so a failure midway leaves it unrecorded and IAM's retry applies it again.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use uuid::Uuid;

use super::no_content;
use crate::db::World;
use crate::error::{AppError, AppResult};
use crate::revocation;
use crate::state::Shared;

pub async fn receive(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> AppResult<Response> {
    if body.len() > 1 << 20 {
        return Err(AppError::invalid("IAM webhook bodies are limited to 1 MiB."));
    }
    // A test delivery's environment may have been learned before a restart; teach the verifier.
    if let Some(digest) = crate::iam::testing_key_digest(&body) {
        let known: Option<Uuid> = sqlx::query_scalar(
            "SELECT environment_id FROM extend_global.test_environments WHERE webhook_key_digest = $1",
        )
        .bind(&digest)
        .fetch_optional(&state.pool)
        .await?;
        if let Some(env) = known {
            state.iam.remember_test_webhook_key(&digest, env).await;
        }
    }
    let event = state.iam.verify_webhook(&headers, &body).await?;
    let (world, _fence) = match event.testing_environment_id {
        None => (World::production(), None),
        Some(id) => {
            let world = World::test(id);
            let fence = state.fence(id).read_owned().await;
            let open = state
                .environment(id)
                .await?
                .is_some_and(|row| matches!(row.state.as_str(), "ready" | "preparing"));
            if !open {
                tracing::info!(event_id = %event.event_id, environment = %id, "IAM event for a test environment that isn't open; dropped");
                return Ok(no_content());
            }
            if !state.ready_worlds.read().await.contains(&world.schema) {
                crate::db::ensure_world(&state.pool, &world).await?;
                state.ready_worlds.write().await.insert(world.schema.clone());
            }
            (world, Some(fence))
        }
    };
    let aggregate = aggregate_of(&body);
    // Deliveries of the same aggregate (or the same event) wait for each other.
    let mut tx = state.pool.begin().await?;
    let lock_key = format!(
        "extend-iam-event:{}:{}",
        world.schema,
        aggregate
            .as_ref()
            .map_or(event.event_id.as_str(), |(id, _)| id.as_str())
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 7342005))")
        .bind(&lock_key)
        .execute(&mut *tx)
        .await?;
    let seen: bool = sqlx::query_scalar(sql!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE event_id = $1)",
        world.t("iam_events")
    ))
    .bind(&event.event_id)
    .fetch_one(&mut *tx)
    .await?;
    if seen {
        // At-least-once delivery: a duplicate is acknowledged and ignored.
        tx.commit().await?;
        return Ok(no_content());
    }
    let newest: Option<i64> = match &aggregate {
        Some((id, _)) => {
            sqlx::query_scalar(sql!(
                "SELECT version FROM {} WHERE aggregate_id = $1",
                world.t("iam_aggregates")
            ))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => None,
    };
    let stale = matches!((&aggregate, newest), (Some((_, v)), Some(n)) if *v <= n);
    if stale {
        tracing::info!(event_id = %event.event_id, aggregate = ?aggregate, newest, "IAM event older than one already applied; dropped");
    } else {
        tracing::info!(event_id = %event.event_id, event_type = %event.event_type, members = ?event.members, environment = ?event.testing_environment_id, "IAM event");
        // An error here leaves the event unrecorded (the transaction rolls back), so IAM's
        // retry applies it again.
        revocation::apply(&state, &world, &event).await?;
        if let Some((id, version)) = &aggregate {
            sqlx::query(sql!(
                "INSERT INTO {} (aggregate_id, version) VALUES ($1, $2)
                 ON CONFLICT (aggregate_id) DO UPDATE SET version = GREATEST(EXCLUDED.version, {}.version), applied_at = now()",
                world.t("iam_aggregates"),
                world.t("iam_aggregates")
            ))
            .bind(id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(sql!(
        "INSERT INTO {} (event_id) VALUES ($1) ON CONFLICT DO NOTHING",
        world.t("iam_events")
    ))
    .bind(&event.event_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(no_content())
}

/// The aggregate an IAM event is about and its version (`aggregate: {type, id, version}`), from a
/// plain event or a signed test envelope (`{"test": {"metadata": {...}}}`).
pub fn aggregate_of(body: &[u8]) -> Option<(String, i64)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let meta = v.get("test").and_then(|t| t.get("metadata")).unwrap_or(&v);
    let agg = meta.get("aggregate")?;
    let id = match agg.get("id")? {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let version = agg.get("version")?.as_i64()?;
    let kind = agg.get("type").and_then(|t| t.as_str()).unwrap_or("");
    Some((format!("{kind}:{id}"), version))
}

#[cfg(test)]
mod tests {
    #[test]
    fn aggregates_are_read_from_plain_and_test_deliveries() {
        let plain = br#"{"event_id":"e","aggregate":{"type":"silicon","id":"si:sous","version":2},"data":{}}"#;
        assert_eq!(super::aggregate_of(plain), Some(("silicon:si:sous".into(), 2)));
        let wrapped = br#"{"test":{"testing_key":"k","metadata":{"aggregate":{"type":"silicon","id":"si:sous","version":7}},"data":{}}}"#;
        assert_eq!(super::aggregate_of(wrapped), Some(("silicon:si:sous".into(), 7)));
        assert_eq!(super::aggregate_of(br#"{"event_id":"e"}"#), None);
        assert_eq!(super::aggregate_of(br#"{"aggregate":{"id":"x"}}"#), None);
    }
}
