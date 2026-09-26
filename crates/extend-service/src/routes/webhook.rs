//! Signed Silicon IAM events.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;

use super::no_content;
use crate::db::World;
use crate::error::{AppError, AppResult};
use crate::revocation;
use crate::state::Shared;

pub async fn receive(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> AppResult<Response> {
    if body.len() > 1 << 20 {
        return Err(AppError::invalid("IAM webhook bodies are limited to 1 MiB."));
    }
    let event = state.iam.verify_webhook(&headers, &body).await?;
    let world = match event.testing_environment_id {
        Some(id) => World::test(id),
        None => World::production(),
    };
    if world.is_test() && !state.ready_worlds.read().await.contains(&world.schema) {
        crate::db::ensure_world(&state.pool, &world).await?;
    }
    // At-least-once delivery: a duplicate is acknowledged and ignored.
    let fresh = sqlx::query(sql!(
        "INSERT INTO {} (event_id) VALUES ($1) ON CONFLICT DO NOTHING",
        world.t("iam_events")
    ))
    .bind(&event.event_id)
    .execute(&state.pool)
    .await?
    .rows_affected()
        == 1;
    if fresh {
        tracing::info!(event_id = %event.event_id, event_type = %event.event_type, members = ?event.members, environment = ?event.testing_environment_id, "IAM event");
        revocation::apply(&state, &world, &event).await?;
    }
    Ok(no_content())
}
