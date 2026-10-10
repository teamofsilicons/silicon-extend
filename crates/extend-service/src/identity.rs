//! 4.0: identity moves from Silicon IAM to Silicon Accounts.
//!
//! Before 4.0 every identity column held an IAM public id as text (`c:handle`, `si:handle`), and
//! most rows carried the Team they were made in. From 4.0 those columns hold Silicon Accounts
//! uuids (short case-sensitive strings that never contain `:`), and Teams are history.
//!
//! - Schema version 9 ([`WORLD_4_0_ACCOUNTS`]) is additive: the accounts cache, webhook dedupe,
//!   held proofs, Ting enrolments per account, `identity_links`, a shadow column `*_iam_id` next
//!   to every identity column, and nullable Team columns. It deletes nothing: grants and open wake
//!   requests that only existed once per Team are merged, and the extra copies are archived
//!   (`device_access_archive`) or withdrawn, never dropped.
//! - Until an operator applies a mapping, old rows still hold IAM ids: they match no access
//!   token's `sub`, so they are inert (nobody can use or see them through the API), and Extend
//!   shows the old id where it renders them.
//! - `extend-service identity apply --file mapping.csv [--dry-run]` (also spelled
//!   `link-identities`) re-keys those rows in one transaction ([`apply`]). The original value of
//!   every column is kept in its `*_iam_id` shadow column, so applying again with a corrected file
//!   re-derives every row from the originals: it can be re-run, and undone, until cutover.
//! - `extend-service identity suggest --out mapping.csv` writes a candidate file by asking Silicon
//!   Accounts which account has each old public id today, for a person to review ([`suggest`]).

/// Schema version 9 (4.0): Silicon Accounts, no Teams. `{s}` is the schema.
pub const WORLD_4_0_ACCOUNTS: &str = r#"
-- The accounts Extend has seen, keyed by Silicon Accounts uuid.
CREATE TABLE IF NOT EXISTS {s}.accounts (
    uuid text PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('carbon', 'silicon')),
    id text NOT NULL DEFAULT '',                  -- current public id ('' once deleted)
    display_name text,
    pfp_url text,
    status text NOT NULL DEFAULT 'active',        -- active | unclaimed | pending_custodian | deleted
    custodian_uuid text,                          -- a Silicon's custodian
    custodian_id text,
    version bigint NOT NULL DEFAULT 0,            -- account.updated ordering
    revoked_before timestamptz,                   -- tokens issued earlier are refused
    deleted_at timestamptz,
    looked_up_at timestamptz,
    seen_at timestamptz,
    id_set_at timestamptz,                        -- when an authoritative source (event, lookup) set id
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS accounts_custodian ON {s}.accounts (custodian_uuid) WHERE custodian_uuid IS NOT NULL;
CREATE INDEX IF NOT EXISTS accounts_public_id ON {s}.accounts (id);

-- Silicon Accounts webhook deliveries applied (dedupe on event_id).
CREATE TABLE IF NOT EXISTS {s}.accounts_events (
    event_id text PRIMARY KEY,
    event_type text NOT NULL,
    account_uuid text,
    occurred_at timestamptz,
    received_at timestamptz NOT NULL DEFAULT now()
);

-- Old IAM public ids and the Silicon Accounts uuid each maps to (identity apply).
CREATE TABLE IF NOT EXISTS {s}.identity_links (
    iam_public_id text PRIMARY KEY,
    iam_principal_id text,
    accounts_uuid text NOT NULL,
    kind text,
    linked_at timestamptz NOT NULL DEFAULT now(),
    source text NOT NULL
);
CREATE INDEX IF NOT EXISTS identity_links_uuid ON {s}.identity_links (accounts_uuid);
-- Every identity apply run, with what it changed (an audit trail for the cutover).
CREATE TABLE IF NOT EXISTS {s}.identity_link_runs (
    run_id uuid PRIMARY KEY,
    ran_at timestamptz NOT NULL DEFAULT now(),
    source text NOT NULL,
    mapping_sha256 text NOT NULL,
    report jsonb NOT NULL
);

-- Proofs Extend holds to act at other apps (refresh tokens sealed with
-- EXTEND_DELEGATION_ENCRYPTION_KEY).
CREATE TABLE IF NOT EXISTS {s}.proof_grants (
    account_uuid text NOT NULL,
    receiving_app text NOT NULL,
    scopes text NOT NULL,
    proof_id text NOT NULL,
    token_cipher bytea NOT NULL,
    token_expires_at timestamptz NOT NULL,
    refresh_cipher bytea,
    refresh_expires_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_uuid, receiving_app, scopes)
);

-- Ting, per account (it was per account and Team).
CREATE TABLE IF NOT EXISTS {s}.ting_enrolments (
    account_uuid text PRIMARY KEY,
    registered_at timestamptz,
    refused_at timestamptz,
    last_error text
);
CREATE TABLE IF NOT EXISTS {s}.ting_types (
    ting_type text PRIMARY KEY,
    missing_since timestamptz NOT NULL,
    last_checked_at timestamptz NOT NULL,
    last_error text
);

-- The value each identity column held before identity apply re-keyed it (NULL: never re-keyed).
ALTER TABLE {s}.devices ADD COLUMN IF NOT EXISTS owner_iam_id text;
ALTER TABLE {s}.device_access ADD COLUMN IF NOT EXISTS silicon_iam_id text;
ALTER TABLE {s}.device_access ADD COLUMN IF NOT EXISTS granted_by_iam_id text;
ALTER TABLE {s}.sessions ADD COLUMN IF NOT EXISTS silicon_iam_id text;
ALTER TABLE {s}.activity ADD COLUMN IF NOT EXISTS actor_iam_id text;
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS from_iam_id text;
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS to_iam_id text;
ALTER TABLE {s}.requests ADD COLUMN IF NOT EXISTS routed_to_iam_id text;
ALTER TABLE {s}.wake_requests ADD COLUMN IF NOT EXISTS from_iam_id text;
ALTER TABLE {s}.wake_requests ADD COLUMN IF NOT EXISTS to_iam_id text;
ALTER TABLE {s}.files ADD COLUMN IF NOT EXISTS created_by_iam_id text;
ALTER TABLE {s}.files ADD COLUMN IF NOT EXISTS shared_with_iam_id text;

-- Teams are history: the columns keep their values; new rows leave them NULL, except
-- devices.team, which installed device apps read as a string ('' for new pairs). (device_access.team
-- is part of the 1.1 primary key; it becomes nullable once the key changes, below.)
ALTER TABLE {s}.sessions ALTER COLUMN team DROP NOT NULL;
ALTER TABLE {s}.requests ALTER COLUMN team DROP NOT NULL;
ALTER TABLE {s}.files ALTER COLUMN team DROP NOT NULL;
ALTER TABLE {s}.wake_requests ALTER COLUMN team DROP NOT NULL;
ALTER TABLE {s}.devices ALTER COLUMN team SET DEFAULT '';
-- The triggers that filled in Teams and organization bindings.
DROP TRIGGER IF EXISTS device_access_team ON {s}.device_access;
DROP TRIGGER IF EXISTS device_initial_organization ON {s}.devices;
CREATE OR REPLACE FUNCTION {s}.requests_1_1() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    NEW.holder_device_id := COALESCE(NEW.holder_device_id, NEW.device_id);
    NEW.holder_session_id := COALESCE(NEW.holder_session_id, NEW.session_id);
    RETURN NEW; END $f$;

-- A pair's "wake requests off" lived on its organization bindings; it moves to the pair.
UPDATE {s}.devices d SET wake_muted = true
 WHERE NOT d.wake_muted AND EXISTS (SELECT 1 FROM {s}.device_organizations o
                                     WHERE o.device_id = d.device_id AND o.wake_muted AND o.removed_at IS NULL);

-- One grant per pair and Silicon (it was one per pair, Team and Silicon). The earliest grant of
-- each Silicon on each pair stays, with the latest use and "muted" if any copy was; every other
-- copy is archived, not dropped.
CREATE TABLE IF NOT EXISTS {s}.device_access_archive (
    device_id text NOT NULL,
    silicon_id text NOT NULL,
    granted_by text NOT NULL,
    granted_at timestamptz NOT NULL,
    last_used_at timestamptz,
    team text,
    wake_muted boolean NOT NULL DEFAULT false,
    silicon_iam_id text,
    granted_by_iam_id text,
    archived_at timestamptz NOT NULL DEFAULT now(),
    archive_reason text NOT NULL
);
INSERT INTO {s}.device_access_archive (device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted, archive_reason)
SELECT device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted,
       'merged into one grant per pair and Silicon (4.0 has no Teams)'
  FROM (SELECT a.*, row_number() OVER (PARTITION BY device_id, silicon_id ORDER BY granted_at, team) AS n
          FROM {s}.device_access a) r
 WHERE r.n > 1;
UPDATE {s}.device_access k SET last_used_at = g.last_used_at, wake_muted = g.wake_muted
  FROM (SELECT device_id, silicon_id, max(last_used_at) AS last_used_at, bool_or(wake_muted) AS wake_muted
          FROM {s}.device_access GROUP BY device_id, silicon_id HAVING count(*) > 1) g
 WHERE k.device_id = g.device_id AND k.silicon_id = g.silicon_id;
DELETE FROM {s}.device_access a
 USING (SELECT device_id, silicon_id, team,
               row_number() OVER (PARTITION BY device_id, silicon_id ORDER BY granted_at, team) AS n
          FROM {s}.device_access) r
 WHERE a.device_id = r.device_id AND a.silicon_id = r.silicon_id AND a.team = r.team AND r.n > 1;
ALTER TABLE {s}.device_access DROP CONSTRAINT IF EXISTS device_access_pkey;
ALTER TABLE {s}.device_access ADD PRIMARY KEY (device_id, silicon_id);
ALTER TABLE {s}.device_access ALTER COLUMN team DROP NOT NULL;
CREATE INDEX IF NOT EXISTS device_access_granter ON {s}.device_access (granted_by);

-- One open wake request per physical device and Silicon (it was per Team too): the latest ask
-- stays open; the others are withdrawn.
UPDATE {s}.wake_requests w SET state = 'withdrawn', ended_at = now(), end_reason = 'left_team',
       ting_delivery = CASE WHEN w.ting_delivery IN ('pending', 'deferred') THEN 'failed' ELSE w.ting_delivery END,
       ting_last_error = CASE WHEN w.ting_delivery IN ('pending', 'deferred') THEN 'The request ended before delivery.'
                              ELSE w.ting_last_error END,
       ting_next_at = NULL
  FROM (SELECT wake_id, row_number() OVER (PARTITION BY instance_id, from_id ORDER BY last_asked_at DESC, wake_id) AS n
          FROM {s}.wake_requests WHERE state = 'open') r
 WHERE w.wake_id = r.wake_id AND r.n > 1;
DROP INDEX IF EXISTS {s}.wake_requests_one_open;
CREATE UNIQUE INDEX IF NOT EXISTS wake_requests_one_open_per_silicon ON {s}.wake_requests (instance_id, from_id) WHERE state = 'open';
CREATE INDEX IF NOT EXISTS wake_requests_asker_2 ON {s}.wake_requests (from_id, instance_id, last_asked_at DESC);
"#;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use crate::db::World;

/// Every identity column, with the shadow column that keeps its value from before the re-key.
pub const IDENTITY_COLUMNS: &[(&str, &str, &str)] = &[
    ("devices", "owner_id", "owner_iam_id"),
    ("device_access", "silicon_id", "silicon_iam_id"),
    ("device_access", "granted_by", "granted_by_iam_id"),
    ("sessions", "silicon_id", "silicon_iam_id"),
    ("activity", "actor_id", "actor_iam_id"),
    ("requests", "from_id", "from_iam_id"),
    ("requests", "to_id", "to_iam_id"),
    ("requests", "routed_to_id", "routed_to_iam_id"),
    ("wake_requests", "from_id", "from_iam_id"),
    ("wake_requests", "to_id", "to_iam_id"),
    ("files", "created_by", "created_by_iam_id"),
    ("files", "shared_with", "shared_with_iam_id"),
];

/// One line of a mapping file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub iam_public_id: String,
    pub iam_principal_id: Option<String>,
    pub accounts_uuid: String,
    pub current_id: Option<String>,
}

fn kind_of(id: &str) -> &'static str {
    if id.starts_with("si:") { "silicon" } else { "carbon" }
}

/// Reads a mapping file: a CSV with a header naming `accounts_uuid` and `iam_public_id` (the
/// `c:`/`si:` id Extend stored; `iam_principal_id` is accepted for it too when its values are
/// public ids), and optionally `current_id`. Blank lines and lines starting with `#` are skipped.
pub fn parse_mapping(text: &str) -> anyhow::Result<Vec<Link>> {
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'));
    let Some((_, header)) = lines.next() else {
        bail!("the mapping file is empty: it needs a header line like `iam_public_id,accounts_uuid`");
    };
    let cols: Vec<String> = header.split(',').map(|c| c.trim().to_ascii_lowercase()).collect();
    let at = |name: &str| cols.iter().position(|c| c == name);
    let uuid_col = at("accounts_uuid").context("the mapping file's header has no accounts_uuid column")?;
    let public_col = at("iam_public_id");
    let principal_col = at("iam_principal_id");
    if public_col.is_none() && principal_col.is_none() {
        bail!("the mapping file's header needs an iam_public_id (or iam_principal_id) column next to accounts_uuid");
    }
    let current_col = at("current_id");
    let mut out: Vec<Link> = Vec::new();
    let mut seen = BTreeSet::new();
    for (n, line) in lines {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let get = |c: Option<usize>| {
            c.and_then(|i| fields.get(i))
                .map(|v| v.trim_matches('"').to_owned())
                .filter(|v| !v.is_empty())
        };
        let principal = get(principal_col);
        let public = get(public_col)
            .or_else(|| {
                principal
                    .clone()
                    .filter(|p| extend_protocol::ids::member_kind(&p.to_ascii_lowercase()).is_some())
            })
            .map(|p| p.to_ascii_lowercase())
            .with_context(|| {
                format!("line {n}: no c:/si: id in iam_public_id (Extend stored public ids, not IAM principal uuids)")
            })?;
        if extend_protocol::ids::member_kind(&public).is_none() {
            bail!("line {n}: {public:?} is not a c:/si: id");
        }
        let Some(uuid) = get(Some(uuid_col)) else {
            // A line without a uuid says "no account": it stays unmapped.
            continue;
        };
        if uuid.len() > 64 || !uuid.bytes().all(|b| b.is_ascii_alphanumeric()) {
            bail!("line {n}: {uuid:?} is not a Silicon Accounts uuid (letters and digits, like zQo)");
        }
        if !seen.insert(public.clone()) {
            bail!("line {n}: {public} appears twice in the mapping file");
        }
        out.push(Link {
            iam_principal_id: principal
                .filter(|p| extend_protocol::ids::member_kind(&p.to_ascii_lowercase()).is_none()),
            iam_public_id: public,
            accounts_uuid: uuid,
            current_id: get(current_col),
        });
    }
    Ok(out)
}

/// `identity apply`: re-keys every identity column from old public ids to Silicon Accounts uuids,
/// in one transaction (refused while another apply runs). Each column's original value is kept in
/// its shadow column first, and every run re-derives from those originals, so applying a corrected
/// file replaces the previous result entirely. Returns the report; `dry_run` rolls everything back.
pub async fn apply(
    pool: &sqlx::PgPool,
    links: &[Link],
    source: &str,
    dry_run: bool,
) -> anyhow::Result<serde_json::Value> {
    let world = World::production();
    let t = |name: &str| world.t(name);
    let digest = {
        let mut h = Sha256::new();
        for l in links {
            h.update(format!("{},{}\n", l.iam_public_id, l.accounts_uuid).as_bytes());
        }
        extend_protocol::ids::hex_lower(&h.finalize())
    };
    let mut tx = pool.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(7342030)")
        .fetch_one(&mut *tx)
        .await?;
    if !locked {
        bail!("another identity apply is running; wait for it to finish");
    }
    // The mapping replaces the previous one.
    sqlx::query(sql!("DELETE FROM {}", t("identity_links")))
        .execute(&mut *tx)
        .await?;
    for l in links {
        sqlx::query(sql!(
            "INSERT INTO {} (iam_public_id, iam_principal_id, accounts_uuid, kind, source) VALUES ($1, $2, $3, $4, $5)",
            t("identity_links")
        ))
        .bind(&l.iam_public_id)
        .bind(&l.iam_principal_id)
        .bind(&l.accounts_uuid)
        .bind(kind_of(&l.iam_public_id))
        .bind(source)
        .execute(&mut *tx)
        .await?;
    }
    // Keep each original before the first re-key (values with ':' are old public ids).
    for (table, col, shadow) in IDENTITY_COLUMNS {
        sqlx::query(sql!(
            "UPDATE {} SET {shadow} = {col} WHERE {shadow} IS NULL AND {col} LIKE '%:%'",
            t(table)
        ))
        .execute(&mut *tx)
        .await?;
    }
    let mapped = |table: &str, col: &str, shadow: &str| {
        format!(
            "COALESCE((SELECT l.accounts_uuid FROM {links} l WHERE l.iam_public_id = {table}.{shadow}), {table}.{shadow}, {table}.{col})",
            links = t("identity_links")
        )
    };
    // Re-keying must not merge two pairs of one device, or two grants on one pair.
    let pair_clash: Vec<(uuid::Uuid, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT instance_id, owner, count(*) FROM (SELECT devices.instance_id, {} AS owner FROM {} devices WHERE devices.removed_at IS NULL) x
         GROUP BY 1, 2 HAVING count(*) > 1",
        mapped("devices", "owner_id", "owner_iam_id"),
        t("devices")
    )))
    .fetch_all(&mut *tx)
    .await?;
    let grant_clash: Vec<(String, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT device_id, silicon, count(*) FROM (SELECT device_access.device_id, {} AS silicon FROM {} device_access) x
         GROUP BY 1, 2 HAVING count(*) > 1",
        mapped("device_access", "silicon_id", "silicon_iam_id"),
        t("device_access")
    )))
    .fetch_all(&mut *tx)
    .await?;
    if !pair_clash.is_empty() || !grant_clash.is_empty() {
        let report = json!({
            "refused": "two old ids map to one account where only one may exist",
            "pairs_of_one_device": pair_clash.iter().map(|(i, o, n)| json!({"instance_id": i, "accounts_uuid": o, "rows": n})).collect::<Vec<_>>(),
            "grants_on_one_pair": grant_clash.iter().map(|(d, s, n)| json!({"device_id": d, "accounts_uuid": s, "rows": n})).collect::<Vec<_>>(),
            "hint": "Unpair one of the pairs (or revoke one of the grants) on the old service, or fix the mapping, then apply again.",
        });
        tx.rollback().await?;
        bail!("identity apply refused:\n{}", serde_json::to_string_pretty(&report)?);
    }
    // Open wake requests are transient: of two that would collide, the older is withdrawn.
    let withdrawn = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {w} w SET state = 'withdrawn', ended_at = now(), end_reason = 'left_team', ting_next_at = NULL
           FROM (SELECT wake_id, row_number() OVER (PARTITION BY instance_id, {m} ORDER BY last_asked_at DESC, wake_id) AS n
                   FROM {w} wake_requests WHERE state = 'open') r
          WHERE w.wake_id = r.wake_id AND r.n > 1",
        w = t("wake_requests"),
        m = mapped("wake_requests", "from_id", "from_iam_id")
    )))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let mut columns = BTreeMap::new();
    for (table, col, shadow) in IDENTITY_COLUMNS {
        let rekeyed = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {tbl} {table} SET {col} = {m} WHERE {table}.{shadow} IS NOT NULL AND {table}.{col} IS DISTINCT FROM {m}",
            tbl = t(table),
            m = mapped(table, col, shadow)
        )))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let unmapped: Vec<String> = sqlx::query_scalar(sql!(
            "SELECT DISTINCT {col} FROM {} WHERE {col} LIKE '%:%' ORDER BY 1",
            t(table)
        ))
        .fetch_all(&mut *tx)
        .await?;
        columns.insert(
            format!("{table}.{col}"),
            json!({"rows_changed": rekeyed, "unmapped_ids": unmapped}),
        );
    }
    // Notifications from before the move were addressed through Silicon IAM; they can't go now.
    let requests_failed = sqlx::query(sql!(
        "UPDATE {} SET delivery = 'failed', ting_next_at = NULL,
                last_error = 'Not delivered: Extend moved to Silicon Accounts before it was delivered.'
         WHERE delivery = 'pending'",
        t("requests")
    ))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let wake_tings_failed = sqlx::query(sql!(
        "UPDATE {} SET ting_delivery = CASE WHEN ting_delivery IN ('pending', 'deferred') THEN 'failed' ELSE ting_delivery END,
                ting_next_at = NULL,
                answer_ting = CASE WHEN answer_ting = 'pending' THEN 'failed' ELSE answer_ting END,
                answer_ting_next_at = NULL,
                ting_last_error = CASE WHEN ting_delivery IN ('pending', 'deferred')
                                       THEN 'Not delivered: Extend moved to Silicon Accounts before it was delivered.' ELSE ting_last_error END
         WHERE ting_delivery IN ('pending', 'deferred') OR answer_ting = 'pending'",
        t("wake_requests")
    ))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Seed the accounts cache, so views show an id until each account signs in (or a lookup or a
    // webhook says better).
    for l in links {
        sqlx::query(sql!(
            "INSERT INTO {} (uuid, kind, id) VALUES ($1, $2, $3) ON CONFLICT (uuid) DO NOTHING",
            t("accounts")
        ))
        .bind(&l.accounts_uuid)
        .bind(kind_of(&l.iam_public_id))
        .bind(l.current_id.as_deref().unwrap_or(&l.iam_public_id))
        .execute(&mut *tx)
        .await?;
    }
    let report = json!({
        "source": source,
        "mapping_sha256": digest,
        "links": links.len(),
        "dry_run": dry_run,
        "columns": columns,
        "wake_requests_withdrawn": withdrawn,
        "pending_requests_failed": requests_failed,
        "pending_wake_tings_failed": wake_tings_failed,
    });
    if dry_run {
        tx.rollback().await?;
    } else {
        sqlx::query(sql!(
            "INSERT INTO {} (run_id, source, mapping_sha256, report) VALUES ($1, $2, $3, $4)",
            t("identity_link_runs")
        ))
        .bind(uuid::Uuid::now_v7())
        .bind(source)
        .bind(&digest)
        .bind(&report)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(report)
}

/// Every old public id still stored (or kept in a shadow column), for `identity suggest`.
pub async fn stored_old_ids(pool: &sqlx::PgPool) -> anyhow::Result<Vec<String>> {
    let world = World::production();
    let mut ids = BTreeSet::new();
    for (table, col, shadow) in IDENTITY_COLUMNS {
        let found: Vec<String> = sqlx::query_scalar(sql!(
            "SELECT DISTINCT v FROM (SELECT {col} AS v FROM {t} UNION SELECT {shadow} FROM {t}) x WHERE v LIKE '%:%'",
            t = world.t(table)
        ))
        .fetch_all(pool)
        .await?;
        ids.extend(found.into_iter().map(|v| v.to_ascii_lowercase()));
    }
    Ok(ids
        .into_iter()
        .filter(|i| extend_protocol::ids::member_kind(i).is_some())
        .collect())
}

/// `identity suggest`: a candidate mapping file, one line per old public id, with the account
/// that has that id in Silicon Accounts today (lookups through Extend's app credentials, at most
/// 500 a minute). A person reviews it before `identity apply`: an id can belong to someone else
/// now.
pub async fn suggest(pool: &sqlx::PgPool, api: &dyn crate::accounts::api::AccountsApi) -> anyhow::Result<String> {
    let mut out = String::from(
        "# Candidate mapping from Silicon Extend: review every line before `extend-service identity apply`.\n\
         # A line with an empty accounts_uuid stays unmapped (its rows stay inert).\n\
         iam_public_id,accounts_uuid,current_id,kind,status\n",
    );
    for (n, id) in stored_old_ids(pool).await?.into_iter().enumerate() {
        if n > 0 && n % 450 == 0 {
            tokio::time::sleep(std::time::Duration::from_secs(61)).await;
        }
        match api.lookup_by_id(&id).await {
            Ok(Some(a)) => {
                let kind = match a.kind {
                    silicon_accounts_client::AccountKind::Carbon => "carbon",
                    silicon_accounts_client::AccountKind::Silicon => "silicon",
                };
                out.push_str(&format!("{id},{},{},{kind},{}\n", a.uuid, a.id, a.status));
            }
            Ok(None) => out.push_str(&format!("{id},,,{},no account has this id\n", kind_of(&id))),
            Err(e) => bail!("looking up {id} in Silicon Accounts failed: {}", e.0.message),
        }
    }
    Ok(out)
}
