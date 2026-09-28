//! 1.1 schema (world version 5) on a live 1.0 database, rows 1.0.0 writes after a rollback, and
//! the down step deploy/rollback/1.1-to-1.0.sql with the roll-forward that follows (test_plan 18).

mod common;

use extend_service::db::{self, World};
use futures::FutureExt as _;
use serde_json::{Value, json};
use sqlx::{Connection as _, PgPool};
use uuid::Uuid;

/// Each rehearsal owns one database. Clean it even if an assertion panics; never sweep databases
/// created by other tests or processes sharing the local PostgreSQL instance.
async fn with_database<F, Fut>(run: F)
where
    F: FnOnce(PgPool) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (url, _) = common::database("mig").await;
    let pool = db::connect(&url).await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        db::migrate_global(&pool).await.unwrap();
        run(pool.clone()).await;
    })
    .catch_unwind()
    .await;
    pool.close().await;
    let (admin, name) = url.rsplit_once('/').unwrap();
    assert!(name.starts_with("extend_mig_") && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    let mut conn = sqlx::PgConnection::connect(&format!("{admin}/postgres")).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&mut conn)
        .await
        .unwrap();
    eprintln!("Cleaned owned migration database {name}");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
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
    with_database(|pool| async move {
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
        5
    );
    assert!(
        one::<bool>(
            &pool,
            &format!("SELECT bool_and(in_use_indicator = 'shown') FROM {s}.device_instances")
        )
        .await,
        "existing devices keep showing their indicator after the additive migration"
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
    }).await;
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
    with_database(|pool| async move {
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
    }).await;
}

#[tokio::test]
async fn repeated_rollback_and_forward_preserve_schema5_indicators_and_world_isolation() {
    with_database(|pool| async move {
        let worlds = [World::production(), World::test(Uuid::new_v4()), World::test(Uuid::new_v4())];
        db::ensure_world_to(&pool, &worlds[1], 4).await.unwrap();
        db::ensure_world(&pool, &worlds[2]).await.unwrap();
        let mut identities = Vec::new();
        for (index, world) in worlds.iter().enumerate() {
            let s = &world.schema;
            let shared = Uuid::new_v4();
            exec(&pool, &format!(r#"
                INSERT INTO {s}.devices (device_id, team, owner_id, name, os, state, instance_id, first_pair, credential_digest, next_credential_digest) VALUES
                    ('cccc0001', 'acme', 'c:alice', 'Shared TV', 'android_tv', 'ready', '{shared}', true, 'confirmed-alice', 'pending-alice'),
                    ('cccc0002', 'acme', 'c:bob', 'Shared TV', 'android_tv', 'ready', '{shared}', false, 'confirmed-bob', NULL);
                INSERT INTO {s}.devices (device_id, team, owner_id, name, os, state) VALUES
                    ('cccc0003', 'acme', 'c:alice', 'Removed later', 'macos', 'ready'),
                    ('cccc0004', 'acme', 'c:alice', 'Own-team TV', 'android_tv', 'ready');
                INSERT INTO {s}.device_access (device_id, team, silicon_id, granted_by, granted_at, last_used_at, wake_muted) VALUES
                    ('cccc0001', 'acme', 'si:chef', 'c:alice', '2026-09-01T00:00:00Z', NULL, false),
                    ('cccc0001', 'globex', 'si:chef', 'c:alice', '2026-09-01T00:00:00Z', NULL, false),
                    ('cccc0001', 'globex', 'si:scout', 'c:alice', '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', true),
                    ('cccc0003', 'globex', 'si:scout', 'c:alice', '2026-09-01T00:00:00Z', NULL, false),
                    ('cccc0004', 'acme', 'si:chef', 'c:alice', '2026-09-01T00:00:00Z', NULL, false);
                INSERT INTO {s}.session_ids VALUES ('c01'), ('c02');
                INSERT INTO {s}.sessions (session_id, device_id, silicon_id, team, state) VALUES
                    ('c01', 'cccc0001', 'si:scout', 'globex', 'active'),
                    ('c02', 'cccc0004', 'si:chef', 'acme', 'active');
                INSERT INTO {s}.device_locks (device_id, session_id) VALUES ('cccc0001', 'c01'), ('cccc0004', 'c02');
                INSERT INTO {s}.wake_requests (wake_id, device_id, instance_id, team, from_id, to_id, reason, expires_at, wake_detectable, device_notice)
                    VALUES ('{wake}', 'cccc0001', '{shared}', 'acme', 'si:chef', 'c:alice', 'wake', now() + interval '30 minutes', true, 'sent');
                INSERT INTO extend_global.enrollments (enrollment_id, secret_digest, os, app_version, pairing_code, code_expires_at, instance_id, from_device_id)
                    VALUES ('{join}', 'join-{index}', 'android_tv', '1.1.0', 'AA0{index}01', now() + interval '5 minutes', '{shared}', 'cccc0001'),
                           ('{first}', 'first-{index}', 'android', '1.0.0', 'AA0{index}02', now() + interval '5 minutes', NULL, NULL);
            "#, wake = Uuid::now_v7(), join = Uuid::now_v7(), first = Uuid::now_v7())).await;
            if index != 1 {
                exec(&pool, &format!("UPDATE {s}.device_instances SET in_use_indicator = 'hidden' WHERE instance_id = '{shared}'")).await;
            }
            // These identities and salts must never be regenerated by rollback or migration.
            let rows: Value = one(&pool, &format!("SELECT jsonb_agg(jsonb_build_array(d.device_id, d.instance_id, d.first_pair, i.side_salt) ORDER BY d.device_id) FROM {s}.devices d JOIN {s}.device_instances i USING (instance_id)")).await;
            let salt: String = one(&pool, &format!("SELECT value FROM {s}.world_settings WHERE name = 'hardware_salt'")).await;
            identities.push((rows, salt));
        }
        let down = include_str!("../../../deploy/rollback/1.1-to-1.0.sql");
        for cycle in 0..3 {
            exec(&pool, down).await;
            exec(&pool, down).await;
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend_global.enrollments").await, 3, "only first-pair enrollments survive");
            for (index, world) in worlds.iter().enumerate() {
                let s = &world.schema;
                assert_eq!(one::<i64>(&pool, &format!("SELECT count(*) FROM {s}.rollback_1_1_grants")).await, if cycle == 0 { 3 } else { 1 });
                assert_eq!(one::<String>(&pool, &format!("SELECT state FROM {s}.sessions WHERE session_id = 'c01'")).await, "ended");
                assert_eq!(one::<String>(&pool, &format!("SELECT state FROM {s}.sessions WHERE session_id = 'c02'")).await, "active");
                assert_eq!(one::<i64>(&pool, &format!("SELECT count(*) FROM {s}.device_locks WHERE session_id = 'c02'")).await, 1);
                assert_eq!(one::<String>(&pool, &format!("SELECT state FROM {s}.wake_requests")).await, "withdrawn");
                assert_eq!(one::<String>(&pool, &format!("SELECT credential_digest FROM {s}.devices WHERE device_id = 'cccc0001'")).await, "confirmed-alice");
                assert_eq!(one::<i64>(&pool, &format!("SELECT count(*) FROM {s}.devices WHERE next_credential_digest IS NOT NULL")).await, 0);

                if cycle == 0 {
                    v1_0::revoke(&pool, s, "cccc0001", "si:chef").await;
                    exec(&pool, &format!("UPDATE {s}.devices SET removed_at = now(), removed_reason = 'pair_revoked' WHERE device_id = 'cccc0003'")).await;
                }
                // Writes copied from 1.0.0: no new columns in INSERT, including its grant upsert.
                let new = format!("dddd000{cycle}");
                let session = format!("d0{cycle}");
                exec(&pool, &format!(r#"
                    INSERT INTO {s}.devices (device_id, team, owner_id, name, os, visibility) VALUES ('{new}', 'acme', 'c:alice', 'Added by 1.0', 'android', 'team');
                    INSERT INTO {s}.device_access (device_id, silicon_id, granted_by) VALUES ('{new}', 'si:chef', 'c:alice') ON CONFLICT DO NOTHING;
                    INSERT INTO {s}.device_access (device_id, silicon_id, granted_by) VALUES ('{new}', 'si:chef', 'c:alice') ON CONFLICT DO NOTHING;
                    INSERT INTO {s}.session_ids VALUES ('{session}');
                    INSERT INTO {s}.sessions (session_id, device_id, silicon_id, team, state) VALUES ('{session}', '{new}', 'si:chef', 'acme', 'active');
                    INSERT INTO {s}.device_locks (device_id, session_id) VALUES ('{new}', '{session}');
                    UPDATE {s}.devices SET visibility = 'team', name = 'Renamed by 1.0' WHERE device_id = 'cccc0001';
                "#)).await;
                assert!(v1_0::access_of(&pool, s, &new, "si:chef").await);
                assert!(!v1_0::access_of(&pool, s, "cccc0001", "si:scout").await);
                assert_eq!(one::<String>(&pool, &format!("SELECT visibility FROM {s}.devices WHERE device_id = 'cccc0001'")).await, "personal");
                // 1.0's migration starter sees version 4/5 and leaves it intact.
                db::ensure_world_to(&pool, world, 3).await.unwrap();
                db::ensure_world(&pool, world).await.unwrap();
                db::ensure_world(&pool, world).await.unwrap();
                assert_eq!(one::<i32>(&pool, &format!("SELECT version FROM extend_global.schema_versions WHERE schema_name = '{s}'")).await, 5);
                let current: Value = one(&pool, &format!("SELECT jsonb_agg(jsonb_build_array(d.device_id, d.instance_id, d.first_pair, i.side_salt) ORDER BY d.device_id) FROM {s}.devices d JOIN {s}.device_instances i USING (instance_id) WHERE d.device_id LIKE 'cccc%'")).await;
                assert_eq!(current, identities[index].0);
                assert_eq!(one::<String>(&pool, &format!("SELECT value FROM {s}.world_settings WHERE name = 'hardware_salt'")).await, identities[index].1);
                let indicator = if index == 1 { "shown" } else { "hidden" };
                assert_eq!(one::<String>(&pool, &format!("SELECT i.in_use_indicator FROM {s}.devices d JOIN {s}.device_instances i USING (instance_id) WHERE d.device_id = 'cccc0002'")).await, indicator);
                assert_eq!(one::<String>(&pool, &format!("SELECT i.in_use_indicator FROM {s}.devices d JOIN {s}.device_instances i USING (instance_id) WHERE d.device_id = '{new}'")).await, "shown");
                assert_eq!(one::<i64>(&pool, &format!("SELECT count(*) FROM {s}.device_access WHERE team = 'globex'")).await, 1);
                assert!(one::<bool>(&pool, &format!("SELECT wake_muted AND granted_at = '2026-09-01T00:00:00Z' AND last_used_at = '2026-09-02T00:00:00Z' FROM {s}.device_access WHERE device_id = 'cccc0001' AND silicon_id = 'si:scout'")).await);
                assert_eq!(one::<i64>(&pool, &format!("SELECT count(*) FROM {s}.activity WHERE details->>'restored_after_rollback' = 'true'")).await, cycle + 1);
                assert!(one::<Option<String>>(&pool, &format!("SELECT to_regclass('{s}.rollback_1_1_grants')::text")).await.is_none());
                // New-column constraints remain live; old-service writes cannot break them.
                let invalid = sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {s}.device_instances SET in_use_indicator = 'dimmed'"))).execute(&pool).await;
                assert_eq!(invalid.unwrap_err().as_database_error().and_then(|e| e.code()).as_deref(), Some("23514"));
            }
        }
    }).await;
}

#[tokio::test]
async fn concurrent_roll_forward_serializes_the_grant_stash_restore() {
    with_database(|pool| async move {
        exec(&pool, r#"
            INSERT INTO extend.devices (device_id, team, owner_id, name, os) VALUES ('eeee0001', 'acme', 'c:alice', 'TV', 'android_tv');
            INSERT INTO extend.device_access (device_id, team, silicon_id, granted_by) VALUES ('eeee0001', 'globex', 'si:scout', 'c:alice');
        "#).await;
        exec(&pool, include_str!("../../../deploy/rollback/1.1-to-1.0.sql")).await;
        // Hold only the stash's table lock. The first restorer reaches DROP after inserting the
        // grant, and waits here; a second startup then reaches the same restore concurrently.
        let mut gate = pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE extend.rollback_1_1_grants IN ACCESS SHARE MODE").execute(&mut *gate).await.unwrap();
        let first_pool = pool.clone();
        let first = tokio::spawn(async move { db::ensure_world(&first_pool, &World::production()).await });
        let first_waiting = wait_for_restore_waiters(&pool, 1).await;
        let second_pool = pool.clone();
        // This startup already checked schema versions before the first acquired the restore
        // lock. It must re-check the stash after waiting, even though it saw it on entry.
        let second = tokio::spawn(async move { db::restore_rollback_grants(&second_pool, &World::production()).await });
        let both_waiting = wait_for_restore_waiters(&pool, 2).await;
        // Release before asserting so even a failed assertion cannot leave a blocked connection.
        gate.commit().await.unwrap();
        let (first, second) = tokio::join!(first, second);
        assert!(first_waiting && both_waiting, "both startup attempts reached the controlled overlap");
        assert!(first.as_ref().is_ok_and(|r| r.is_ok()), "first startup: {first:?}");
        assert!(second.as_ref().is_ok_and(|r| r.is_ok()), "second startup: {second:?}");
        assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.device_access WHERE device_id = 'eeee0001' AND team = 'globex'").await, 1);
        assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.activity WHERE details->>'restored_after_rollback' = 'true'").await, 1);
        assert!(one::<Option<String>>(&pool, "SELECT to_regclass('extend.rollback_1_1_grants')::text").await.is_none());
    }).await;
}

/// Opt-in because its input must be an independently captured 1.0 schema, not a schema rebuilt
/// from this checkout. The runner imports it with psql before this test and owns all cleanup.
#[tokio::test]
#[ignore = "run deploy/rollback/rehearse-schema.py with a captured schema and its SHA-256"]
async fn copied_production_schema_rolls_backward_and_forward() {
    let url = std::env::var("EXTEND_MIGRATION_SCHEMA_DATABASE_URL").expect("use the owned-container rehearsal runner");
    let parsed = url::Url::parse(&url).unwrap();
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    let name = parsed.path().trim_start_matches('/');
    assert!(name.starts_with("extend_copy_") && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    let pool = db::connect(&url).await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        assert_eq!(one::<i32>(&pool, "SELECT version FROM extend_global.schema_versions WHERE schema_name = 'extend'").await, 3);
        assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.devices").await, 0, "schema copy contains no device data");
        assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend_global.test_environments").await, 0);
        assert!(one::<Option<String>>(&pool, "SELECT to_regclass('extend.device_instances')::text").await.is_none());

        // These are synthetic records written through 1.0's columns before any current migration.
        exec(&pool, r#"
            INSERT INTO extend.devices (device_id, team, owner_id, name, os, visibility, state, credential_digest) VALUES
                ('cafe0001', 'acme', 'c:alice', 'Synthetic TV', 'android_tv', 'team', 'ready', 'synthetic-confirmed-tv'),
                ('cafe0002', 'acme', 'c:alice', 'Synthetic Mac', 'macos', 'personal', 'ready', 'synthetic-confirmed-mac');
            INSERT INTO extend.device_access (device_id, silicon_id, granted_by, granted_at) VALUES
                ('cafe0001', 'si:chef', 'c:alice', '2026-09-01T00:00:00Z'),
                ('cafe0002', 'si:chef', 'c:alice', '2026-09-01T00:00:00Z');
            INSERT INTO extend.session_ids VALUES ('f01');
            INSERT INTO extend.sessions (session_id, device_id, silicon_id, team, state) VALUES ('f01', 'cafe0001', 'si:chef', 'acme', 'active');
            INSERT INTO extend.device_locks (device_id, session_id) VALUES ('cafe0001', 'f01');
        "#).await;
        db::migrate_global(&pool).await.unwrap();
        assert_eq!(one::<i32>(&pool, "SELECT version FROM extend_global.schema_versions WHERE schema_name = 'extend'").await, 5);
        assert_eq!(one::<i64>(&pool, "SELECT count(DISTINCT instance_id) FROM extend.devices").await, 2);
        assert!(one::<bool>(&pool, "SELECT bool_and(visibility = 'personal' AND first_pair) FROM extend.devices").await);
        assert!(one::<bool>(&pool, "SELECT bool_and(in_use_indicator = 'shown' AND length(side_salt) = 64) FROM extend.device_instances").await);
        assert!(one::<bool>(&pool, "SELECT bool_and(team = 'acme' AND granted_at = '2026-09-01T00:00:00Z') FROM extend.device_access").await);
        assert!(one::<bool>(&pool, "SELECT l.instance_id = d.instance_id FROM extend.device_locks l JOIN extend.devices d USING (device_id)").await);
        assert_eq!(one::<String>(&pool, "SELECT credential_digest FROM extend.devices WHERE device_id = 'cafe0001'").await, "synthetic-confirmed-tv");
        assert_eq!(one::<String>(&pool, "SELECT credential_digest FROM extend.devices WHERE device_id = 'cafe0002'").await, "synthetic-confirmed-mac");
        eprintln!("Copied production schema: version 3 -> 5 with original synthetic 1.0 credentials, grants, sessions and locks retained");

        // Add 1.1's second Carbon pair and cross-Team grants, then choose a hidden indicator.
        exec(&pool, r#"
            INSERT INTO extend.devices (device_id, team, owner_id, name, os, state, instance_id, first_pair, credential_digest)
                SELECT 'cafe0003', 'acme', 'c:bob', name, os, state, instance_id, false, 'synthetic-confirmed-alias'
                FROM extend.devices WHERE device_id = 'cafe0002';
            UPDATE extend.device_instances SET in_use_indicator = 'hidden'
                WHERE instance_id = (SELECT instance_id FROM extend.devices WHERE device_id = 'cafe0002');
            INSERT INTO extend.device_access (device_id, team, silicon_id, granted_by, granted_at, last_used_at, wake_muted) VALUES
                ('cafe0002', 'globex', 'si:chef', 'c:alice', '2026-09-01T00:00:00Z', NULL, false),
                ('cafe0002', 'globex', 'si:scout', 'c:alice', '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', true);
            INSERT INTO extend.session_ids VALUES ('f02');
            INSERT INTO extend.sessions (session_id, device_id, silicon_id, team, state) VALUES ('f02', 'cafe0002', 'si:scout', 'globex', 'active');
            INSERT INTO extend.device_locks (device_id, session_id) VALUES ('cafe0002', 'f02');
        "#).await;
        let identity_sql = "SELECT jsonb_agg(jsonb_build_array(d.device_id, d.instance_id, d.first_pair, d.credential_digest, i.side_salt, i.in_use_indicator) ORDER BY d.device_id) FROM extend.devices d JOIN extend.device_instances i USING (instance_id) WHERE d.device_id LIKE 'cafe%'";
        let identities: Value = one(&pool, identity_sql).await;
        let hardware_salt: String = one(&pool, "SELECT value FROM extend.world_settings WHERE name = 'hardware_salt'").await;
        for cycle in 0..3 {
            exec(&pool, "UPDATE extend.devices SET next_credential_digest = 'synthetic-unconfirmed' WHERE device_id = 'cafe0002'").await;
            exec(&pool, include_str!("../../../deploy/rollback/1.1-to-1.0.sql")).await;
            exec(&pool, include_str!("../../../deploy/rollback/1.1-to-1.0.sql")).await;
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.rollback_1_1_grants").await, if cycle == 0 { 2 } else { 1 });
            assert_eq!(one::<Value>(&pool, identity_sql).await, identities);
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.devices WHERE next_credential_digest IS NOT NULL").await, 0);
            assert_eq!(one::<String>(&pool, "SELECT state FROM extend.sessions WHERE session_id = 'f01'").await, "active");
            assert_eq!(one::<String>(&pool, "SELECT state FROM extend.sessions WHERE session_id = 'f02'").await, "ended");
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.device_locks WHERE session_id = 'f01'").await, 1);
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.device_locks WHERE session_id = 'f02'").await, 0);
            assert!(!v1_0::access_of(&pool, "extend", "cafe0002", "si:scout").await);
            if cycle == 0 {
                v1_0::revoke(&pool, "extend", "cafe0002", "si:chef").await;
            }
            let new = format!("beef000{cycle}");
            exec(&pool, &format!(r#"
                INSERT INTO extend.devices (device_id, team, owner_id, name, os, visibility, credential_digest)
                    VALUES ('{new}', 'acme', 'c:alice', 'Synthetic 1.0 insert', 'android', 'team', 'synthetic-rollback-{cycle}');
                INSERT INTO extend.device_access (device_id, silicon_id, granted_by) VALUES ('{new}', 'si:chef', 'c:alice') ON CONFLICT DO NOTHING;
                INSERT INTO extend.device_access (device_id, silicon_id, granted_by) VALUES ('{new}', 'si:chef', 'c:alice') ON CONFLICT DO NOTHING;
            "#)).await;
            assert!(one::<bool>(&pool, &format!("SELECT d.visibility = 'personal' AND d.first_pair AND i.in_use_indicator = 'shown' FROM extend.devices d JOIN extend.device_instances i USING (instance_id) WHERE d.device_id = '{new}'")).await);
            assert!(v1_0::access_of(&pool, "extend", &new, "si:chef").await);

            db::migrate_global(&pool).await.unwrap();
            db::migrate_global(&pool).await.unwrap();
            assert_eq!(one::<Value>(&pool, identity_sql).await, identities);
            assert_eq!(one::<String>(&pool, "SELECT value FROM extend.world_settings WHERE name = 'hardware_salt'").await, hardware_salt);
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.device_access WHERE team = 'globex'").await, 1);
            assert!(!v1_0::access_of(&pool, "extend", "cafe0002", "si:chef").await, "revoked grants never resurrect");
            assert!(one::<bool>(&pool, "SELECT wake_muted AND granted_at = '2026-09-01T00:00:00Z' AND last_used_at = '2026-09-02T00:00:00Z' FROM extend.device_access WHERE device_id = 'cafe0002' AND silicon_id = 'si:scout'").await);
            assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.activity WHERE details->>'restored_after_rollback' = 'true'").await, cycle + 1);
            assert!(one::<Option<String>>(&pool, "SELECT to_regclass('extend.rollback_1_1_grants')::text").await.is_none());
            assert_eq!(one::<i32>(&pool, "SELECT version FROM extend_global.schema_versions WHERE schema_name = 'extend'").await, 5);
            eprintln!("Copied production schema: cycle {} passed (down twice, forward twice, exact grants/credentials/instances/salts/indicator retained)", cycle + 1);
        }
    }).catch_unwind().await;
    pool.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn wait_for_restore_waiters(pool: &PgPool, count: i64) -> bool {
    for _ in 0..150 {
        let waiting = one::<i64>(
            pool,
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .await;
        if waiting >= count {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}
