//! Same-reason folding through independent service states and real PostgreSQL transactions.
//! All accounts/providers are local fixtures; each test owns and drops its database.

mod common;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::*;
use extend_protocol::DeviceOs;
use extend_service::config::Tuning;
use extend_service::error::AppResult;
use extend_service::iam::{Principal, TestingSelection};
use extend_service::ting::{LocalNotifier, Notifier};
use futures::FutureExt as _;
use serde_json::{Value, json};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

struct PausedNotifier {
    inner: Arc<LocalNotifier>,
    pause: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}

#[async_trait]
impl Notifier for PausedNotifier {
    async fn send_frozen(&self, actor: &Principal, body: &Value, sel: Option<&TestingSelection>) -> AppResult<()> {
        if body["type"] == "extend.device.requested" && self.pause.swap(false, Ordering::SeqCst) {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        self.inner.send_frozen(actor, body, sel).await
    }

    async fn register_recipient(&self, p: &Principal, force: bool, sel: Option<&TestingSelection>) -> AppResult<()> {
        self.inner.register_recipient(p, force, sel).await
    }
}

struct Fixture {
    envs: Vec<Env>,
    names: Vec<String>,
    servers: Vec<JoinHandle<()>>,
    original_pools: Vec<sqlx::PgPool>,
    observer: sqlx::PgPool,
    notifier: Arc<PausedNotifier>,
    url: String,
    data: std::path::PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let (url, data) = database("request_fold").await;
        let observer = PgPoolOptions::new().max_connections(3).connect(&url).await.unwrap();
        let mut envs = Vec::new();
        let mut names = Vec::new();
        let mut servers = Vec::new();
        let mut original_pools = Vec::new();
        let mut notifier = None;
        for index in 0..2 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let mut state = extend_service::build(config(
                url.clone(),
                addr,
                data.join(index.to_string()),
                Tuning::default(),
            ))
            .await
            .unwrap();
            // Independent one-connection route pools make accidental nested acquisitions and
            // connections held during retry/provider waits observable rather than cosmetic.
            let name = format!("request-fold-{index}-{}", uuid::Uuid::new_v4());
            let options: PgConnectOptions = url.parse().unwrap();
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(options.application_name(&name))
                .await
                .unwrap();
            let unique = Arc::get_mut(&mut state).unwrap();
            original_pools.push(std::mem::replace(&mut unique.pool, pool.clone()));
            if index == 0 {
                let paused = Arc::new(PausedNotifier {
                    inner: unique.local_ting.clone().unwrap(),
                    pause: AtomicBool::new(false),
                    entered: Semaphore::new(0),
                    release: Semaphore::new(0),
                });
                unique.notifier = paused.clone();
                notifier = Some(paused);
            }
            let versions = extend_service::versions::Registry::start(
                pool.clone(),
                extend_service::versions::Policy::from_env().unwrap(),
            )
            .await
            .unwrap();
            let app = extend_service::routes::router(state.clone(), versions)
                .into_make_service_with_connect_info::<std::net::SocketAddr>();
            // The real HTTP router, without unrelated scheduler passes in either process.
            servers.push(tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }));
            let base = format!("http://{addr}");
            let client = silicon_extend_client::Client::connect(&base).await.unwrap();
            envs.push(Env {
                base,
                pool,
                client,
                state,
            });
            names.push(name);
        }
        sqlx::raw_sql(
            "CREATE FUNCTION extend.request_insert_gate() RETURNS trigger LANGUAGE plpgsql AS $$
               BEGIN PERFORM pg_advisory_xact_lock(7342112, 1); RETURN NEW; END $$;
             CREATE TRIGGER request_insert_gate BEFORE INSERT ON extend.requests
               FOR EACH ROW EXECUTE FUNCTION extend.request_insert_gate();",
        )
        .execute(&observer)
        .await
        .unwrap();
        Self {
            envs,
            names,
            servers,
            original_pools,
            observer,
            notifier: notifier.unwrap(),
            url,
            data,
        }
    }

    async fn gate(&self) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut gate = self.observer.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(7342112, 1)")
            .execute(&mut *gate)
            .await
            .unwrap();
        gate
    }

    async fn first_at_insert(&self) {
        eventually("first request blocked inside the real INSERT", || async {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE application_name = $1
                 AND wait_event = 'advisory' AND query LIKE '%INSERT INTO extend.requests%')",
            )
            .bind(&self.names[0])
            .fetch_one(&self.observer)
            .await
            .unwrap()
        })
        .await;
    }

    async fn second_released_its_connection(&self) {
        // ROLLBACK remains this named, otherwise idle connection's last query during the
        // backoff. Seeing it proves the second real handler reached the contention path;
        // no sleep-based assumption that its HTTP request has started is needed.
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT state, query FROM pg_stat_activity WHERE application_name = $1")
                    .bind(&self.names[1])
                    .fetch_all(&self.observer)
                    .await
                    .unwrap();
            assert!(
                rows.iter().all(|(_, q)| !q.contains("INSERT INTO extend.requests")),
                "second handler reached INSERT before the first committed: folding is not atomic"
            );
            if rows.iter().any(|(state, query)| state == "idle" && query == "ROLLBACK") {
                break;
            }
            assert!(
                Instant::now() < until,
                "second handler did not release its contended transaction: {rows:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::timeout(Duration::from_millis(500), self.envs[1].pool.acquire())
            .await
            .expect("a contended request must release the only pooled connection")
            .unwrap();
    }

    async fn close(self) {
        for server in self.servers {
            server.abort();
            let _ = server.await;
        }
        for env in self.envs {
            env.pool.close().await;
        }
        for pool in self.original_pools {
            pool.close().await;
        }
        self.observer.close().await;
        let (admin, name) = self.url.rsplit_once('/').unwrap();
        assert!(name.starts_with("extend_request_fold_"));
        let mut conn = sqlx::PgConnection::connect(&format!("{admin}/postgres")).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {name} WITH (FORCE)")))
            .execute(&mut conn)
            .await
            .unwrap();
        if self.data.exists() {
            std::fs::remove_dir_all(self.data).unwrap();
        }
    }

    async fn ting_count(&self) -> usize {
        let mut count = 0;
        for env in &self.envs {
            count += env
                .state
                .local_ting
                .as_ref()
                .unwrap()
                .sent_of("device.requested")
                .await
                .len();
        }
        count
    }
}

fn send(env: &Env, token: &str, device: &str, reason: &str, key: Option<&str>) -> JoinHandle<(u16, Value)> {
    let mut request = reqwest::Client::new()
        .post(format!("{}/api/v1/devices/{device}/requests", env.base))
        .bearer_auth(token)
        .header("x-org-id", "acme")
        .json(&json!({"type": "request", "data": {"reason": reason}}));
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    tokio::spawn(async move {
        let response = request.send().await.unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    })
}

async fn finish(task: JoinHandle<(u16, Value)>) -> (u16, Value) {
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
}

async fn occupied(f: &Fixture) -> (String, String, String, App) {
    let a = &f.envs[0];
    let alice = login(a, "c:alice").await;
    let chef = login(a, "si:chef").await;
    let sous_a = login(a, "si:sous").await;
    let sous_b = login(&f.envs[1], "si:sous").await;
    let (device, credential) = pair(
        a,
        &alice,
        Some("acme"),
        DeviceOs::Android,
        "Phone",
        &["si:chef", "si:sous"],
    )
    .await;
    let app = App::connect(a, &credential, hello(DeviceOs::Android, "1.1.0")).await;
    let (status, body) = session(a, &chef, "acme", &device).await;
    assert_eq!(status, 201, "{body}");
    (device, sous_a, sous_b, app)
}

#[tokio::test]
async fn simultaneous_reasons_fold_across_states_with_distinct_or_absent_keys() {
    let f = Fixture::new().await;
    let result = AssertUnwindSafe(async {
        let (device, sous_a, sous_b, _app) = occupied(&f).await;
        for (index, keys) in [(None, None), (Some("independent-key-a"), Some("independent-key-b"))]
            .into_iter()
            .enumerate()
        {
            let raw = format!("  Need it now {index}\n");
            let gate = f.gate().await;
            f.notifier.pause.store(true, Ordering::SeqCst);
            let first = send(&f.envs[0], &sous_a, &device, &raw, keys.0);
            f.first_at_insert().await;
            let second = send(&f.envs[1], &sous_b, &device, &raw, keys.1);
            f.second_released_its_connection().await;
            gate.commit().await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), f.notifier.entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            // First delivery is deliberately paused. The committed row and the sole connection
            // must already be available, and the competing handler must finish without it.
            let (status, folded) = finish(second).await;
            assert_eq!(status, 200, "{folded}");
            assert_eq!(folded["data"]["reason"], raw);
            assert_eq!(f.ting_count().await, index);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.requests WHERE reason = $1")
                .bind(&raw)
                .fetch_one(&f.envs[0].pool)
                .await
                .unwrap();
            assert_eq!(count, 1);
            assert!(!first.is_finished(), "provider remains paused outside the transaction");
            f.notifier.release.add_permits(1);
            let (status, created) = finish(first).await;
            assert_eq!(status, 201, "{created}");
            assert_eq!(created["data"]["request_id"], folded["data"]["request_id"]);
            assert_eq!(f.ting_count().await, index + 1);
        }
        // Trimming for validation must not collapse distinct raw reason values.
        let (status, trimmed) = finish(send(&f.envs[1], &sous_b, &device, "Need it now 0", None)).await;
        assert_eq!(status, 201, "{trimmed}");
        assert_eq!(f.ting_count().await, 3);
    })
    .catch_unwind()
    .await;
    f.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn contended_reason_wait_is_bounded_and_retry_folds_after_release() {
    let f = Fixture::new().await;
    let result = AssertUnwindSafe(async {
        let (device, sous_a, sous_b, _app) = occupied(&f).await;
        let gate = f.gate().await;
        let first = send(&f.envs[0], &sous_a, &device, "Bounded request", None);
        f.first_at_insert().await;
        let started = Instant::now();
        let second = send(
            &f.envs[1],
            &sous_b,
            &device,
            "Bounded request",
            Some("bounded-attempt-key"),
        );
        f.second_released_its_connection().await;
        let (status, busy) = finish(second).await;
        assert_eq!(status, 503, "{busy}");
        assert_eq!(busy["data"]["code"], "service_unavailable");
        assert!(started.elapsed() < Duration::from_secs(8));
        assert_eq!(f.ting_count().await, 0);
        gate.commit().await.unwrap();
        let (status, created) = finish(first).await;
        assert_eq!(status, 201, "{created}");
        // A definite refusal is retained under its key. A new attempt uses a fresh key;
        // it can now fold the committed request without another insert or notification.
        let (status, replayed) = finish(send(
            &f.envs[1],
            &sous_b,
            &device,
            "Bounded request",
            Some("bounded-attempt-key"),
        ))
        .await;
        assert_eq!(status, 503, "{replayed}");
        let (status, folded) = finish(send(
            &f.envs[1],
            &sous_b,
            &device,
            "Bounded request",
            Some("bounded-retry-key"),
        ))
        .await;
        assert_eq!(status, 200, "{folded}");
        assert_eq!(created["data"]["request_id"], folded["data"]["request_id"]);
        assert_eq!(f.ting_count().await, 1);
    })
    .catch_unwind()
    .await;
    f.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn cross_pair_routing_uses_the_same_transaction_connection() {
    let f = Fixture::new().await;
    let result = AssertUnwindSafe(async {
        let env = &f.envs[0];
        let alice = login(env, "c:alice").await;
        let bob = login(env, "c:bob").await;
        let chef = login(env, "si:chef").await;
        let sous = login(env, "si:sous").await;
        let (holder_pair, credential) = pair(
            env,
            &alice,
            Some("acme"),
            DeviceOs::Android,
            "Alice phone",
            &["si:sous"],
        )
        .await;
        let _app = App::connect(env, &credential, hello(DeviceOs::Android, "1.1.0")).await;
        let (requester_pair, _) = pair_another(env, &credential, &bob, Some("acme"), &["si:chef"]).await;
        let (status, held) = session(env, &sous, "acme", &holder_pair).await;
        assert_eq!(status, 201, "{held}");
        let (status, request) = finish(send(env, &chef, &requester_pair, "Need the shared phone", None)).await;
        assert_eq!(status, 201, "{request}");
        assert_eq!(request["data"]["to_hidden"], true);
        assert!(request["data"].get("session_id").is_none());
        let tings = env.state.local_ting.as_ref().unwrap().sent_of("device.requested").await;
        assert_eq!(tings.len(), 1);
        assert_eq!(tings[0]["for"], "c:alice");
        assert_eq!(tings[0]["data"]["device_id"], holder_pair);
    })
    .catch_unwind()
    .await;
    f.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
