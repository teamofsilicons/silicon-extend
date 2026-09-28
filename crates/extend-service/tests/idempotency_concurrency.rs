//! Real PostgreSQL reservation races across independent service states and one-connection pools.
//! Each test owns and drops its exact database, including when its assertions panic.
mod common;

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use extend_service::config::Tuning;
use extend_service::db::World;
use extend_service::error::AppResult;
use extend_service::routes::idempotent;
use extend_service::state::Shared;
use futures::FutureExt as _;
use serde_json::{Value, json};
use sqlx::Connection as _;
use tokio::sync::Notify;

async fn with_states<F, Fut>(test: F)
where
    F: FnOnce([Shared; 2]) -> Fut,
    Fut: Future<Output = ()>,
{
    let (url, data) = common::database("idem_conc").await;
    let cfg = common::config(
        url.clone(),
        "127.0.0.1:9".parse().unwrap(),
        data.clone(),
        Tuning::default(),
    );
    let mut first = extend_service::build(cfg.clone()).await.unwrap();
    let mut second = extend_service::build(cfg).await.unwrap();
    // A provider wait or a duplicate's polling must leave this sole connection usable.
    for state in [&mut first, &mut second] {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(&url)
            .await
            .unwrap();
        let old = std::mem::replace(&mut Arc::get_mut(state).unwrap().pool, pool);
        old.close().await;
    }
    let result = AssertUnwindSafe(test([first.clone(), second.clone()]))
        .catch_unwind()
        .await;
    first.pool.close().await;
    second.pool.close().await;
    let (admin, db) = url.rsplit_once('/').unwrap();
    let mut conn = sqlx::PgConnection::connect(&format!("{admin}/postgres")).await.unwrap();
    assert!(db.starts_with("extend_idem_conc_"));
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {db} WITH (FORCE)")))
        .execute(&mut conn)
        .await
        .unwrap();
    std::fs::remove_dir_all(data).unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn headers(key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", HeaderValue::from_str(key).unwrap());
    headers
}

async fn call<F, Fut>(state: &Shared, key: &str, hash: &str, run: F) -> AppResult<Response>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = AppResult<(StatusCode, &'static str, Value)>>,
{
    idempotent(
        state,
        &World::production(),
        "si:chef",
        "reservation-test",
        &headers(key),
        hash,
        run,
    )
    .await
}

async fn answer(result: AppResult<Response>) -> (StatusCode, HeaderMap, Value) {
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
    (parts.status, parts.headers, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn concurrent_same_key_runs_once_across_independent_pools() {
    with_states(|[first, second]| async move {
        let count = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let owner = tokio::spawn({
            let (first, count, entered, release) = (first.clone(), count.clone(), entered.clone(), release.clone());
            async move {
                call(&first, "concurrent-key", "body-a", || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.notified().await;
                    Ok((StatusCode::CREATED, "created", json!({"id": "one", "original": true})))
                })
                .await
            }
        });
        entered.notified().await;
        let duplicate = tokio::spawn({
            let (second, count) = (second.clone(), count.clone());
            async move {
                call(&second, "concurrent-key", "body-a", || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok((StatusCode::OK, "duplicate", json!({"id": "wrong"})))
                })
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Both independent pools remain usable while a provider and a duplicate wait.
        for state in [&first, &second] {
            tokio::time::timeout(Duration::from_secs(1), sqlx::query("SELECT 1").execute(&state.pool))
                .await
                .unwrap()
                .unwrap();
        }
        release.notify_one();
        let original = answer(owner.await.unwrap()).await;
        let replay = answer(duplicate.await.unwrap()).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "duplicate executed the operation");
        assert_eq!(original.0, StatusCode::CREATED);
        assert_eq!(replay.0, original.0);
        assert_eq!(replay.2, original.2);
        assert_eq!(replay.1["idempotency-replayed"], "true");
        assert_eq!(replay.1["cache-control"], "no-store");
        assert!(!original.1.contains_key("idempotency-replayed"));
    })
    .await;
}

#[tokio::test]
async fn explicit_errors_are_final_answers_with_original_request_id_and_status() {
    use extend_protocol::ErrorCode;
    use extend_service::error::{AppError, REQUEST_ID};
    with_states(|[first, second]| async move {
        for (i, code) in [
            ErrorCode::InvalidInput,
            ErrorCode::Internal,
            ErrorCode::ServiceUnavailable,
        ]
        .into_iter()
        .enumerate()
        {
            let key = format!("explicit-error-{i}");
            let original = answer(
                REQUEST_ID
                    .scope(
                        "original-request".into(),
                        call(&first, &key, "body-a", || async {
                            Err(AppError::new(code, "explicit answer after attempted operation")
                                .status(409)
                                .hint("original hint")
                                .details(json!({"recorded": true})))
                        }),
                    )
                    .await,
            )
            .await;
            let replay = answer(
                REQUEST_ID
                    .scope(
                        "retry-request".into(),
                        call(&second, &key, "body-a", || async {
                            panic!("an explicit failed operation must not execute again")
                        }),
                    )
                    .await,
            )
            .await;
            assert_eq!(original.0, StatusCode::CONFLICT);
            assert_eq!(original.2["data"]["request_id"], "original-request");
            assert_eq!(original.2["data"]["code"], code.as_str());
            assert_eq!(replay.0, original.0);
            assert_eq!(replay.2, original.2);
            assert_eq!(replay.1["idempotency-replayed"], "true");
        }
        // A normal finalized 503 must not be mistaken for a pending reservation.
        let first_response = answer(
            call(&first, "explicit-normal-503", "body-a", || async {
                Err(AppError::new(
                    ErrorCode::ServiceUnavailable,
                    "provider answered unavailable",
                ))
            })
            .await,
        )
        .await;
        let replay = answer(
            tokio::time::timeout(
                Duration::from_secs(1),
                call(&second, "explicit-normal-503", "body-a", || async {
                    panic!("final 503 replay must not invoke the operation")
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(replay.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(replay.2, first_response.2);
        assert_eq!(replay.1["idempotency-replayed"], "true");
    })
    .await;
}

#[tokio::test]
async fn cancellation_retains_indeterminate_claim_and_changed_body_conflicts() {
    with_states(|[first, second]| async move {
        let entered = Arc::new(Notify::new());
        let side_effects = Arc::new(AtomicUsize::new(0));
        let owner = tokio::spawn({
            let (first, entered, side_effects) = (first.clone(), entered.clone(), side_effects.clone());
            async move {
                call(&first, "cancelled-key", "body-a", || async {
                    side_effects.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    std::future::pending().await
                })
                .await
            }
        });
        entered.notified().await;
        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        // Even very old reservations must not be stolen: a provider may already have acted.
        sqlx::query("UPDATE extend.idempotency SET created_at=now()-interval '90 days' WHERE key='cancelled-key'")
            .execute(&first.pool)
            .await
            .unwrap();
        let different = answer(
            tokio::time::timeout(
                Duration::from_secs(1),
                call(&second, "cancelled-key", "body-b", || async {
                    panic!("changed body must conflict while the result is indeterminate")
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(different.0, StatusCode::CONFLICT);
        let started = std::time::Instant::now();
        let retry = answer(
            call(&second, "cancelled-key", "body-a", || async {
                panic!("cancelled owner must never be replaced automatically")
            })
            .await,
        )
        .await;
        assert_eq!(retry.0, StatusCode::SERVICE_UNAVAILABLE);
        assert!(started.elapsed() >= Duration::from_secs(4));
        assert!(started.elapsed() < Duration::from_secs(7));
        assert!(!retry.1.contains_key("idempotency-replayed"));
        assert_eq!(side_effects.load(Ordering::SeqCst), 1);
        assert_eq!(retry.2["data"]["code"], "service_unavailable");
        assert!(retry.2["data"]["hint"].as_str().unwrap().contains("same key"));
        let pending: Value = sqlx::query_scalar("SELECT response FROM extend.idempotency WHERE key='cancelled-key'")
            .fetch_one(&first.pool)
            .await
            .unwrap();
        assert!(pending["data"]["details"]["__extend_idempotency"]["owner"].is_string());
        // Its stored form is a normal envelope readable by older protocol clients.
        let parsed: extend_protocol::ApiError = serde_json::from_value(pending["data"].clone()).unwrap();
        assert_eq!(parsed.code, extend_protocol::ErrorCode::ServiceUnavailable);
    })
    .await;
}

#[tokio::test]
async fn changed_body_conflicts_while_owner_is_running() {
    with_states(|[first, second]| async move {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let owner = tokio::spawn({
            let (first, entered, release) = (first.clone(), entered.clone(), release.clone());
            async move {
                call(&first, "running-key", "body-a", || async {
                    entered.notify_one();
                    release.notified().await;
                    Ok((StatusCode::CREATED, "created", json!({"id": "original"})))
                })
                .await
            }
        });
        entered.notified().await;
        let conflict = answer(
            tokio::time::timeout(
                Duration::from_secs(1),
                call(&second, "running-key", "body-b", || async {
                    panic!("changed body reached the operation")
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        release.notify_one();
        assert_eq!(answer(owner.await.unwrap()).await.0, StatusCode::CREATED);
        assert_eq!(conflict.0, StatusCode::CONFLICT);
    })
    .await;
}

#[tokio::test]
async fn lost_ownership_never_overwrites_another_result_or_returns_success() {
    with_states(|[first, second]| async move {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let owner = tokio::spawn({
            let (first, entered, release) = (first.clone(), entered.clone(), release.clone());
            async move {
                call(&first, "ownership-key", "body-a", || async {
                    entered.notify_one();
                    release.notified().await;
                    Ok((StatusCode::CREATED, "created", json!({"id": "must-not-overwrite"})))
                }).await
            }
        });
        entered.notified().await;
        let replacement = uuid::Uuid::new_v4().to_string();
        sqlx::query("UPDATE extend.idempotency SET response=jsonb_set(response,'{data,details,__extend_idempotency,owner}',to_jsonb($1::text)) WHERE key='ownership-key'")
            .bind(&replacement).execute(&second.pool).await.unwrap();
        release.notify_one();
        assert_eq!(answer(owner.await.unwrap()).await.0, StatusCode::SERVICE_UNAVAILABLE);
        let (status, body): (i32, Value) = sqlx::query_as("SELECT status,response FROM extend.idempotency WHERE key='ownership-key'")
            .fetch_one(&second.pool).await.unwrap();
        assert_eq!(status, 503);
        assert_eq!(body["data"]["details"]["__extend_idempotency"]["owner"], replacement);
    }).await;
}

#[tokio::test]
async fn finalization_database_failure_keeps_claim_and_never_returns_success() {
    with_states(|[first, second]| async move {
        sqlx::raw_sql("CREATE FUNCTION extend.reject_idempotency_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected finalization failure'; END $$; CREATE TRIGGER reject_idempotency_update BEFORE UPDATE ON extend.idempotency FOR EACH ROW EXECUTE FUNCTION extend.reject_idempotency_update();")
            .execute(&first.pool).await.unwrap();
        let side_effects = AtomicUsize::new(0);
        let result = answer(call(&first, "persistence-key", "body-a", || async {
            side_effects.fetch_add(1, Ordering::SeqCst);
            Ok((StatusCode::CREATED, "created", json!({"id": "already-applied"})))
        }).await).await;
        assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(side_effects.load(Ordering::SeqCst), 1);
        sqlx::query("DROP TRIGGER reject_idempotency_update ON extend.idempotency").execute(&first.pool).await.unwrap();
        let retry = answer(call(&second, "persistence-key", "body-a", || async {
            panic!("a result persistence failure must not repeat the side effect")
        }).await).await;
        assert_eq!(retry.0, StatusCode::SERVICE_UNAVAILABLE);
        let status: i32 = sqlx::query_scalar("SELECT status FROM extend.idempotency WHERE key='persistence-key'")
            .fetch_one(&second.pool).await.unwrap();
        assert_eq!(status, 503);
    }).await;
}

#[tokio::test]
async fn existing_completed_rows_replay_and_absent_keys_remain_unreserved() {
    with_states(|[first, second]| async move {
        let body = json!({"type": "legacy", "data": {"exact": [3, 2, 1]}});
        sqlx::query("INSERT INTO extend.idempotency (principal,route,key,request_hash,status,response) VALUES ('si:chef','reservation-test','legacy-complete','body-a',202,$1)")
            .bind(&body).execute(&first.pool).await.unwrap();
        let replay = answer(call(&second, "legacy-complete", "body-a", || async {
            panic!("existing completed rows must stay replayable")
        }).await).await;
        assert_eq!(replay.0, StatusCode::ACCEPTED);
        assert_eq!(replay.2, body);
        let conflict = answer(call(&second, "legacy-complete", "body-b", || async {
            panic!("a completed key must still refuse changed bodies")
        }).await).await;
        assert_eq!(conflict.0, StatusCode::CONFLICT);
        let count = AtomicUsize::new(0);
        for _ in 0..2 {
            let response = idempotent(&first, &World::production(), "si:chef", "unkeyed", &HeaderMap::new(), "same-body", || async {
                count.fetch_add(1, Ordering::SeqCst);
                Ok((StatusCode::CREATED, "unkeyed", json!({})))
            }).await;
            assert_eq!(answer(response).await.0, StatusCode::CREATED);
        }
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.idempotency WHERE route='unkeyed'")
            .fetch_one(&first.pool).await.unwrap();
        assert_eq!(rows, 0);
    }).await;
}

#[tokio::test]
async fn malformed_keys_are_rejected_including_opaque_non_ascii_headers() {
    with_states(|[first, _]| async move {
        for value in [b"short".as_slice(), b"space in key", b"eightxx\xff", &[b'x'; 256]] {
            let mut headers = HeaderMap::new();
            headers.insert("idempotency-key", HeaderValue::from_bytes(value).unwrap());
            let response = answer(
                idempotent(
                    &first,
                    &World::production(),
                    "si:chef",
                    "malformed",
                    &headers,
                    "same-body",
                    || async { panic!("a malformed key must not be treated as an unkeyed operation") },
                )
                .await,
            )
            .await;
            assert_eq!(response.0, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(response.2["data"]["code"], "invalid_input");
        }
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.idempotency")
            .fetch_one(&first.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    })
    .await;
}
