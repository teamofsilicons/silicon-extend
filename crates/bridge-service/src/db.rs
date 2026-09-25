//! PostgreSQL: the global schema, and one schema per world (production, and each test environment).
//!
//! Worlds live in separate schemas so a query that forgets a filter still can't read another
//! world's data (TECHNICAL.md section 3). Schema names are generated here from UUIDs only, never
//! from caller input, which is what makes it safe to format them into SQL.

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Executor as _, Row as _};
use uuid::Uuid;

/// One isolated data plane.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct World {
    pub schema: String,
    pub environment_id: Option<Uuid>,
}

impl World {
    pub fn production() -> Self {
        Self { schema: "bridge".into(), environment_id: None }
    }
    pub fn test(environment_id: Uuid) -> Self {
        Self { schema: format!("bridge_test_{}", environment_id.simple()), environment_id: Some(environment_id) }
    }
    pub fn is_test(&self) -> bool {
        self.environment_id.is_some()
    }
    /// A qualified table name.
    pub fn t(&self, table: &str) -> String {
        format!("{}.{}", self.schema, table)
    }
}

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new().max_connections(32).connect(url).await?;
    Ok(pool)
}

const GLOBAL: &[&str] = &[
    r#"
CREATE SCHEMA IF NOT EXISTS bridge_global;
CREATE TABLE IF NOT EXISTS bridge_global.schema_versions (
    schema_name text PRIMARY KEY,
    version integer NOT NULL
);
CREATE TABLE IF NOT EXISTS bridge_global.enrollments (
    enrollment_id uuid PRIMARY KEY,
    secret_digest text NOT NULL UNIQUE,
    os text NOT NULL,
    os_version text,
    model text,
    app_version text NOT NULL,
    agent_device_version text,
    pairing_code text NOT NULL UNIQUE,
    code_expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    -- Set once the code is claimed; the credential is handed over once, then the row is deleted.
    paired_schema text,
    paired_device_id text,
    paired_credential text,
    paired_environment jsonb
);
CREATE TABLE IF NOT EXISTS bridge_global.test_environments (
    environment_id uuid PRIMARY KEY,
    org_id text NOT NULL,
    app_id text NOT NULL,
    name text NOT NULL,
    state text NOT NULL,
    environment_revision bigint NOT NULL,
    generation bigint NOT NULL,
    key_version bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_activity_at timestamptz NOT NULL DEFAULT now(),
    retired_at timestamptz
);
CREATE TABLE IF NOT EXISTS bridge_global.honeycomb_operations (
    environment_id uuid NOT NULL,
    operation_id uuid NOT NULL,
    request_hash text NOT NULL,
    receipt jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (environment_id, operation_id)
);
CREATE TABLE IF NOT EXISTS bridge_global.api_version_usage (
    api_version integer NOT NULL,
    day date NOT NULL,
    requests bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (api_version, day)
);
CREATE TABLE IF NOT EXISTS bridge_global.local_test_apps (
    secret_digest text PRIMARY KEY,
    environment_id uuid NOT NULL
);
"#,
];

/// Migrations for every world schema. `{s}` is replaced with the schema name.
const WORLD: &[&str] = &[
    r#"
CREATE SCHEMA IF NOT EXISTS {s};
CREATE TABLE IF NOT EXISTS {s}.devices (
    device_id text PRIMARY KEY,
    team text NOT NULL,
    owner_id text NOT NULL,
    name text NOT NULL,
    os text NOT NULL,
    os_version text,
    model text,
    address text,
    visibility text NOT NULL DEFAULT 'team',
    pair_ttl_days integer NOT NULL DEFAULT 14 CHECK (pair_ttl_days BETWEEN 1 AND 30),
    paired_at timestamptz NOT NULL DEFAULT now(),
    last_activity_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    last_seen_at timestamptz,
    version bigint NOT NULL DEFAULT 1,
    host_device_id text,
    state text NOT NULL DEFAULT 'setup',
    setup jsonb NOT NULL DEFAULT '{"state":"in_progress","steps":[]}',
    capabilities jsonb NOT NULL DEFAULT '[]',
    missing jsonb NOT NULL DEFAULT '[]',
    app_version text,
    agent_device_version text,
    credential_digest text UNIQUE,
    removed_at timestamptz,
    removed_reason text
);
CREATE INDEX IF NOT EXISTS devices_owner ON {s}.devices (team, owner_id) WHERE removed_at IS NULL;
CREATE TABLE IF NOT EXISTS {s}.device_access (
    device_id text NOT NULL REFERENCES {s}.devices(device_id),
    silicon_id text NOT NULL,
    granted_by text NOT NULL,
    granted_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    PRIMARY KEY (device_id, silicon_id)
);
CREATE INDEX IF NOT EXISTS device_access_silicon ON {s}.device_access (silicon_id);
CREATE TABLE IF NOT EXISTS {s}.session_ids (
    session_id text PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS {s}.sessions (
    session_id text PRIMARY KEY REFERENCES {s}.session_ids(session_id),
    device_id text NOT NULL REFERENCES {s}.devices(device_id),
    silicon_id text NOT NULL,
    team text NOT NULL,
    state text NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now(),
    last_command_at timestamptz,
    idle_ends_at timestamptz,
    ended_at timestamptz,
    end_reason text,
    command_count bigint NOT NULL DEFAULT 0,
    takeover jsonb
);
CREATE INDEX IF NOT EXISTS sessions_silicon ON {s}.sessions (silicon_id, started_at DESC);
CREATE INDEX IF NOT EXISTS sessions_device ON {s}.sessions (device_id, started_at DESC);
-- One Silicon at a time: the primary key is the lock.
CREATE TABLE IF NOT EXISTS {s}.device_locks (
    device_id text PRIMARY KEY REFERENCES {s}.devices(device_id),
    session_id text NOT NULL UNIQUE REFERENCES {s}.sessions(session_id)
);
CREATE TABLE IF NOT EXISTS {s}.activity (
    id uuid PRIMARY KEY,
    device_id text NOT NULL,
    at timestamptz NOT NULL DEFAULT now(),
    actor_kind text NOT NULL,
    actor_id text NOT NULL,
    action text NOT NULL,
    session_id text,
    command text,
    args jsonb,
    outcome text,
    files jsonb NOT NULL DEFAULT '[]',
    details jsonb
);
CREATE INDEX IF NOT EXISTS activity_device ON {s}.activity (device_id, at DESC, id DESC);
CREATE TABLE IF NOT EXISTS {s}.requests (
    request_id uuid PRIMARY KEY,
    device_id text NOT NULL,
    team text NOT NULL,
    from_id text NOT NULL,
    to_id text NOT NULL,
    session_id text,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    delivery text NOT NULL DEFAULT 'pending',
    attempts integer NOT NULL DEFAULT 0,
    last_error text
);
CREATE INDEX IF NOT EXISTS requests_device ON {s}.requests (device_id, created_at DESC);
CREATE TABLE IF NOT EXISTS {s}.files (
    file_id uuid PRIMARY KEY,
    team text NOT NULL,
    device_id text NOT NULL,
    session_id text,
    command_id uuid,
    created_by text NOT NULL,
    shared_with text,
    name text NOT NULL,
    kind text NOT NULL,
    content_type text NOT NULL,
    size_bytes bigint NOT NULL,
    url text NOT NULL,
    self_destruct_at timestamptz,
    permanent boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS files_session ON {s}.files (session_id);
CREATE TABLE IF NOT EXISTS {s}.uploads (
    upload_id uuid PRIMARY KEY,
    device_id text NOT NULL,
    command_id uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    received boolean NOT NULL DEFAULT false,
    name text,
    content_type text,
    size_bytes bigint
);
CREATE TABLE IF NOT EXISTS {s}.iam_events (
    event_id text PRIMARY KEY,
    received_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS {s}.idempotency (
    principal text NOT NULL,
    route text NOT NULL,
    key text NOT NULL,
    request_hash text NOT NULL,
    status integer NOT NULL,
    response jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (principal, route, key)
);
CREATE TABLE IF NOT EXISTS {s}.reports (
    report_id uuid PRIMARY KEY,
    member_id text NOT NULL,
    message text NOT NULL,
    pr text,
    client_version text NOT NULL,
    context jsonb,
    notification text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS {s}.telemetry (
    id bigserial PRIMARY KEY,
    member_id text,
    event jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
"#,
];

pub async fn migrate_global(pool: &PgPool) -> anyhow::Result<()> {
    // Serialise migrations across service instances.
    let mut conn = pool.acquire().await?;
    conn.execute("SELECT pg_advisory_lock(7342001)").await?;
    let result = async {
        for sql in GLOBAL {
            conn.execute(*sql).await?;
        }
        anyhow::Ok(())
    }
    .await;
    conn.execute("SELECT pg_advisory_unlock(7342001)").await?;
    result?;
    ensure_world(pool, &World::production()).await
}

/// Creates or upgrades a world's schema.
pub async fn ensure_world(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    conn.execute("SELECT pg_advisory_lock(7342002)").await?;
    let result = async {
        let current: i32 = sqlx::query("SELECT version FROM bridge_global.schema_versions WHERE schema_name = $1")
            .bind(&world.schema)
            .fetch_optional(&mut *conn)
            .await?
            .map(|r| r.get::<i32, _>(0))
            .unwrap_or(0);
        for (i, sql) in WORLD.iter().enumerate() {
            let version = i32::try_from(i + 1)?;
            if version <= current {
                continue;
            }
            let sql = sql.replace("{s}", &world.schema);
            conn.execute(sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))).await?;
            sqlx::query(
                "INSERT INTO bridge_global.schema_versions (schema_name, version) VALUES ($1, $2)
                 ON CONFLICT (schema_name) DO UPDATE SET version = EXCLUDED.version",
            )
            .bind(&world.schema)
            .bind(version)
            .execute(&mut *conn)
            .await?;
        }
        anyhow::Ok(())
    }
    .await;
    conn.execute("SELECT pg_advisory_unlock(7342002)").await?;
    result
}

/// Empties every table in a world (a test environment clean).
pub async fn truncate_world(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    anyhow::ensure!(world.is_test(), "refusing to truncate production");
    let sql = format!(
        "TRUNCATE {s}.device_locks, {s}.sessions, {s}.session_ids, {s}.device_access, {s}.activity, {s}.requests,
                  {s}.files, {s}.uploads, {s}.iam_events, {s}.idempotency, {s}.reports, {s}.telemetry, {s}.devices CASCADE",
        s = world.schema
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone())).execute(pool).await?;
    Ok(())
}

pub async fn drop_world(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    anyhow::ensure!(world.is_test(), "refusing to drop production");
    sqlx::raw_sql(sql!("DROP SCHEMA IF EXISTS {} CASCADE", world.schema)).execute(pool).await?;
    sqlx::query("DELETE FROM bridge_global.schema_versions WHERE schema_name = $1")
        .bind(&world.schema)
        .execute(pool)
        .await?;
    Ok(())
}
