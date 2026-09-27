//! 1.1 schema (world version 4) on a live 1.0 database, rows 1.0.0 writes after a rollback, and
//! the down step deploy/rollback/1.1-to-1.0.sql with the roll-forward that follows (test_plan 18).

mod common;

use extend_service::db::{self, World};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> PgPool {
    let (url, _) = common::database("mig").await;
    let pool = db::connect(&url).await.unwrap();
    db::migrate_global(&pool).await.unwrap();
    pool
}

async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{e}: {sql}"));
}

async fn one<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin>(
    pool: &PgPool,
    sql: &str,
) -> T {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{e}: {sql}"))
}

#[tokio::test]
async fn a_1_0_world_upgrades_and_keeps_rows_1_0_writes_consistent() {
    let pool = pool().await;
    let world = World::test(Uuid::new_v4());
    let s = world.schema.clone();
    db::ensure_world_to(&pool, &world, 3).await.unwrap();
    assert_eq!(
        one::<i32>(
            &pool,
            &format!("SELECT version FROM extend_global.schema_versions WHERE schema_name = '{s}'")
        )
        .await,
        3
    );
    // 1.0 data: a team-visible device and a personal one, grants, a running session with its
    // lock, activity, and a request.
    exec(&pool, &format!("
        INSERT INTO {s}.devices (device_id, team, owner_id, name, os, visibility, state) VALUES
            ('aaaa0001', 'acme', 'c:alice', 'TV', 'android_tv', 'team', 'ready'),
            ('aaaa0002', 'labs', 'c:bob', 'Mac', 'macos', 'personal', 'ready');
        INSERT INTO {s}.device_access (device_id, silicon_id, granted_by) VALUES ('aaaa0001', 'si:chef', 'c:alice'), ('aaaa0002', 'si:sous', 'c:bob');
        INSERT INTO {s}.session_ids VALUES ('a3f');
        INSERT INTO {s}.sessions (session_id, device_id, silicon_id, team, state) VALUES ('a3f', 'aaaa0001', 'si:chef', 'acme', 'active');
        INSERT INTO {s}.device_locks (device_id, session_id) VALUES ('aaaa0001', 'a3f');
        INSERT INTO {s}.activity (id, device_id, actor_kind, actor_id, action, session_id) VALUES
            ('{a1}', 'aaaa0001', 'silicon', 'si:chef', 'command', 'a3f'),
            ('{a2}', 'aaaa0001', 'carbon', 'c:alice', 'access_granted', NULL),
            ('{a3}', 'aaaa0001', 'carbon', 'c:alice', 'renamed', NULL);
        INSERT INTO {s}.requests (request_id, device_id, team, from_id, to_id, session_id, reason) VALUES
            ('{r1}', 'aaaa0001', 'acme', 'si:sous', 'si:chef', 'a3f', 'please');",
        a1 = Uuid::now_v7(), a2 = Uuid::now_v7(), a3 = Uuid::now_v7(), r1 = Uuid::now_v7())).await;
    db::ensure_world(&pool, &world).await.unwrap();
    assert_eq!(
        one::<i32>(
            &pool,
            &format!("SELECT version FROM extend_global.schema_versions WHERE schema_name = '{s}'")
        )
        .await,
        4
    );
    // Each device is its own instance, with its own side salt; every device is personal.
    assert_eq!(
        one::<i64>(&pool, &format!("SELECT count(DISTINCT instance_id) FROM {s}.devices")).await,
        2
    );
    assert_eq!(
        one::<i64>(
            &pool,
            &format!("SELECT count(*) FROM {s}.device_instances WHERE length(side_salt) = 64")
        )
        .await,
        2
    );
    assert_eq!(
        one::<i64>(
            &pool,
            &format!("SELECT count(*) FROM {s}.devices WHERE visibility <> 'personal'")
        )
        .await,
        0
    );
    assert!(one::<bool>(&pool, &format!("SELECT bool_and(first_pair) FROM {s}.devices")).await);
    // Grants got the device's Team; the lock its instance.
    assert_eq!(
        one::<String>(
            &pool,
            &format!("SELECT team FROM {s}.device_access WHERE silicon_id = 'si:sous'")
        )
        .await,
        "labs"
    );
    assert!(
        one::<bool>(
            &pool,
            &format!(
                "SELECT l.instance_id = d.instance_id FROM {s}.device_locks l JOIN {s}.devices d USING (device_id)"
            )
        )
        .await
    );
    // Activity: from the session, or the device's Team for grant rows; others stay NULL.
    let teams: Vec<(String, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT action, team FROM {s}.activity ORDER BY id"
    )))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        teams,
        vec![
            ("command".into(), Some("acme".into())),
            ("access_granted".into(), Some("acme".into())),
            ("renamed".into(), None)
        ]
    );
    // Requests: the holder columns.
    let r: (String, String, String, String, String) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT holder_device_id, holder_team, holder_session_id, ting_team, routed_to FROM {s}.requests"
    )))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        r,
        (
            "aaaa0001".into(),
            "acme".into(),
            "a3f".into(),
            "acme".into(),
            "holder".into()
        )
    );
    assert_eq!(
        one::<i64>(
            &pool,
            &format!("SELECT count(*) FROM {s}.world_settings WHERE name = 'hardware_salt'")
        )
        .await,
        1
    );

    // Rows as 1.0.0 writes them after a rollback.
    exec(&pool, &format!("
        INSERT INTO {s}.devices (device_id, team, owner_id, name, os, visibility) VALUES ('aaaa0003', 'acme', 'c:alice', 'New', 'android', 'team');
        INSERT INTO {s}.device_access (device_id, silicon_id, granted_by) VALUES ('aaaa0003', 'si:chef', 'c:alice');
        INSERT INTO {s}.session_ids VALUES ('b4e');
        INSERT INTO {s}.sessions (session_id, device_id, silicon_id, team, state) VALUES ('b4e', 'aaaa0003', 'si:chef', 'acme', 'active');
        INSERT INTO {s}.device_locks (device_id, session_id) VALUES ('aaaa0003', 'b4e');
        INSERT INTO {s}.requests (request_id, device_id, team, from_id, to_id, session_id, reason) VALUES ('{r2}', 'aaaa0003', 'acme', 'si:sous', 'si:chef', 'b4e', 'again');
        UPDATE {s}.devices SET visibility = 'team' WHERE device_id = 'aaaa0002';", r2 = Uuid::now_v7())).await;
    assert_eq!(
        one::<String>(
            &pool,
            &format!("SELECT visibility FROM {s}.devices WHERE device_id = 'aaaa0003'")
        )
        .await,
        "personal"
    );
    assert_eq!(
        one::<String>(
            &pool,
            &format!("SELECT visibility FROM {s}.devices WHERE device_id = 'aaaa0002'")
        )
        .await,
        "personal"
    );
    assert!(one::<bool>(&pool, &format!(
        "SELECT EXISTS (SELECT 1 FROM {s}.device_instances i JOIN {s}.devices d USING (instance_id) WHERE d.device_id = 'aaaa0003')")).await);
    assert_eq!(
        one::<String>(
            &pool,
            &format!("SELECT team FROM {s}.device_access WHERE device_id = 'aaaa0003'")
        )
        .await,
        "acme"
    );
    assert!(
        one::<bool>(
            &pool,
            &format!("SELECT instance_id IS NOT NULL FROM {s}.device_locks WHERE device_id = 'aaaa0003'")
        )
        .await
    );
    let r: (String, String, String, String) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT holder_device_id, holder_team, holder_session_id, ting_team FROM {s}.requests WHERE device_id = 'aaaa0003'")))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(r, ("aaaa0003".into(), "acme".into(), "b4e".into(), "acme".into()));
}

/// 1.0.0's queries, as it runs them.
mod v1_0 {
    use sqlx::PgPool;

    pub async fn access_of(pool: &PgPool, s: &str, device: &str, silicon: &str) -> bool {
        sqlx::query_scalar::<_, i32>(sqlx::AssertSqlSafe(format!(
            "SELECT 1 FROM {s}.device_access WHERE device_id = $1 AND silicon_id = $2"
        )))
        .bind(device)
        .bind(silicon)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
    }

    pub async fn my_requests(pool: &PgPool, s: &str, team: &str, me: &str) -> Vec<(String, Option<String>)> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT to_id, session_id FROM {s}.requests WHERE team = $1 AND (from_id = $2 OR to_id = $2) ORDER BY request_id DESC")))
            .bind(team)
            .bind(me)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    pub async fn requests_for_device(pool: &PgPool, s: &str, device: &str) -> Vec<(String, String, Option<String>)> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT from_id, to_id, session_id FROM {s}.requests WHERE device_id = $1 ORDER BY request_id DESC"
        )))
        .bind(device)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    pub async fn retry_requests(pool: &PgPool, s: &str) -> Vec<String> {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT r.to_id FROM {s}.requests r JOIN {s}.devices d USING (device_id)
             WHERE r.delivery = 'pending' AND r.attempts < 6 ORDER BY r.created_at LIMIT 200"
        )))
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// The revoke route: delete, and log when something was deleted.
    pub async fn revoke(pool: &PgPool, s: &str, device: &str, silicon: &str) {
        let n = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {s}.device_access WHERE device_id = $1 AND silicon_id = $2"
        )))
        .bind(device)
        .bind(silicon)
        .execute(pool)
        .await
        .unwrap()
        .rows_affected();
        if n > 0 {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {s}.activity (id, device_id, actor_kind, actor_id, action, details) VALUES ($1, $2, 'carbon', 'c:alice', 'access_revoked', $3)")))
                .bind(uuid::Uuid::now_v7())
                .bind(device)
                .bind(serde_json::json!({"silicon_id": silicon}))
                .execute(pool)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn the_down_step_keeps_1_0_safe_and_the_roll_forward_restores_only_untouched_grants() {
    let pool = pool().await;
    let s = "extend";
    let instance = Uuid::new_v4();
    exec(&pool, &format!("
        INSERT INTO {s}.devices (device_id, team, owner_id, name, os, state, instance_id, next_credential_digest) VALUES
            ('bbbb0001', 'acme', 'c:alice', 'Family TV', 'android_tv', 'ready', '{instance}', 'pending:bbbb0001'),
            ('bbbb0002', 'acme', 'c:bob', 'TV', 'android_tv', 'ready', '{instance}', NULL);
        INSERT INTO {s}.device_access (device_id, team, silicon_id, granted_by) VALUES
            ('bbbb0001', 'acme', 'si:chef', 'c:alice'), ('bbbb0001', 'globex', 'si:chef', 'c:alice'),
            ('bbbb0001', 'globex', 'si:scout', 'c:alice');
        INSERT INTO {s}.session_ids VALUES ('c5d');
        INSERT INTO {s}.sessions (session_id, device_id, silicon_id, team, state) VALUES ('c5d', 'bbbb0001', 'si:scout', 'globex', 'active');
        INSERT INTO {s}.device_locks (device_id, session_id, instance_id) VALUES ('bbbb0001', 'c5d', '{instance}');
        INSERT INTO {s}.requests (request_id, device_id, team, from_id, to_id, session_id, reason, routed_to, routed_to_id,
                                  holder_device_id, holder_team, holder_session_id, ting_team) VALUES
            ('{r}', 'bbbb0002', 'acme', 'si:chef', '{hidden}', NULL, 'routed', 'carbon', 'c:alice', 'bbbb0001', 'globex', 'c5d', 'globex');
        INSERT INTO {s}.wake_requests (wake_id, device_id, instance_id, team, from_id, to_id, reason, expires_at, wake_detectable, device_notice)
            VALUES ('{w}', 'bbbb0001', '{instance}', 'acme', 'si:chef', 'c:alice', 'wake', now() + interval '30 minutes', true, 'sent');
        INSERT INTO extend_global.enrollments (enrollment_id, secret_digest, os, app_version, pairing_code, code_expires_at, instance_id, from_device_id)
            VALUES ('{e}', 'digest', 'android_tv', '1.1.0', 'ABC123', now() + interval '5 minutes', '{instance}', 'bbbb0001');",
        r = Uuid::now_v7(), w = Uuid::now_v7(), e = Uuid::now_v7(), hidden = extend_protocol::REQUEST_TO_HIDDEN)).await;
    let down = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/rollback/1.1-to-1.0.sql"
    ))
    .unwrap();
    exec(&pool, &down).await;
    // Run twice: no harm.
    exec(&pool, &down).await;

    // 1.0.0's access check: scout's globex grant is gone (set aside), and its session ended.
    assert!(!v1_0::access_of(&pool, s, "bbbb0001", "si:scout").await);
    assert!(
        v1_0::access_of(&pool, s, "bbbb0001", "si:chef").await,
        "the device's own Team's grant stays"
    );
    let (state, reason): (String, String) =
        sqlx::query_as("SELECT state, end_reason FROM extend.sessions WHERE session_id = 'c5d'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), reason.as_str()), ("ended", "access_removed"));
    assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.device_locks").await, 0);
    // No other Carbon's id or holder session in what 1.0.0 shows, and nothing routed is retried.
    for (to, session) in v1_0::my_requests(&pool, s, "acme", "si:chef").await {
        assert_eq!((to.as_str(), session), (extend_protocol::REQUEST_TO_HIDDEN, None));
    }
    for (_, to, session) in v1_0::requests_for_device(&pool, s, "bbbb0002").await {
        assert!(!to.contains("c:alice") && session.is_none());
    }
    assert!(v1_0::retry_requests(&pool, s).await.is_empty());
    assert_eq!(
        one::<String>(&pool, "SELECT delivery FROM extend.requests").await,
        "failed"
    );
    // Wake requests withdrawn; a waiting "Pair with another Carbon" code deleted; the unconfirmed
    // rotation dropped.
    assert_eq!(
        one::<String>(&pool, "SELECT end_reason FROM extend.wake_requests").await,
        "rollback"
    );
    assert_eq!(
        one::<i64>(&pool, "SELECT count(*) FROM extend_global.enrollments").await,
        0
    );
    assert_eq!(
        one::<i64>(
            &pool,
            "SELECT count(*) FROM extend.devices WHERE next_credential_digest IS NOT NULL"
        )
        .await,
        0
    );

    // While rolled back, the Carbon revokes chef (1.0.0 deletes its acme grant and logs it).
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    v1_0::revoke(&pool, s, "bbbb0001", "si:chef").await;
    // 1.1.0 starts again: scout's grant comes back, logged; chef's doesn't.
    db::ensure_world(&pool, &World::production()).await.unwrap();
    let grants: Vec<(String, String)> =
        sqlx::query_as("SELECT silicon_id, team FROM extend.device_access ORDER BY silicon_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(grants, vec![("si:scout".into(), "globex".into())]);
    let logged: Value = sqlx::query_scalar("SELECT details FROM extend.activity WHERE action = 'access_granted'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logged["restored_after_rollback"], json!(true));
    assert_eq!(logged["silicon_id"], "si:scout");
    let stash: Option<String> = sqlx::query_scalar("SELECT to_regclass('extend.rollback_1_1_grants')::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stash.is_none(), "the stash is dropped");
}
