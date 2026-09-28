//! PostgreSQL: the global schema, and one schema per world (production, and each test environment).
//!
//! Worlds live in separate schemas so a query that forgets a filter still can't read another
//! world's data (TECHNICAL.md section 3). Schema names are generated here from UUIDs only, never
//! from caller input, which is what makes it safe to format them into SQL.

use anyhow::Context as _;
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
        Self {
            schema: "extend".into(),
            environment_id: None,
        }
    }
    pub fn test(environment_id: Uuid) -> Self {
        Self {
            schema: format!("extend_test_{}", environment_id.simple()),
            environment_id: Some(environment_id),
        }
    }
    pub fn is_test(&self) -> bool {
        self.environment_id.is_some()
    }
    /// A qualified table name.
    pub fn t(&self, table: &str) -> String {
        format!("{}.{}", self.schema, table)
    }
}

/// Advisory lock that serialises every change to how many test environments are active, so the
/// limit of 10 holds under concurrent `prepare` and `restore` instructions.
pub const TEST_SLOT_LOCK: i64 = 7342003;

/// Takes the transaction-scoped advisory lock that serialises Honeycomb operations on one test
/// environment (held until `tx` ends, including when it's dropped).
pub async fn lock_environment(tx: &mut sqlx::PgConnection, environment_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 7342004))")
        .bind(format!("extend-test-environment:{environment_id}"))
        .execute(tx)
        .await?;
    Ok(())
}

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new().max_connections(32).connect(url).await?;
    Ok(pool)
}

const GLOBAL: &[&str] = &[
    r#"
CREATE SCHEMA IF NOT EXISTS extend_global;
CREATE TABLE IF NOT EXISTS extend_global.schema_versions (
    schema_name text PRIMARY KEY,
    version integer NOT NULL
);
CREATE TABLE IF NOT EXISTS extend_global.enrollments (
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
CREATE TABLE IF NOT EXISTS extend_global.test_environments (
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
CREATE TABLE IF NOT EXISTS extend_global.honeycomb_operations (
    environment_id uuid NOT NULL,
    operation_id uuid NOT NULL,
    request_hash text NOT NULL,
    receipt jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (environment_id, operation_id)
);
CREATE TABLE IF NOT EXISTS extend_global.api_version_usage (
    api_version integer NOT NULL,
    day date NOT NULL,
    requests bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (api_version, day)
);
CREATE TABLE IF NOT EXISTS extend_global.local_test_apps (
    secret_digest text PRIMARY KEY,
    environment_id uuid NOT NULL
);
"#,
    r#"
-- The world an enrollment was started in: its pairing code pairs a device into that world only.
ALTER TABLE extend_global.enrollments ADD COLUMN IF NOT EXISTS world_schema text NOT NULL DEFAULT 'extend';
ALTER TABLE extend_global.enrollments ADD COLUMN IF NOT EXISTS environment_id uuid;
CREATE INDEX IF NOT EXISTS enrollments_world ON extend_global.enrollments (world_schema) WHERE world_schema <> 'extend';
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'enrollments_claimed_in_own_world') THEN
        -- A backstop for the claim route's own check: a code never pairs into another world.
        ALTER TABLE extend_global.enrollments ADD CONSTRAINT enrollments_claimed_in_own_world
            CHECK (paired_schema IS NULL OR paired_schema = world_schema) NOT VALID;
    END IF;
END $$;
-- The operation that last moved the environment (a retry of it may repeat its revision), and the
-- digest of IAM's test webhook key, so signed test deliveries route after a restart.
ALTER TABLE extend_global.test_environments ADD COLUMN IF NOT EXISTS last_operation_id uuid;
ALTER TABLE extend_global.test_environments ADD COLUMN IF NOT EXISTS webhook_key_digest text;
CREATE INDEX IF NOT EXISTS test_environments_webhook_key ON extend_global.test_environments (webhook_key_digest)
    WHERE webhook_key_digest IS NOT NULL;
ALTER TABLE extend_global.honeycomb_operations ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT now();
-- The local IAM stand-in's view of a test application: active once IAM (Honeycomb) opened it.
ALTER TABLE extend_global.local_test_apps ADD COLUMN IF NOT EXISTS active boolean NOT NULL DEFAULT true;
-- Test application secrets (by digest) IAM has confirmed, and for which environment, so a secret
-- IAM refuses while its environment is being prepared or restored gets a precise answer.
CREATE TABLE IF NOT EXISTS extend_global.test_secret_bindings (
    secret_digest text PRIMARY KEY,
    environment_id uuid NOT NULL,
    confirmed_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS test_secret_bindings_env ON extend_global.test_secret_bindings (environment_id);
"#,
    r#"
-- 1.1.0: an enrollment started by an app that is already paired ("Pair with another Carbon") adds a
-- pair to that device.
ALTER TABLE extend_global.enrollments ADD COLUMN IF NOT EXISTS instance_id uuid;       -- NULL: a first pairing
ALTER TABLE extend_global.enrollments ADD COLUMN IF NOT EXISTS from_device_id text;    -- the pair whose credential started it
CREATE INDEX IF NOT EXISTS enrollments_instance ON extend_global.enrollments (instance_id) WHERE instance_id IS NOT NULL;
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
    r#"
ALTER TABLE {s}.telemetry ADD COLUMN IF NOT EXISTS exported_at timestamptz;
CREATE INDEX IF NOT EXISTS telemetry_pending ON {s}.telemetry (id) WHERE exported_at IS NULL;
"#,
    r#"
-- The newest version of each IAM aggregate applied here, so an older event arriving late is
-- dropped instead of undoing a newer one (TECHNICAL.md section 9).
CREATE TABLE IF NOT EXISTS {s}.iam_aggregates (
    aggregate_id text PRIMARY KEY,
    version bigint NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
);
"#,
    WORLD_1_1,
    WORLD_1_1_BANNER,
];

/// Schema version 4 (1.1.0): devices belong to the Carbons who paired them; several Carbons can
/// pair one device; waking. A `devices` row is one Carbon's pair; `device_instances` is the physical
/// device. The triggers keep rows any version writes (1.0.0 after a rollback too) consistent with
/// the 1.1 rules: personal devices, an instance for every device, a Team on every grant, one lock
/// per physical device, and the holder columns on every request (deploy/rollback/1.1-to-1.0.sql).
const WORLD_1_1: &str = r#"
CREATE TABLE IF NOT EXISTS {s}.device_instances (
    instance_id uuid PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Keys side tags; never leaves the service.
    side_salt text NOT NULL DEFAULT replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', ''),
    awake boolean,                 -- NULL: not reported since a socket of the device connected
    sleep_state text,              -- last report: screen_off|locked|asleep|standby|other_session
    awake_changed_at timestamptz,
    awake_run uuid,                -- the app run of the last awake frame applied
    awake_seq bigint,              -- and its sequence number
    last_wake_alert_at timestamptz -- the last wake notification that sounded on the device
);
-- Values that belong to the world itself, not to its test data: a clean keeps them.
CREATE TABLE IF NOT EXISTS {s}.world_settings (name text PRIMARY KEY, value text NOT NULL);
INSERT INTO {s}.world_settings (name, value)
    VALUES ('hardware_salt', replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', '')) ON CONFLICT DO NOTHING;

ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS instance_id uuid;
UPDATE {s}.devices SET instance_id = gen_random_uuid() WHERE instance_id IS NULL;
ALTER TABLE {s}.devices ALTER COLUMN instance_id SET DEFAULT gen_random_uuid();
ALTER TABLE {s}.devices ALTER COLUMN instance_id SET NOT NULL;
INSERT INTO {s}.device_instances (instance_id, created_at)
    SELECT instance_id, min(paired_at) FROM {s}.devices GROUP BY instance_id ON CONFLICT DO NOTHING;
-- Every devices row (from any version, 1.0 after a rollback too) is personal and has its instance row.
CREATE OR REPLACE FUNCTION {s}.devices_1_1() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    NEW.visibility := 'personal';
    IF TG_OP = 'INSERT' THEN
        INSERT INTO {s}.device_instances (instance_id) VALUES (NEW.instance_id) ON CONFLICT DO NOTHING;
    END IF;
    RETURN NEW; END $f$;
CREATE OR REPLACE TRIGGER devices_1_1 BEFORE INSERT OR UPDATE OF visibility ON {s}.devices
    FOR EACH ROW EXECUTE FUNCTION {s}.devices_1_1();
UPDATE {s}.devices SET visibility = 'personal' WHERE visibility <> 'personal';
CREATE INDEX IF NOT EXISTS devices_instance ON {s}.devices (instance_id) WHERE removed_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS devices_one_pair_per_carbon ON {s}.devices (instance_id, owner_id) WHERE removed_at IS NULL;
CREATE INDEX IF NOT EXISTS devices_owner_all ON {s}.devices (owner_id, device_id);
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS hardware_key text;          -- carried pairs: HMAC of the hardware id
CREATE INDEX IF NOT EXISTS devices_hardware_key ON {s}.devices (hardware_key) WHERE hardware_key IS NOT NULL AND removed_at IS NULL;
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS duplicate text;             -- a carried pair refused as a duplicate: 'own:<device_id>' | 'other_computer'
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS provisional_until timestamptz; -- test worlds: a carried pair over the limit, waiting to be linked
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS next_credential_digest text;   -- a rotated credential the app hasn't confirmed yet
CREATE UNIQUE INDEX IF NOT EXISTS devices_next_credential ON {s}.devices (next_credential_digest) WHERE next_credential_digest IS NOT NULL;
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS wake_muted boolean NOT NULL DEFAULT false;
-- The pair made by the app's first enrollment (the Carbon who installed Extend on the device), as
-- opposed to one added with "Pair with another Carbon". On a computer several Carbons paired, only
-- Silicons given access through it get the terminal (Carbon decision, 2026-09-27). Every 1.0 device
-- was its own first pairing, and so is every row 1.0.0 inserts after a rollback.
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS first_pair boolean NOT NULL DEFAULT true;

ALTER TABLE {s}.device_access ADD COLUMN IF NOT EXISTS team text;     -- the Silicon's Team
UPDATE {s}.device_access a SET team = d.team FROM {s}.devices d WHERE d.device_id = a.device_id AND a.team IS NULL;
CREATE OR REPLACE FUNCTION {s}.device_access_team() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    IF NEW.team IS NULL THEN SELECT d.team INTO NEW.team FROM {s}.devices d WHERE d.device_id = NEW.device_id; END IF;
    RETURN NEW; END $f$;
CREATE OR REPLACE TRIGGER device_access_team BEFORE INSERT ON {s}.device_access
    FOR EACH ROW EXECUTE FUNCTION {s}.device_access_team();
ALTER TABLE {s}.device_access ALTER COLUMN team SET NOT NULL;
ALTER TABLE {s}.device_access DROP CONSTRAINT IF EXISTS device_access_pkey;
ALTER TABLE {s}.device_access ADD PRIMARY KEY (device_id, team, silicon_id);
ALTER TABLE {s}.device_access ADD COLUMN IF NOT EXISTS wake_muted boolean NOT NULL DEFAULT false;
CREATE INDEX IF NOT EXISTS device_access_silicon_team ON {s}.device_access (silicon_id, team);
CREATE INDEX IF NOT EXISTS device_access_granter_team ON {s}.device_access (granted_by, team);

-- One Silicon at a time per physical device.
ALTER TABLE {s}.device_locks ADD COLUMN IF NOT EXISTS instance_id uuid;
UPDATE {s}.device_locks l SET instance_id = d.instance_id FROM {s}.devices d WHERE d.device_id = l.device_id AND l.instance_id IS NULL;
CREATE OR REPLACE FUNCTION {s}.device_locks_instance() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    IF NEW.instance_id IS NULL THEN SELECT d.instance_id INTO NEW.instance_id FROM {s}.devices d WHERE d.device_id = NEW.device_id; END IF;
    RETURN NEW; END $f$;
CREATE OR REPLACE TRIGGER device_locks_instance BEFORE INSERT ON {s}.device_locks
    FOR EACH ROW EXECUTE FUNCTION {s}.device_locks_instance();
ALTER TABLE {s}.device_locks ALTER COLUMN instance_id SET NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS device_locks_one_per_instance ON {s}.device_locks (instance_id);

ALTER TABLE {s}.activity ADD COLUMN IF NOT EXISTS team text;          -- the acting Silicon's Team; NULL for Carbon and device-level rows
UPDATE {s}.activity x SET team = s.team FROM {s}.sessions s WHERE s.session_id = x.session_id AND x.team IS NULL;
UPDATE {s}.activity x SET team = d.team FROM {s}.devices d WHERE d.device_id = x.device_id AND x.team IS NULL
    AND x.action IN ('access_granted','access_revoked','request_sent','request_failed');
CREATE INDEX IF NOT EXISTS activity_device_team ON {s}.activity (device_id, team, at DESC);

ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS routed_to text NOT NULL DEFAULT 'holder';  -- holder | carbon
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS routed_to_id text;      -- carbon rows: the Carbon who gave the holder access
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS holder_device_id text;  -- the pair the holder's session runs through
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS holder_team text;
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS holder_session_id text;
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS ting_team text;         -- org_id of the Ting
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS ting_body jsonb;        -- exact first body; NULL: first sent by 1.0.0
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS ting_next_at timestamptz;
UPDATE {s}.requests SET holder_device_id = device_id, holder_team = team, holder_session_id = session_id, ting_team = team
    WHERE holder_device_id IS NULL;
-- Rows any version inserts (1.0.0 after a rollback too) get the holder columns.
CREATE OR REPLACE FUNCTION {s}.requests_1_1() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    NEW.holder_device_id := COALESCE(NEW.holder_device_id, NEW.device_id);
    NEW.holder_team := COALESCE(NEW.holder_team, NEW.team);
    NEW.holder_session_id := COALESCE(NEW.holder_session_id, NEW.session_id);
    NEW.ting_team := COALESCE(NEW.ting_team, NEW.team);
    RETURN NEW; END $f$;
CREATE OR REPLACE TRIGGER requests_1_1 BEFORE INSERT ON {s}.requests
    FOR EACH ROW EXECUTE FUNCTION {s}.requests_1_1();
CREATE INDEX IF NOT EXISTS requests_routed ON {s}.requests (routed_to_id, holder_device_id, created_at DESC) WHERE routed_to = 'carbon';
CREATE INDEX IF NOT EXISTS requests_repeat ON {s}.requests (device_id, from_id, team, created_at DESC);

CREATE TABLE IF NOT EXISTS {s}.wake_requests (
    wake_id uuid PRIMARY KEY,
    device_id text NOT NULL REFERENCES {s}.devices(device_id),  -- the pair the Silicon asked through
    instance_id uuid NOT NULL,
    team text NOT NULL,            -- the asking Silicon's Team (its X-Org-ID, the grant's Team)
    from_id text NOT NULL,         -- the asking Silicon
    to_id text NOT NULL,           -- the Carbon who gave it access (the pair's owner)
    reason text NOT NULL,          -- the latest ask's reason, exactly as written
    created_at timestamptz NOT NULL DEFAULT now(),
    last_asked_at timestamptz NOT NULL DEFAULT now(),
    asks integer NOT NULL DEFAULT 1,
    expires_at timestamptz NOT NULL,                 -- last_asked_at + 1800 s
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open','woken','expired','withdrawn','declined')),
    ended_at timestamptz,
    end_reason text,
    wake_detectable boolean NOT NULL,
    device_notice text NOT NULL,   -- sent | shown | not_shown | offline | unsupported
    device_notice_note text,
    ting_delivery text,            -- NULL: covered by an earlier Carbon Ting (ting_covered_by); else pending|deferred|delivered|failed
    ting_covered_by uuid,
    ting_key text, ting_body jsonb, ting_attempts integer NOT NULL DEFAULT 0,
    ting_next_at timestamptz, ting_last_error text, ting_sent_at timestamptz,
    answer_ting text, answer_ting_body jsonb, answer_ting_attempts integer NOT NULL DEFAULT 0,
    answer_ting_next_at timestamptz, answer_ting_last_error text);
CREATE INDEX IF NOT EXISTS wake_requests_device ON {s}.wake_requests (device_id, created_at DESC);
CREATE INDEX IF NOT EXISTS wake_requests_instance_open ON {s}.wake_requests (instance_id) WHERE state = 'open';
CREATE INDEX IF NOT EXISTS wake_requests_asker ON {s}.wake_requests (from_id, team, instance_id, last_asked_at DESC);
CREATE INDEX IF NOT EXISTS wake_requests_carbon_tings ON {s}.wake_requests (to_id, ting_sent_at DESC) WHERE ting_sent_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS wake_requests_work ON {s}.wake_requests (state, ting_delivery, answer_ting);
CREATE UNIQUE INDEX IF NOT EXISTS wake_requests_one_open ON {s}.wake_requests (instance_id, team, from_id) WHERE state = 'open';

-- Whether Extend's Tings reach a member in a Team (Ting's grant is per app, Team and recipient).
CREATE TABLE IF NOT EXISTS {s}.ting_recipients (
    member_id text NOT NULL, team text NOT NULL,
    registered_at timestamptz, refused_at timestamptz, last_error text,
    PRIMARY KEY (member_id, team));
-- Extend's Ting types that Ting answered unknown in a Team (a row exists only while one is missing).
CREATE TABLE IF NOT EXISTS {s}.ting_type_status (
    team text NOT NULL, ting_type text NOT NULL,
    missing_since timestamptz NOT NULL, last_checked_at timestamptz NOT NULL, last_error text,
    PRIMARY KEY (team, ting_type));
-- IAM's answers about a Silicon's or a granting Carbon's membership of a Team (at-use checks and the sweep).
CREATE TABLE IF NOT EXISTS {s}.membership_checks (
    member_id text NOT NULL, team text NOT NULL,
    checked_at timestamptz NOT NULL,
    state text NOT NULL CHECK (state IN ('active','gone','unknown')),
    gone_since timestamptz,        -- first definite "not a member" not yet contradicted
    contradicted_at timestamptz,   -- a second reader said active after a Silicon reader said gone
    PRIMARY KEY (member_id, team));
"#;

/// Schema version 5 (1.1.0): whether the device shows the badge, banner or notification naming
/// the Silicon using it. One setting per physical device, so every pair of it shares it. Additive:
/// 1.0.0 never reads it after a rollback, and the instances its rows create get the default.
const WORLD_1_1_BANNER: &str = r#"
ALTER TABLE {s}.device_instances ADD COLUMN IF NOT EXISTS in_use_indicator text NOT NULL DEFAULT 'shown'
    CHECK (in_use_indicator IN ('shown','hidden'));
"#;

/// The schema version a 1.1.0 service brings every world to.
pub const WORLD_VERSION: i32 = 5;

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

/// Creates or upgrades a world's schema, then puts back the grants a rollback to 1.0.0 set aside
/// (see [`restore_rollback_grants`]).
pub async fn ensure_world(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    ensure_world_to(pool, world, WORLD.len()).await?;
    restore_rollback_grants(pool, world).await
}

/// Brings a world's schema up to `version` (tests build a 1.0.0 world with 3).
pub async fn ensure_world_to(pool: &PgPool, world: &World, version: usize) -> anyhow::Result<()> {
    let wanted = version.min(WORLD.len());
    let mut conn = pool.acquire().await?;
    conn.execute("SELECT pg_advisory_lock(7342002)").await?;
    let result = async {
        let current: i32 = sqlx::query("SELECT version FROM extend_global.schema_versions WHERE schema_name = $1")
            .bind(&world.schema)
            .fetch_optional(&mut *conn)
            .await?
            .map(|r| r.get::<i32, _>(0))
            .unwrap_or(0);
        for (i, sql) in WORLD.iter().enumerate().take(wanted) {
            let version = i32::try_from(i + 1)?;
            if version <= current {
                continue;
            }
            let sql = sql.replace("{s}", &world.schema);
            conn.execute(sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))).await?;
            sqlx::query(
                "INSERT INTO extend_global.schema_versions (schema_name, version) VALUES ($1, $2)
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

/// Empties every table in a world (a test environment clean). The IAM event and aggregate
/// records stay: they hold no test data, only which IAM events were already applied, so an event
/// IAM delivers again after the clean can't be applied a second time. `world_settings` stays too:
/// it belongs to the world (the hardware salt its computers hold), not to its test data.
pub async fn truncate_world(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    anyhow::ensure!(world.is_test(), "refusing to truncate production");
    let sql = format!(
        "TRUNCATE {s}.device_locks, {s}.sessions, {s}.session_ids, {s}.device_access, {s}.activity, {s}.requests,
                  {s}.files, {s}.uploads, {s}.idempotency, {s}.reports, {s}.telemetry, {s}.devices,
                  {s}.device_instances, {s}.wake_requests, {s}.ting_recipients, {s}.ting_type_status,
                  {s}.membership_checks CASCADE",
        s = world.schema
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone())).execute(pool).await?;
    Ok(())
}

pub async fn drop_world(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    anyhow::ensure!(world.is_test(), "refusing to drop production");
    sqlx::raw_sql(sql!("DROP SCHEMA IF EXISTS {} CASCADE", world.schema))
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM extend_global.schema_versions WHERE schema_name = $1")
        .bind(&world.schema)
        .execute(pool)
        .await?;
    Ok(())
}

/// Brings every test world that still exists (not permanently removed) to the current schema, at
/// start and before the scheduler runs: the scheduler walks every open test world, and device
/// sockets reach test worlds nobody has selected since the service started. Returns their schemas.
pub async fn ensure_test_worlds(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let envs: Vec<(Uuid,)> =
        sqlx::query_as("SELECT environment_id FROM extend_global.test_environments WHERE state <> 'removed'")
            .fetch_all(pool)
            .await?;
    let mut done = Vec::new();
    for (id,) in envs {
        let world = World::test(id);
        ensure_world(pool, &world).await?;
        done.push(world.schema);
    }
    Ok(done)
}

/// A rollback to 1.0.0 (deploy/rollback/1.1-to-1.0.sql) moves grants from Teams other than the
/// device's own Team into `rollback_1_1_grants`, because 1.0.0 would accept them in the device's
/// Team. Rolling forward puts each back only when nothing happened to it meanwhile: its pair is
/// still paired, and the Carbon didn't revoke that Silicon on it after the rollback. Each grant put
/// back is logged as `access_granted` with `restored_after_rollback`, and the table then goes. The
/// owner-active check and the membership sweep re-check the grants like any other.
pub async fn restore_rollback_grants(pool: &PgPool, world: &World) -> anyhow::Result<()> {
    let stash = world.t("rollback_1_1_grants");
    let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(&stash)
        .fetch_one(pool)
        .await
        .context("checking rollback stash before restore")?;
    if exists.is_none() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    // ensure_world_to releases this migration lock before restoration. Two service startups can
    // therefore both see the stash above; serialize the restore transaction and re-check after
    // waiting, before either reading the stash or dropping it. The fast path stays lock-free.
    sqlx::query("SELECT pg_advisory_xact_lock(7342002)")
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = 'rollback_1_1_grants')")
        .bind(&world.schema)
        .fetch_one(&mut *tx)
        .await
        .context("checking rollback stash under lock")?;
    if !exists {
        tx.commit().await?;
        return Ok(());
    }
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT g.device_id, g.silicon_id, g.team, g.granted_by FROM {stash} g JOIN {devices} d ON d.device_id = g.device_id
         WHERE d.removed_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM {activity} x WHERE x.device_id = g.device_id AND x.action = 'access_revoked'
                             AND x.details->>'silicon_id' = g.silicon_id AND x.at > g.stashed_at)",
        devices = world.t("devices"),
        activity = world.t("activity"),
    )))
    .fetch_all(&mut *tx)
    .await.context("reading rollback grants")?;
    let mut restored = 0;
    for (device_id, silicon_id, team, granted_by) in rows {
        let inserted = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {access} (device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted)
             SELECT device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted FROM {stash}
             WHERE device_id = $1 AND silicon_id = $2 AND team = $3 ON CONFLICT DO NOTHING",
            access = world.t("device_access"),
        )))
        .bind(&device_id)
        .bind(&silicon_id)
        .bind(&team)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            continue;
        }
        restored += 1;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} (id, device_id, actor_kind, actor_id, action, details, team) VALUES ($1, $2, 'carbon', 'extend', 'access_granted', $3, $4)",
            world.t("activity")
        )))
        .bind(Uuid::now_v7())
        .bind(&device_id)
        .bind(serde_json::json!({"silicon_id": silicon_id, "granted_by": granted_by, "restored_after_rollback": true}))
        .bind(&team)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TABLE {stash}")))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(world = %world.schema, restored, "put back the grants a rollback to 1.0.0 had set aside");
    Ok(())
}
