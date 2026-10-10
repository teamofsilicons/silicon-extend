//! Schema version 9 (4.0, Silicon Accounts, no Teams) on an empty database and on a 3.1 database
//! with data, and `extend-service identity apply` / `link-identities`, which re-keys old ids to
//! Silicon Accounts uuids. Nothing is ever dropped: rows that only existed once per Team are merged
//! and their extra copies archived, and every re-keyed column keeps its original value.

mod common;

use extend_service::db::{self, World};
use extend_service::identity;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Schema version 8: what origin/main (3.1) brings a database to. Its migrations are byte-for-byte
/// the ones this build still carries.
const SCHEMA_3_1: usize = 8;

async fn database() -> (String, PgPool) {
    let (url, _) = common::database("acmig").await;
    let pool = db::connect(&url).await.unwrap();
    (url, pool)
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

async fn version(pool: &PgPool) -> i32 {
    one(
        pool,
        "SELECT version FROM extend_global.schema_versions WHERE schema_name = 'extend'",
    )
    .await
}

#[tokio::test]
async fn an_empty_database_migrates_to_4_0() {
    let (_, pool) = database().await;
    db::migrate_global(&pool).await.unwrap();
    assert_eq!(version(&pool).await, db::WORLD_VERSION);
    assert_eq!(db::WORLD_VERSION, 9);
    for table in [
        "accounts",
        "accounts_events",
        "identity_links",
        "identity_link_runs",
        "proof_grants",
        "ting_enrolments",
        "ting_types",
        "device_access_archive",
    ] {
        let found: Option<String> = one(&pool, &format!("SELECT to_regclass('extend.{table}')::text")).await;
        assert!(found.is_some(), "extend.{table} exists");
    }
    // One grant per pair and Silicon, and no Team needed on new rows.
    let pk: String = one(
        &pool,
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'device_access_pkey'",
    )
    .await;
    assert_eq!(pk, "PRIMARY KEY (device_id, silicon_id)");
    exec(
        &pool,
        "INSERT INTO extend.devices (device_id, owner_id, name, os) VALUES ('aaaa0001', 'Al1ceUu', 'Pixel', 'android')",
    )
    .await;
    exec(&pool, "INSERT INTO extend.device_access (device_id, silicon_id, granted_by) VALUES ('aaaa0001', 'Ch3fUuid', 'Al1ceUu')").await;
    let team: Option<String> = one(&pool, "SELECT team FROM extend.device_access").await;
    assert_eq!(team, None);
    assert_eq!(one::<String>(&pool, "SELECT team FROM extend.devices").await, "");
    // Running it again changes nothing.
    db::migrate_global(&pool).await.unwrap();
    assert_eq!(version(&pool).await, 9);
}

/// A 3.1 database with Team-scoped rows, as origin/main writes them.
async fn a_3_1_database() -> PgPool {
    let (_, pool) = database().await;
    db::migrate_global_schema(&pool).await.unwrap();
    db::ensure_world_to(&pool, &World::production(), SCHEMA_3_1)
        .await
        .unwrap();
    assert_eq!(version(&pool).await, SCHEMA_3_1 as i32);
    let instance = Uuid::new_v4();
    exec(&pool, &format!(r#"
        INSERT INTO extend.devices (device_id, team, owner_id, name, os, state, instance_id, first_pair, credential_digest) VALUES
            ('dddd0001', 'acme', 'c:alice', 'Family TV', 'android_tv', 'ready', '{instance}', true, 'cred-alice'),
            ('dddd0002', 'globex', 'c:bob', 'TV', 'android_tv', 'ready', '{instance}', false, 'cred-bob');
        INSERT INTO extend.devices (device_id, team, owner_id, name, os, state, credential_digest) VALUES
            ('dddd0003', 'globex', 'c:alice', 'Mac', 'macos', 'ready', 'cred-mac');
        UPDATE extend.device_organizations SET wake_muted = true WHERE device_id = 'dddd0001';
        INSERT INTO extend.device_access (device_id, team, silicon_id, granted_by, granted_at, last_used_at, wake_muted) VALUES
            ('dddd0001', 'acme',   'si:chef',  'c:alice', '2026-09-01T00:00:00Z', NULL, false),
            ('dddd0001', 'globex', 'si:chef',  'c:alice', '2026-09-02T00:00:00Z', '2026-09-03T00:00:00Z', true),
            ('dddd0001', 'globex', 'si:scout', 'c:alice', '2026-09-01T00:00:00Z', NULL, false),
            ('dddd0003', 'globex', 'si:sous',  'c:alice', '2026-09-01T00:00:00Z', NULL, false);
        INSERT INTO extend.session_ids VALUES ('a01'), ('a02');
        INSERT INTO extend.sessions (session_id, device_id, silicon_id, team, state, end_reason) VALUES
            ('a01', 'dddd0001', 'si:chef', 'acme', 'active', NULL),
            ('a02', 'dddd0003', 'si:sous', 'globex', 'ended', 'ended_by_silicon');
        INSERT INTO extend.device_locks (device_id, session_id) VALUES ('dddd0001', 'a01');
        INSERT INTO extend.activity (id, device_id, actor_kind, actor_id, action, session_id, team) VALUES
            ('{a1}', 'dddd0001', 'carbon', 'c:alice', 'paired', NULL, 'acme'),
            ('{a2}', 'dddd0001', 'silicon', 'si:chef', 'command', 'a01', 'acme');
        INSERT INTO extend.requests (request_id, device_id, team, from_id, to_id, session_id, reason, delivery) VALUES
            ('{r1}', 'dddd0001', 'acme', 'si:sous', 'si:chef', 'a01', 'pending one', 'pending'),
            ('{r2}', 'dddd0001', 'acme', 'si:sous', 'si:chef', 'a01', 'delivered one', 'delivered');
        INSERT INTO extend.wake_requests (wake_id, device_id, instance_id, team, from_id, to_id, reason, expires_at, wake_detectable, device_notice, last_asked_at) VALUES
            ('{w1}', 'dddd0001', '{instance}', 'acme',   'si:chef', 'c:alice', 'older', now() + interval '20 minutes', true, 'sent', now() - interval '5 minutes'),
            ('{w2}', 'dddd0002', '{instance}', 'globex', 'si:chef', 'c:bob',   'newer', now() + interval '25 minutes', true, 'sent', now());
        INSERT INTO extend.files (file_id, team, device_id, session_id, created_by, shared_with, name, kind, content_type, size_bytes, url) VALUES
            ('{f1}', 'acme', 'dddd0001', 'a01', 'si:chef', 'c:alice', 'shot.png', 'screenshot', 'image/png', 4, 'https://briefcase.example/x');
    "#,
        a1 = Uuid::now_v7(), a2 = Uuid::now_v7(), r1 = Uuid::now_v7(), r2 = Uuid::now_v7(),
        w1 = Uuid::now_v7(), w2 = Uuid::now_v7(), f1 = Uuid::now_v7(),
    )).await;
    pool
}

/// Every row of the tables that hold data, as JSON, ordered: what must survive.
async fn rows(pool: &PgPool, table: &str, order: &str) -> Value {
    one(
        pool,
        &format!("SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY {order}), '[]') FROM extend.{table} t"),
    )
    .await
}

#[tokio::test]
async fn a_3_1_database_upgrades_to_4_0_and_keeps_every_row() {
    let pool = a_3_1_database().await;
    let count = |t: &str| format!("SELECT count(*) FROM extend.{t}");
    let mut counts = Vec::new();
    for t in [
        "devices",
        "sessions",
        "activity",
        "requests",
        "wake_requests",
        "files",
        "device_organizations",
        "device_instances",
    ] {
        counts.push((t, one::<i64>(&pool, &count(t)).await));
    }
    let grants_before = one::<i64>(&pool, &count("device_access")).await;
    let devices_before = rows(&pool, "devices", "device_id").await;

    db::migrate_global(&pool).await.unwrap();
    assert_eq!(version(&pool).await, 9);
    for (t, n) in counts {
        assert_eq!(one::<i64>(&pool, &count(t)).await, n, "every row of extend.{t} stays");
    }
    // One grant per pair and Silicon: the earliest stays, with the latest use and "muted" if any
    // copy was; the other copy is archived, not dropped.
    let grants = one::<i64>(&pool, &count("device_access")).await;
    let archived = one::<i64>(&pool, &count("device_access_archive")).await;
    assert_eq!((grants, archived, grants + archived), (3, 1, grants_before));
    let chef: (String, bool, Option<time::OffsetDateTime>) = sqlx::query_as(
        "SELECT team, wake_muted, last_used_at FROM extend.device_access WHERE device_id = 'dddd0001' AND silicon_id = 'si:chef'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((chef.0.as_str(), chef.1), ("acme", true));
    assert!(chef.2.is_some());
    assert_eq!(
        one::<String>(&pool, "SELECT team FROM extend.device_access_archive").await,
        "globex"
    );
    // The pair's "wake requests off" moved from its Team binding to the pair.
    assert!(
        one::<bool>(
            &pool,
            "SELECT wake_muted FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await
    );
    // One open wake request per device and Silicon: the latest ask stays open.
    let wakes: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT reason, state, end_reason FROM extend.wake_requests ORDER BY reason")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        wakes,
        vec![
            ("newer".into(), "open".into(), None),
            ("older".into(), "withdrawn".into(), Some("left_team".into()))
        ]
    );
    // Devices are untouched (credentials, instances, Teams kept as history).
    let devices_after = rows(&pool, "devices", "device_id").await;
    for (b, a) in devices_before
        .as_array()
        .unwrap()
        .iter()
        .zip(devices_after.as_array().unwrap())
    {
        for key in [
            "device_id",
            "team",
            "owner_id",
            "instance_id",
            "credential_digest",
            "name",
        ] {
            assert_eq!(b[key], a[key], "{key}");
        }
    }
    // Old ids stay as they are until an operator applies a mapping: they match no access token.
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "c:alice"
    );
    assert_eq!(
        identity::stored_old_ids(&pool).await.unwrap(),
        vec!["c:alice", "c:bob", "si:chef", "si:scout", "si:sous"]
    );
}

const MAPPING: &str = "# reviewed 2026-10-10
iam_public_id,accounts_uuid,current_id
c:alice,Al1ceUu,c:alice
c:bob,B0bUuid,c:robert
si:chef,Ch3fUuid,si:chef
si:sous,S0usUuid,
si:scout,,
";

#[tokio::test]
async fn identity_apply_rekeys_in_place_keeps_originals_and_can_be_redone() {
    let pool = a_3_1_database().await;
    db::migrate_global(&pool).await.unwrap();
    let links = identity::parse_mapping(MAPPING).unwrap();
    assert_eq!(links.len(), 4, "a line without a uuid stays unmapped");

    // A dry run reports and changes nothing.
    let report = identity::apply(&pool, &links, "mapping.csv", true).await.unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["columns"]["devices.owner_id"]["rows_changed"], 3);
    assert_eq!(
        report["columns"]["device_access.silicon_id"]["unmapped_ids"],
        serde_json::json!(["si:scout"])
    );
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "c:alice"
    );
    assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.identity_links").await, 0);

    let report = identity::apply(&pool, &links, "mapping.csv", false).await.unwrap();
    assert_eq!(
        (
            report["pending_requests_failed"].as_i64(),
            report["wake_requests_withdrawn"].as_i64()
        ),
        (Some(1), Some(0))
    );
    let owners: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT device_id, owner_id, owner_iam_id FROM extend.devices ORDER BY device_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        owners,
        vec![
            ("dddd0001".into(), "Al1ceUu".into(), Some("c:alice".into())),
            ("dddd0002".into(), "B0bUuid".into(), Some("c:bob".into())),
            ("dddd0003".into(), "Al1ceUu".into(), Some("c:alice".into())),
        ]
    );
    for (sql, want) in [
        (
            "SELECT silicon_id FROM extend.sessions WHERE session_id = 'a01'",
            "Ch3fUuid",
        ),
        (
            "SELECT actor_id FROM extend.activity WHERE action = 'command'",
            "Ch3fUuid",
        ),
        ("SELECT created_by FROM extend.files", "Ch3fUuid"),
        ("SELECT shared_with FROM extend.files", "Al1ceUu"),
        (
            "SELECT granted_by FROM extend.device_access WHERE silicon_id = 'Ch3fUuid'",
            "Al1ceUu",
        ),
        ("SELECT to_id FROM extend.wake_requests WHERE state = 'open'", "B0bUuid"),
        (
            "SELECT silicon_id FROM extend.device_access WHERE silicon_iam_id = 'si:scout'",
            "si:scout",
        ),
    ] {
        assert_eq!(one::<String>(&pool, sql).await, want, "{sql}");
    }
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT delivery FROM extend.requests WHERE reason = 'pending one'"
        )
        .await,
        "failed",
        "a notification addressed through Silicon IAM can't go now"
    );
    assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.identity_links").await, 4);
    assert_eq!(
        one::<String>(&pool, "SELECT id FROM extend.accounts WHERE uuid = 'B0bUuid'").await,
        "c:robert"
    );
    assert_eq!(
        one::<i64>(&pool, "SELECT count(*) FROM extend.identity_link_runs").await,
        1
    );

    // A corrected mapping re-derives everything from the originals.
    let fixed = identity::parse_mapping(&MAPPING.replace("c:alice,Al1ceUu", "c:alice,Al1ce2u")).unwrap();
    identity::apply(&pool, &fixed, "mapping-fixed.csv", false)
        .await
        .unwrap();
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "Al1ce2u"
    );
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_iam_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "c:alice"
    );
    assert_eq!(
        one::<i64>(
            &pool,
            "SELECT count(*) FROM extend.identity_links WHERE accounts_uuid = 'Al1ceUu'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn identity_apply_refuses_a_mapping_that_would_merge_two_pairs() {
    let pool = a_3_1_database().await;
    db::migrate_global(&pool).await.unwrap();
    // alice and bob each paired the TV: one account can't hold two pairs of one device.
    let merged = identity::parse_mapping("iam_public_id,accounts_uuid\nc:alice,SameUuid\nc:bob,SameUuid\n").unwrap();
    let e = identity::apply(&pool, &merged, "merged.csv", false)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("identity apply refused") && e.contains("pairs_of_one_device"),
        "{e}"
    );
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0002'"
        )
        .await,
        "c:bob"
    );
    assert_eq!(one::<i64>(&pool, "SELECT count(*) FROM extend.identity_links").await, 0);
    // Mapping files are checked line by line.
    for (bad, says) in [
        ("accounts_uuid\nAbc\n", "iam_public_id"),
        ("iam_public_id,accounts_uuid\nalice,Abc\n", "not a c:/si: id"),
        (
            "iam_public_id,accounts_uuid\nc:alice,not a uuid\n",
            "is not a Silicon Accounts uuid",
        ),
        (
            "iam_public_id,accounts_uuid\nc:alice,Abc\nc:alice,Def\n",
            "appears twice",
        ),
    ] {
        let e = identity::parse_mapping(bad).unwrap_err().to_string();
        assert!(e.contains(says), "{bad:?}: {e}");
    }
}

#[tokio::test]
async fn the_link_identities_command_reports_and_applies() {
    let pool = a_3_1_database().await;
    db::migrate_global(&pool).await.unwrap();
    let url: String = {
        let db: String = one(&pool, "SELECT current_database()::text").await;
        let admin = std::env::var("EXTEND_TEST_ADMIN_URL")
            .unwrap_or_else(|_| "postgres://extend:extend@127.0.0.1:5440/postgres".into());
        format!("{}/{db}", admin.rsplit_once('/').unwrap().0)
    };
    let dir = std::env::temp_dir().join(format!("extend-link-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("mapping.csv");
    std::fs::write(&file, MAPPING).unwrap();
    let run = |dry: bool| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_extend-service"));
        cmd.arg("link-identities").arg("--file").arg(&file);
        if dry {
            cmd.arg("--dry-run");
        }
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("EXTEND_ENVIRONMENT", "test")
            .env("EXTEND_DATABASE_URL", &url)
            .env("EXTEND_DATA_DIR", &dir)
            .env("EXTEND_LOG", "warn")
            .output()
            .unwrap()
    };
    let out = run(true);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (report["dry_run"].as_bool(), report["links"].as_i64()),
        (Some(true), Some(4))
    );
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "c:alice"
    );
    let out = run(false);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["dry_run"], false);
    assert_eq!(
        report["columns"]["sessions.silicon_id"]["unmapped_ids"],
        serde_json::json!([])
    );
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "Al1ceUu"
    );
    // Running it again is harmless.
    assert!(run(false).status.success());
    assert_eq!(
        one::<String>(
            &pool,
            "SELECT owner_id FROM extend.devices WHERE device_id = 'dddd0001'"
        )
        .await,
        "Al1ceUu"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
