//! The accounts Extend has seen, cached in the `accounts` table (and in memory for a minute):
//! current public id, display name, photo, custodian and status, keyed by uuid. Filled from token
//! claims at every sign-in, from Silicon Accounts lookups (rate limited below the 600 a minute
//! Accounts allows each app), and kept current by webhooks. Also the custodian circle helpers.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use extend_protocol::ErrorCode;
use extend_protocol::model::{Member, MemberKind};
use silicon_accounts_client::AccountSummary;
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock};

use super::Principal;
use super::api::AccountsApi;
use crate::db::World;
use crate::error::{AppError, AppResult};

/// How long a cached row is reused from memory before the database is read again.
const MEMORY_TTL: Duration = Duration::from_secs(60);
/// A Silicon's custodian (and anyone's profile) is looked up again after this long.
pub const PROFILE_MAX_AGE: time::Duration = time::Duration::hours(12);
/// Lookups Extend allows itself per minute (Silicon Accounts allows each app 600).
pub const LOOKUPS_PER_MINUTE: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AccountRow {
    pub uuid: String,
    pub kind: String,
    /// The current public id; empty for a deleted account.
    pub id: String,
    pub display_name: Option<String>,
    pub pfp_url: Option<String>,
    /// `active`, `unclaimed`, `pending_custodian` or `deleted`.
    pub status: String,
    pub custodian_uuid: Option<String>,
    pub custodian_id: Option<String>,
    pub version: i64,
    pub revoked_before: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
    pub looked_up_at: Option<OffsetDateTime>,
    /// When an event or a lookup last said what `id` is. An access token issued before then may
    /// still carry the old id, so its claim doesn't overwrite it.
    pub id_set_at: Option<OffsetDateTime>,
}

impl AccountRow {
    pub fn kind(&self) -> MemberKind {
        if self.kind == "silicon" {
            MemberKind::Silicon
        } else {
            MemberKind::Carbon
        }
    }
    pub fn is_silicon(&self) -> bool {
        self.kind == "silicon"
    }
    /// What to show for it: the current id, or that it was deleted.
    pub fn shown_id(&self) -> String {
        if self.status == "deleted" || self.id.is_empty() {
            format!("deleted account {}", self.uuid)
        } else {
            self.id.clone()
        }
    }
}

const COLUMNS: &str = "uuid, kind, id, display_name, pfp_url, status, custodian_uuid, custodian_id, version, revoked_before, deleted_at, looked_up_at, id_set_at";

fn kind_str(kind: MemberKind) -> &'static str {
    match kind {
        MemberKind::Carbon => "carbon",
        MemberKind::Silicon => "silicon",
    }
}

pub struct Directory {
    pool: sqlx::PgPool,
    api: Arc<dyn AccountsApi>,
    table: String,
    memory: RwLock<HashMap<String, (Instant, AccountRow)>>,
    lookups: Mutex<VecDeque<Instant>>,
}

impl Directory {
    pub fn new(pool: sqlx::PgPool, api: Arc<dyn AccountsApi>) -> Self {
        Self {
            pool,
            api,
            table: World::production().t("accounts"),
            memory: RwLock::default(),
            lookups: Mutex::default(),
        }
    }

    async fn remember(&self, row: &AccountRow) {
        let mut memory = self.memory.write().await;
        if memory.len() > 100_000 {
            memory.retain(|_, (at, _)| at.elapsed() < MEMORY_TTL);
        }
        memory.insert(row.uuid.clone(), (Instant::now(), row.clone()));
    }

    pub async fn forget(&self, uuid: &str) {
        self.memory.write().await.remove(uuid);
    }

    /// One lookup's turn under [`LOOKUPS_PER_MINUTE`]; `false` when the minute's budget is spent.
    async fn lookup_turn(&self) -> bool {
        let mut q = self.lookups.lock().await;
        while q.front().is_some_and(|t| t.elapsed() >= Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() >= LOOKUPS_PER_MINUTE {
            return false;
        }
        q.push_back(Instant::now());
        true
    }

    /// The cached row (memory, then the database). Never asks Silicon Accounts.
    pub async fn get(&self, uuid: &str) -> AppResult<Option<AccountRow>> {
        if let Some((at, row)) = self.memory.read().await.get(uuid)
            && at.elapsed() < MEMORY_TTL
        {
            return Ok(Some(row.clone()));
        }
        let row: Option<AccountRow> = sqlx::query_as(sql!("SELECT {COLUMNS} FROM {} WHERE uuid = $1", self.table))
            .bind(uuid)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(r) = &row {
            self.remember(r).await;
        }
        Ok(row)
    }

    /// The row, looked up in Silicon Accounts when Extend has none or it is older than
    /// [`PROFILE_MAX_AGE`] (if the lookup budget allows; otherwise the cached row).
    pub async fn fetch(&self, uuid: &str) -> AppResult<Option<AccountRow>> {
        let cached = self.get(uuid).await?;
        let stale = cached.as_ref().is_none_or(|r| {
            r.looked_up_at
                .is_none_or(|t| OffsetDateTime::now_utc() - t > PROFILE_MAX_AGE)
        });
        if !stale || legacy_id(uuid) || !self.lookup_turn().await {
            return Ok(cached);
        }
        match self.api.lookup(uuid).await {
            Ok(Some(s)) => Ok(Some(self.upsert_summary(&s).await?)),
            Ok(None) => Ok(cached),
            Err(e) => {
                tracing::warn!(uuid, error = %e, "looking up an account in Silicon Accounts failed; using what Extend has");
                Ok(cached)
            }
        }
    }

    /// Records what a sign-in says (kind and current id) and makes sure a Silicon's custodian is
    /// known (looked up once, then kept current by webhooks and every [`PROFILE_MAX_AGE`]).
    pub async fn on_sign_in(&self, p: &Principal) -> AppResult<AccountRow> {
        let retired: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM extend.accounts_uuid128_map WHERE old_uuid=$1)")
                .bind(&p.uuid)
                .fetch_one(&self.pool)
                .await?;
        if retired {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "This account identity was migrated. Sign in to Extend again.",
            ));
        }
        let cached = self.get(&p.uuid).await?;
        // A token's `id` claim is what the id was when it was issued: newer than what Extend has
        // only if the token was issued after an event or lookup last set the id.
        let newer_claim = |r: &AccountRow| {
            r.id_set_at
                .is_none_or(|set| p.issued_at.is_some_and(|iat| iat > set.unix_timestamp()))
        };
        let changed = cached.as_ref().is_none_or(|r| {
            r.kind != kind_str(p.kind) || (!p.id.is_empty() && r.id != p.id && r.status != "deleted" && newer_claim(r))
        });
        let row = if changed {
            let row: AccountRow = sqlx::query_as(sql!(
                "INSERT INTO {t} (uuid, kind, id, seen_at) VALUES ($1, $2, $3, now())
                 ON CONFLICT (uuid) DO UPDATE SET kind = EXCLUDED.kind,
                     id = CASE WHEN {t}.status = 'deleted' OR EXCLUDED.id = ''
                                    OR ({t}.id_set_at IS NOT NULL AND ($4::bigint IS NULL OR to_timestamp($4) <= {t}.id_set_at))
                               THEN {t}.id ELSE EXCLUDED.id END,
                     seen_at = now(), updated_at = now()
                 RETURNING {COLUMNS}",
                t = self.table
            ))
            .bind(&p.uuid)
            .bind(kind_str(p.kind))
            .bind(&p.id)
            .bind(p.issued_at)
            .fetch_one(&self.pool)
            .await?;
            self.remember(&row).await;
            row
        } else {
            cached.unwrap_or_else(|| unreachable!("changed is true when nothing is cached"))
        };
        // A Silicon's custodian decides who may see what, and a profile names people: both come
        // from a lookup, made at the first sign-in and again every PROFILE_MAX_AGE (webhooks keep
        // them current in between). A failed or rate-limited lookup keeps what Extend has.
        let needs_profile = row
            .looked_up_at
            .is_none_or(|t| OffsetDateTime::now_utc() - t > PROFILE_MAX_AGE);
        if needs_profile && let Ok(Some(fresh)) = self.fetch(&p.uuid).await {
            return Ok(fresh);
        }
        Ok(row)
    }

    /// Stores a lookup answer.
    pub async fn upsert_summary(&self, s: &AccountSummary) -> AppResult<AccountRow> {
        let kind = match s.kind {
            silicon_accounts_client::AccountKind::Carbon => "carbon",
            silicon_accounts_client::AccountKind::Silicon => "silicon",
        };
        let (cu, ci) = match &s.custodian {
            Some(c) => (Some(c.uuid.clone()), Some(c.id.clone()).filter(|i| !i.is_empty())),
            None => (None, None),
        };
        let row: AccountRow = sqlx::query_as(sql!(
            "INSERT INTO {t} (uuid, kind, id, display_name, pfp_url, status, custodian_uuid, custodian_id, looked_up_at, id_set_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), now())
             ON CONFLICT (uuid) DO UPDATE SET kind = EXCLUDED.kind,
                 id = CASE WHEN EXCLUDED.id = '' THEN {t}.id ELSE EXCLUDED.id END,
                 id_set_at = CASE WHEN EXCLUDED.id = '' THEN {t}.id_set_at ELSE now() END,
                 display_name = EXCLUDED.display_name, pfp_url = EXCLUDED.pfp_url,
                 status = CASE WHEN {t}.status = 'deleted' THEN {t}.status ELSE EXCLUDED.status END,
                 custodian_uuid = COALESCE(EXCLUDED.custodian_uuid, {t}.custodian_uuid),
                 custodian_id = COALESCE(EXCLUDED.custodian_id, {t}.custodian_id),
                 looked_up_at = now(), updated_at = now()
             RETURNING {COLUMNS}",
            t = self.table
        ))
        .bind(&s.uuid)
        .bind(kind)
        .bind(&s.id)
        .bind(Some(s.display_name.clone()).filter(|n| !n.is_empty()))
        .bind(Some(s.pfp_url.clone()).filter(|u| !u.is_empty()))
        .bind(if s.status.is_empty() { "active" } else { s.status.as_str() })
        .bind(cu)
        .bind(ci)
        .fetch_one(&self.pool)
        .await?;
        self.remember(&row).await;
        Ok(row)
    }

    /// Resolves an account a caller named: a `c:`/`si:` id (through Silicon Accounts, current
    /// ids only) or a uuid Extend knows. Refuses unknown ids with an exact error.
    pub async fn resolve(&self, given: &str, want: Option<MemberKind>) -> AppResult<AccountRow> {
        let given = given.trim();
        let word = match want {
            Some(MemberKind::Silicon) => "Silicon",
            Some(MemberKind::Carbon) => "Carbon",
            None => "account",
        };
        let row = if let Some(kind) = extend_protocol::ids::member_kind(&given.to_ascii_lowercase()) {
            if want.is_some_and(|w| w != kind) {
                return Err(AppError::invalid(format!(
                    "{given} is not a {word} id; {word} ids look like {}.",
                    example(want)
                )));
            }
            if !self.lookup_turn().await {
                return Err(AppError::new(
                    ErrorCode::RateLimited,
                    "Extend is looking up too many accounts in Silicon Accounts right now.",
                )
                .hint("Retry in a minute."));
            }
            match self.api.lookup_by_id(given).await? {
                Some(s) => self.upsert_summary(&s).await?,
                None => {
                    return Err(AppError::invalid(format!(
                        "No {word} {given} exists in Silicon Accounts (ids are current ids; an account that changed its id is found by its new one)."
                    ))
                    .hint("Check the id on accounts.teamofsilicons.com, or ask its owner for it."));
                }
            }
        } else if super::is_account_uuid(given) {
            self.fetch(given).await?.ok_or_else(|| {
                AppError::invalid(format!("No {word} with uuid {given} is known to Silicon Accounts."))
                    .hint(format!("Name it by its id instead, like {}.", example(want)))
            })?
        } else {
            return Err(AppError::invalid(format!(
                "{given:?} is not a {word} id or uuid; {word} ids look like {}.",
                example(want)
            )));
        };
        if want.is_some_and(|w| w != row.kind()) {
            return Err(AppError::invalid(format!("{} is not a {word}.", row.shown_id())));
        }
        if row.status == "deleted" {
            return Err(AppError::invalid(format!(
                "The {word} {given} was deleted in Silicon Accounts."
            )));
        }
        Ok(row)
    }

    /// The current public id to show for a stored identity: a uuid Extend knows, else the value
    /// itself (rows from before the move to Silicon Accounts hold an old public id until the
    /// operator re-keys them).
    pub async fn public_id(&self, stored: &str) -> String {
        if stored.is_empty() || legacy_id(stored) || stored == crate::domain::SYSTEM_ACTOR {
            return stored.to_owned();
        }
        match self.get(stored).await {
            Ok(Some(row)) => row.shown_id(),
            _ => match self.fetch(stored).await {
                Ok(Some(row)) => row.shown_id(),
                _ => stored.to_owned(),
            },
        }
    }

    /// A stored identity as the API shows it: current id, uuid, kind and name.
    pub async fn member(&self, kind: MemberKind, stored: &str) -> Member {
        if stored == crate::domain::SYSTEM_ACTOR {
            return crate::domain::system_member();
        }
        if legacy_id(stored) {
            return Member::new(extend_protocol::ids::member_kind(stored).unwrap_or(kind), stored);
        }
        match self.get(stored).await.ok().flatten() {
            Some(row) => {
                let mut m = Member::new(row.kind(), row.shown_id()).with_uuid(stored);
                m.display_name = row.display_name.clone();
                m
            }
            None => Member::new(kind, self.public_id(stored).await).with_uuid(stored),
        }
    }

    /// A Silicon's custodian (uuid), as Extend knows it.
    pub async fn custodian_of(&self, silicon: &str) -> Option<String> {
        self.get(silicon).await.ok().flatten().and_then(|r| r.custodian_uuid)
    }

    /// Whether `carbon` is the custodian of `silicon`.
    pub async fn is_custodian(&self, carbon: &str, silicon: &str) -> bool {
        self.custodian_of(silicon).await.as_deref() == Some(carbon)
    }

    /// Whether two accounts are in one custodian circle: the same account; a Silicon and its
    /// custodian; or two Silicons with the same custodian.
    pub async fn same_circle(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        let (ca, cb) = (self.custodian_of(a).await, self.custodian_of(b).await);
        ca.as_deref() == Some(b) || cb.as_deref() == Some(a) || (ca.is_some() && ca == cb)
    }

    /// The Silicons Extend knows `carbon` is the custodian of.
    pub async fn silicons_of(&self, carbon: &str) -> AppResult<Vec<AccountRow>> {
        Ok(sqlx::query_as(sql!(
            "SELECT {COLUMNS} FROM {} WHERE custodian_uuid = $1 AND kind = 'silicon' AND status <> 'deleted' ORDER BY id",
            self.table
        ))
        .bind(carbon)
        .fetch_all(&self.pool)
        .await?)
    }

    // ───────────── Webhook writes ─────────────

    /// `account.id_changed` (the change happened at `at`).
    pub async fn set_id(
        &self,
        uuid: &str,
        kind: Option<MemberKind>,
        new_id: &str,
        at: OffsetDateTime,
    ) -> AppResult<()> {
        sqlx::query(sql!(
            "INSERT INTO {t} (uuid, kind, id, id_set_at) VALUES ($1, $2, $3, $4)
             ON CONFLICT (uuid) DO UPDATE SET id = EXCLUDED.id,
                 id_set_at = GREATEST({t}.id_set_at, EXCLUDED.id_set_at), updated_at = now()",
            t = self.table
        ))
        .bind(uuid)
        .bind(kind_str(kind.unwrap_or(if new_id.starts_with("si:") {
            MemberKind::Silicon
        } else {
            MemberKind::Carbon
        })))
        .bind(new_id)
        .bind(at)
        .execute(&self.pool)
        .await?;
        // A Silicon's custodian id is cached on its Silicons' rows too.
        sqlx::query(sql!(
            "UPDATE {} SET custodian_id = $2 WHERE custodian_uuid = $1",
            self.table
        ))
        .bind(uuid)
        .bind(new_id)
        .execute(&self.pool)
        .await?;
        self.memory.write().await.clear();
        Ok(())
    }

    /// `account.updated` (only when `version` is newer than what Extend has).
    pub async fn apply_update(&self, a: &silicon_accounts_client::AccountForApp) -> AppResult<bool> {
        let kind = match a.kind {
            silicon_accounts_client::AccountKind::Carbon => "carbon",
            silicon_accounts_client::AccountKind::Silicon => "silicon",
        };
        let (cu, ci) = match &a.custodian {
            Some(c) => (Some(c.uuid.clone()), Some(c.id.clone()).filter(|i| !i.is_empty())),
            None => (None, None),
        };
        let applied = sqlx::query(sql!(
            "INSERT INTO {t} (uuid, kind, id, display_name, pfp_url, custodian_uuid, custodian_id, version, id_set_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
             ON CONFLICT (uuid) DO UPDATE SET kind = EXCLUDED.kind,
                 id = CASE WHEN EXCLUDED.id = '' THEN {t}.id ELSE EXCLUDED.id END,
                 id_set_at = CASE WHEN EXCLUDED.id = '' THEN {t}.id_set_at ELSE now() END,
                 display_name = EXCLUDED.display_name, pfp_url = EXCLUDED.pfp_url,
                 custodian_uuid = COALESCE(EXCLUDED.custodian_uuid, {t}.custodian_uuid),
                 custodian_id = COALESCE(EXCLUDED.custodian_id, {t}.custodian_id),
                 version = EXCLUDED.version, updated_at = now()
             WHERE {t}.version < EXCLUDED.version OR EXCLUDED.version = 0",
            t = self.table
        ))
        .bind(&a.uuid)
        .bind(kind)
        .bind(&a.id)
        .bind(Some(a.display_name.clone()).filter(|n| !n.is_empty()))
        .bind(Some(a.pfp_url.clone()).filter(|u| !u.is_empty()))
        .bind(cu)
        .bind(ci)
        .bind(a.version)
        .execute(&self.pool)
        .await?
        .rows_affected();
        self.forget(&a.uuid).await;
        Ok(applied > 0)
    }

    /// `silicon.custodian_changed`: returns the previous custodian.
    pub async fn set_custodian(&self, silicon: &str, to: Option<(&str, &str)>) -> AppResult<Option<String>> {
        let before = self.get(silicon).await?.and_then(|r| r.custodian_uuid);
        sqlx::query(sql!(
            "INSERT INTO {t} (uuid, kind, custodian_uuid, custodian_id) VALUES ($1, 'silicon', $2, $3)
             ON CONFLICT (uuid) DO UPDATE SET custodian_uuid = EXCLUDED.custodian_uuid,
                 custodian_id = EXCLUDED.custodian_id, updated_at = now()",
            t = self.table
        ))
        .bind(silicon)
        .bind(to.map(|t| t.0))
        .bind(to.map(|t| t.1).filter(|i| !i.is_empty()))
        .execute(&self.pool)
        .await?;
        self.forget(silicon).await;
        Ok(before)
    }

    /// Tokens of `uuid` issued before `at` are refused from now on.
    pub async fn revoke_before(&self, uuid: &str, at: OffsetDateTime) -> AppResult<()> {
        sqlx::query(sql!(
            "INSERT INTO {t} (uuid, kind, revoked_before) VALUES ($1, 'carbon', $2)
             ON CONFLICT (uuid) DO UPDATE SET revoked_before = GREATEST({t}.revoked_before, EXCLUDED.revoked_before),
                 updated_at = now()",
            t = self.table
        ))
        .bind(uuid)
        .bind(at)
        .execute(&self.pool)
        .await?;
        self.forget(uuid).await;
        Ok(())
    }

    /// `account.deleted`: the account's own details go (id, name, photo); its uuid stays so
    /// others' history still reads "deleted account".
    pub async fn mark_deleted(&self, uuid: &str, at: OffsetDateTime) -> AppResult<()> {
        sqlx::query(sql!(
            "INSERT INTO {t} (uuid, kind, status, deleted_at, revoked_before) VALUES ($1, 'carbon', 'deleted', $2, $2)
             ON CONFLICT (uuid) DO UPDATE SET status = 'deleted', id = '', display_name = NULL, pfp_url = NULL,
                 deleted_at = EXCLUDED.deleted_at, revoked_before = EXCLUDED.revoked_before, updated_at = now()",
            t = self.table
        ))
        .bind(uuid)
        .bind(at)
        .execute(&self.pool)
        .await?;
        self.forget(uuid).await;
        Ok(())
    }
}

/// Whether a stored identity is an old public id (`c:…`, `si:…`), from before Extend moved to
/// Silicon Accounts. Accounts uuids never contain `:`.
pub fn legacy_id(stored: &str) -> bool {
    stored.contains(':')
}

fn example(want: Option<MemberKind>) -> &'static str {
    match want {
        Some(MemberKind::Silicon) => "si:scout",
        Some(MemberKind::Carbon) => "c:ada",
        None => "c:ada or si:scout",
    }
}
