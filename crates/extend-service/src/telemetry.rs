//! Telemetry: events are written to each world's `telemetry` table (an outbox) and exported to
//! Space Station when a table key is configured. A test environment exports only to its own key
//! (`EXTEND_TEST_TELEMETRY_KEYS`, a JSON map of environment id → key), never to production's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::db::World;
use crate::state::AppState;

/// Records a service-side event. Never fails the caller.
pub async fn record(state: &AppState, world: &World, member: Option<&str>, event: serde_json::Value) {
    let _ = sqlx::query(sql!(
        "INSERT INTO {} (member_id, event) VALUES ($1, $2)",
        world.t("telemetry")
    ))
    .bind(member)
    .bind(&event)
    .execute(&state.pool)
    .await;
}

fn key_for(world: &World) -> Option<String> {
    match world.environment_id {
        None => std::env::var("EXTEND_SPACE_STATION_KEY").ok().filter(|k| !k.is_empty()),
        Some(id) => {
            let raw = std::env::var("EXTEND_TEST_TELEMETRY_KEYS").ok()?;
            let keys: std::collections::BTreeMap<uuid::Uuid, String> = serde_json::from_str(&raw).ok()?;
            let key = keys.get(&id)?.clone();
            // A test key identical to production's would leak test data into production.
            (std::env::var("EXTEND_SPACE_STATION_KEY").ok().as_deref() != Some(key.as_str())).then_some(key)
        }
    }
}

/// Sends up to 200 pending events for a world. Rows are marked exported only after a flush that
/// reported no error, so a failed export is retried on the next pass.
pub async fn export(state: &AppState, world: &World) -> crate::error::AppResult<()> {
    let Some(key) = key_for(world) else { return Ok(()) };
    let rows: Vec<(i64, Option<String>, serde_json::Value, time::OffsetDateTime)> = sqlx::query_as(sql!(
        "SELECT id, member_id, event, created_at FROM {} WHERE exported_at IS NULL ORDER BY id LIMIT 200",
        world.t("telemetry")
    ))
    .fetch_all(&state.pool)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let home = state.cfg.data_dir.join("telemetry").join(world.schema.clone());
    let env = world.environment_id;
    let delivered = tokio::task::spawn_blocking(move || {
        let failed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&failed);
        let client = space_station::SpaceClient::builder(&key)
            .url(space_station::DEFAULT_URL)
            .home(home)
            .flush_timeout(std::time::Duration::from_secs(3))
            .on_error(move |_| flag.store(true, Ordering::Relaxed))
            .build()
            .ok()?;
        for (_, member, event, at) in rows {
            client.record(serde_json::json!({
                "service": "silicon-extend",
                "environment_id": env,
                "member_id": member,
                "recorded_at_ms": at.unix_timestamp_nanos() / 1_000_000,
                "event": event,
            }));
        }
        client.flush();
        Some(!failed.load(Ordering::Relaxed))
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if delivered {
        sqlx::query(sql!(
            "UPDATE {} SET exported_at = now() WHERE id = ANY($1)",
            world.t("telemetry")
        ))
        .bind(&ids)
        .execute(&state.pool)
        .await?;
    }
    Ok(())
}
