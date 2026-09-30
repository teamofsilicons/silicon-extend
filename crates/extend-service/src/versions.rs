//! API majors served side by side, their lifecycle, and the compatibility matrix
//! (UNDERSTANDING.md "Versioning" 3, 5 and 6; TECHNICAL.md section 10).
//!
//! Every API major lives under its own path prefix: `/api/v1/…`, `/api/v2/…`. [`layer`] runs in
//! front of every route. For a versioned path it reads the major from the path, then:
//!
//! - refuses a major this build doesn't serve (`400 api_version_unsupported`);
//! - refuses a request whose `Silicon-Extend-API-Version` pin names another major
//!   (`400 api_version_mismatch`), so each path checks its own pin;
//! - refuses a sunset major (`410 api_version_sunset`, with a hint to update);
//! - counts the request per major and UTC day in `extend_global.api_version_usage`;
//! - marks every response of a deprecated major with `Deprecation` (RFC 9745) and `Sunset`
//!   (RFC 8594) headers.
//!
//! # Lifecycle
//!
//! Each served major is `current`, `deprecated` or `sunset`, kept in `extend_global.api_versions`
//! so every service instance agrees and the state survives restarts:
//!
//! - **current → deprecated** by configuration: `EXTEND_DEPRECATED_API_VERSIONS=1` (a comma list),
//!   applied when an instance starts. `deprecated_at` is when an instance first applied it.
//!   Removing a major from the list makes it current again (also at start), unless it has already
//!   been sunset.
//! - **deprecated → sunset** automatically, by [`Registry::upkeep`] (at start and every
//!   [`UPKEEP_EVERY`]), after [`SUNSET_QUIET_DAYS`] consecutive days with zero requests. The quiet
//!   days are counted from the later of `deprecated_at` and the end of the last UTC day that had a
//!   request, so a deprecated major always gets a full week's notice. `sunset_at` is the instant
//!   the rule was met, whenever upkeep noticed it. Sunset is final.
//!
//! Configuration refuses to deprecate the newest major this build serves: clients would have
//! nothing to move to, and a quiet week would switch the whole API off.
//!
//! # Adding API v2
//!
//! 1. Write `routes/v2.rs` with `pub fn routes() -> Router<Shared>` listing the `/api/v2/…` paths.
//!    Reuse the v1 handlers wherever the shape is unchanged; write new handlers only for the
//!    endpoints whose shape breaks.
//! 2. In `routes::router`, `.merge(v2::routes())` into the `api` router before its layers, so
//!    [`layer`] covers the new paths.
//! 3. Add `2` to [`SERVED`]. Negotiation (`GET /api/version`) then offers 2 to clients that list
//!    it and keeps answering 1 to clients that only speak 1. Give v2 its client and CLI ranges
//!    with `EXTEND_API_V2_CLIENT_CRATE` / `EXTEND_API_V2_CLI` if they aren't `>=2.0.0, <3.0.0`.
//! 4. In `silicon-extend-client`, add 2 to `SUPPORTED_API_VERSIONS` and branch on
//!    `Client::api_version()` where the paths differ; record `contracts/v2/client/*.json` with
//!    its `contract_fixtures` test. The service's `contracts` test then replays the fixtures of
//!    both majors.
//! 5. When v2 is out, set `EXTEND_DEPRECATED_API_VERSIONS=1`. v1 keeps working, with
//!    `Deprecation` and `Sunset` headers, until it has gone 7 days without a request. Remove the
//!    v1 routes from the build only after `GET /api/v2/contracts` shows it `sunset`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use anyhow::{Context as _, bail};
use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use extend_protocol::{API_VERSION_HEADER, ErrorCode};
use serde::Serialize;
use sqlx::PgPool;
use time::{Date, OffsetDateTime};

use crate::error::AppError;

/// Every API major whose routes this build mounts, oldest first.
pub const SERVED: &[u32] = &[extend_protocol::API_VERSION];
/// Consecutive days without a request after which a deprecated major is sunset.
pub const SUNSET_QUIET_DAYS: i64 = 7;
/// The rule, as the compatibility matrix states it.
pub const SUNSET_RULE: &str = "Sunset after 7 consecutive days with zero requests";
/// How often each instance re-reads the shared state and applies the sunset rule.
pub const UPKEEP_EVERY: Duration = Duration::from_secs(300);
/// Response header naming when a major was deprecated (RFC 9745: `@<unix seconds>`).
pub const DEPRECATION_HEADER: &str = "deprecation";
/// Response header naming the soonest a deprecated major can be sunset (RFC 8594: an HTTP-date).
pub const SUNSET_HEADER: &str = "sunset";

const MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS extend_global.api_versions (
    api_version integer PRIMARY KEY,
    state text NOT NULL CHECK (state IN ('current', 'deprecated', 'sunset')),
    deprecated_at timestamptz,
    sunset_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);
"#;

/// Creates the lifecycle table. Idempotent; serialised across instances.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(7342010)")
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(MIGRATION).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// The client crate and CLI versions that work with one API major.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Compat {
    pub client_crate: String,
    pub cli: String,
}

impl Compat {
    fn default_for(major: u32) -> Self {
        // Client/CLI 2 removed model selection, while ordinary requests still use API v1.
        let range = if major == 1 {
            ">=1.0.0, <3.0.0".to_owned()
        } else {
            format!(">={major}.0.0, <{}.0.0", major + 1)
        };
        Self {
            client_crate: range.clone(),
            cli: range,
        }
    }
}

/// What configuration says about the majors. Built once at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Majors this build mounts, oldest first.
    pub served: Vec<u32>,
    /// Majors configuration deprecates.
    pub deprecated: BTreeSet<u32>,
    /// Client crate and CLI ranges per served major.
    pub compat: BTreeMap<u32, Compat>,
}

impl Policy {
    /// Reads `EXTEND_DEPRECATED_API_VERSIONS`, `EXTEND_API_V{n}_CLIENT_CRATE` and `EXTEND_API_V{n}_CLI`.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(SERVED, |name| std::env::var(name).ok())
    }

    /// Builds a policy for `served` majors from variables looked up with `var`.
    pub fn parse(served: &[u32], var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let var = |name: &str| var(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let mut served: Vec<u32> = served.to_vec();
        served.sort_unstable();
        served.dedup();
        let newest = *served.last().context("a build must serve at least one API major")?;
        let list = served.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let mut deprecated = BTreeSet::new();
        if let Some(raw) = var("EXTEND_DEPRECATED_API_VERSIONS") {
            for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let major: u32 = part.parse().ok().filter(|m| *m >= 1).with_context(|| {
                    format!(
                        "EXTEND_DEPRECATED_API_VERSIONS: {part:?} is not an API major. \
                         List majors as whole numbers, like `1` or `1, 2`."
                    )
                })?;
                if !served.contains(&major) {
                    bail!(
                        "EXTEND_DEPRECATED_API_VERSIONS names API version {major}, which this build does not serve \
                         (it serves {list}). Remove it from the list, or deploy a build that serves it."
                    );
                }
                if major == newest {
                    bail!(
                        "EXTEND_DEPRECATED_API_VERSIONS deprecates API version {major}, the newest this build serves. \
                         Clients would have no version to move to, and a week without requests would switch the \
                         API off. Deprecate a major only once a build serving a newer one is deployed."
                    );
                }
                deprecated.insert(major);
            }
        }
        let mut compat = BTreeMap::new();
        for &major in &served {
            let mut c = Compat::default_for(major);
            for (suffix, slot) in [("CLIENT_CRATE", &mut c.client_crate), ("CLI", &mut c.cli)] {
                let name = format!("EXTEND_API_V{major}_{suffix}");
                if let Some(range) = var(&name) {
                    if !valid_range(&range) {
                        bail!(
                            "{name} must be a version range like \">={major}.0.0, <{}.0.0\" \
                             (comparators >=, >, <=, <, = joined by commas), got {range:?}.",
                            major + 1
                        );
                    }
                    *slot = range;
                }
            }
            compat.insert(major, c);
        }
        Ok(Self {
            served,
            deprecated,
            compat,
        })
    }
}

/// `>=1.0.0, <2.0.0` style: comparators joined by commas, each over a full `x.y.z` version.
fn valid_range(s: &str) -> bool {
    s.split(',').all(|part| {
        let part = part.trim();
        let version = [">=", "<=", ">", "<", "="]
            .iter()
            .find_map(|op| part.strip_prefix(op))
            .unwrap_or(part)
            .trim();
        let nums: Vec<&str> = version.split('.').collect();
        nums.len() == 3
            && nums
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Current,
    Deprecated,
    Sunset,
}

impl Lifecycle {
    fn parse(s: &str) -> Self {
        match s {
            "deprecated" => Self::Deprecated,
            "sunset" => Self::Sunset,
            _ => Self::Current,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Deprecated => "deprecated",
            Self::Sunset => "sunset",
        }
    }
}

/// One major's shared state, as of the last upkeep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Major {
    pub api_version: u32,
    pub state: Lifecycle,
    pub deprecated_at: Option<OffsetDateTime>,
    pub sunset_at: Option<OffsetDateTime>,
    /// The last UTC day with at least one request.
    pub last_request_on: Option<Date>,
    /// Whether this build mounts it (a sunset major may outlive its routes).
    pub served: bool,
}

impl Major {
    /// The soonest a deprecated major can be sunset: 7 days after the later of its deprecation and
    /// the end of its last day with a request. `request_today` counts a request being served now.
    pub fn sunset_earliest_at(&self, now: OffsetDateTime, request_today: bool) -> Option<OffsetDateTime> {
        if self.state != Lifecycle::Deprecated {
            return None;
        }
        let deprecated_at = self.deprecated_at.unwrap_or(now);
        let last = match (self.last_request_on, request_today) {
            (Some(d), true) => Some(d.max(now.date())),
            (None, true) => Some(now.date()),
            (d, false) => d,
        };
        let quiet_since = match last.and_then(Date::next_day) {
            Some(day) => deprecated_at.max(day.midnight().assume_utc()),
            None => deprecated_at,
        };
        Some(quiet_since + time::Duration::days(SUNSET_QUIET_DAYS))
    }
}

pub type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// The majors this instance serves and their lifecycle, refreshed from the database by upkeep.
pub struct Registry {
    policy: Policy,
    pool: PgPool,
    clock: Clock,
    majors: RwLock<Arc<Vec<Major>>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("policy", &self.policy)
            .field("majors", &self.majors())
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// Creates the table if needed, applies the policy and loads the state.
    pub async fn start(pool: PgPool, policy: Policy) -> anyhow::Result<Arc<Self>> {
        Self::start_with_clock(pool, policy, Arc::new(OffsetDateTime::now_utc)).await
    }

    /// [`Registry::start`] with an injected clock (tests move time forward with it).
    pub async fn start_with_clock(pool: PgPool, policy: Policy, clock: Clock) -> anyhow::Result<Arc<Self>> {
        migrate(&pool).await.context("creating extend_global.api_versions")?;
        let registry = Arc::new(Self {
            policy,
            pool,
            clock,
            majors: RwLock::new(Arc::new(Vec::new())),
        });
        registry.apply_policy().await?;
        registry.upkeep().await?;
        Ok(registry)
    }

    /// Writes what configuration says into the shared state: deprecates the majors it lists and
    /// makes current again a deprecated major it no longer lists (a sunset major stays sunset).
    /// Runs once, when an instance starts, so instances with different configuration during a
    /// rolling deploy don't undo each other.
    async fn apply_policy(&self) -> anyhow::Result<()> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(7342011)")
            .execute(&mut *tx)
            .await?;
        for &major in &self.policy.served {
            let v = i32::try_from(major)?;
            sqlx::query(
                "INSERT INTO extend_global.api_versions (api_version, state) VALUES ($1, 'current')
                 ON CONFLICT (api_version) DO NOTHING",
            )
            .bind(v)
            .execute(&mut *tx)
            .await?;
            if self.policy.deprecated.contains(&major) {
                let changed = sqlx::query(
                    "UPDATE extend_global.api_versions SET state = 'deprecated', deprecated_at = $2, updated_at = now()
                     WHERE api_version = $1 AND state = 'current'",
                )
                .bind(v)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                if changed.rows_affected() > 0 {
                    tracing::warn!(api_version = major, "API version deprecated by configuration");
                }
            } else {
                let changed = sqlx::query(
                    "UPDATE extend_global.api_versions SET state = 'current', deprecated_at = NULL, updated_at = now()
                     WHERE api_version = $1 AND state = 'deprecated'",
                )
                .bind(v)
                .execute(&mut *tx)
                .await?;
                if changed.rows_affected() > 0 {
                    tracing::info!(
                        api_version = major,
                        "API version is current again: EXTEND_DEPRECATED_API_VERSIONS no longer lists it"
                    );
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Every major with a lifecycle row or a route, oldest first.
    pub fn majors(&self) -> Arc<Vec<Major>> {
        match self.majors.read() {
            Ok(m) => m.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// A major this build serves.
    pub fn major(&self, api_version: u32) -> Option<Major> {
        self.majors()
            .iter()
            .find(|m| m.api_version == api_version && m.served)
            .cloned()
    }

    /// Majors a client can still agree on: served and not sunset.
    pub fn negotiable(&self) -> Vec<u32> {
        self.majors()
            .iter()
            .filter(|m| m.served && m.state != Lifecycle::Sunset)
            .map(|m| m.api_version)
            .collect()
    }

    pub fn deprecated(&self) -> Vec<u32> {
        self.majors()
            .iter()
            .filter(|m| m.served && m.state == Lifecycle::Deprecated)
            .map(|m| m.api_version)
            .collect()
    }

    /// The newest major a client can agree on (the newest served one if all are sunset).
    pub fn newest(&self) -> u32 {
        self.negotiable()
            .last()
            .copied()
            .or_else(|| self.policy.served.last().copied())
            .unwrap_or(extend_protocol::API_VERSION)
    }

    /// Sunsets deprecated majors that went quiet and reloads the shared state. Returns the majors
    /// this call sunset.
    pub async fn upkeep(&self) -> anyhow::Result<Vec<u32>> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(7342011)")
            .execute(&mut *tx)
            .await?;
        let rows: Vec<(
            i32,
            String,
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
            Option<Date>,
        )> = sqlx::query_as(
            "SELECT v.api_version, v.state, v.deprecated_at, v.sunset_at,
                    (SELECT max(u.day) FROM extend_global.api_version_usage u
                      WHERE u.api_version = v.api_version AND u.requests > 0)
             FROM extend_global.api_versions v ORDER BY v.api_version",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut majors = Vec::with_capacity(rows.len());
        let mut sunset = Vec::new();
        for (v, state, deprecated_at, sunset_at, last_request_on) in rows {
            let Ok(api_version) = u32::try_from(v) else { continue };
            let served = self.policy.served.contains(&api_version);
            let mut m = Major {
                api_version,
                state: Lifecycle::parse(&state),
                deprecated_at,
                sunset_at,
                last_request_on,
                served,
            };
            if served
                && let Some(at) = m.sunset_earliest_at(now, false)
                && now >= at
            {
                sqlx::query(
                    "UPDATE extend_global.api_versions SET state = 'sunset', sunset_at = $2, updated_at = now()
                     WHERE api_version = $1 AND state = 'deprecated'",
                )
                .bind(v)
                .bind(at)
                .execute(&mut *tx)
                .await?;
                tracing::warn!(
                    api_version,
                    sunset_at = %at,
                    "API version sunset: {SUNSET_QUIET_DAYS} consecutive days without a request since it was deprecated"
                );
                m.state = Lifecycle::Sunset;
                m.sunset_at = Some(at);
                sunset.push(api_version);
            }
            // Keep served majors, and retired ones for the matrix; skip rows another build wrote.
            if served || m.state == Lifecycle::Sunset {
                majors.push(m);
            }
        }
        tx.commit().await?;
        match self.majors.write() {
            Ok(mut slot) => *slot = Arc::new(majors),
            Err(poisoned) => *poisoned.into_inner() = Arc::new(majors),
        }
        Ok(sunset)
    }

    /// Runs [`Registry::upkeep`] every [`UPKEEP_EVERY`] until the registry is dropped.
    pub fn spawn_upkeep(self: &Arc<Self>) {
        let weak: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(UPKEEP_EVERY);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(registry) = weak.upgrade() else { break };
                if let Err(e) = registry.upkeep().await {
                    tracing::warn!(error = %e, "API version upkeep failed; the last known state stays in force");
                }
            }
        });
    }

    /// Counts one request against a major for today (UTC), without delaying the response.
    fn count(&self, major: u32) {
        let pool = self.pool.clone();
        let day = self.now().date();
        let Ok(v) = i32::try_from(major) else { return };
        tokio::spawn(async move {
            if let Err(e) = sqlx::query(
                "INSERT INTO extend_global.api_version_usage (api_version, day, requests) VALUES ($1, $2, 1)
                 ON CONFLICT (api_version, day) DO UPDATE SET requests = extend_global.api_version_usage.requests + 1",
            )
            .bind(v)
            .bind(day)
            .execute(&pool)
            .await
            {
                tracing::warn!(api_version = major, error = %e, "could not count an API request for the sunset rule");
            }
        });
    }

    /// The compatibility matrix `GET /api/v{n}/contracts` returns. `path_major` is the major
    /// the request came in on; it counts as a request today.
    pub fn matrix(&self, device_app_min: &str, path_major: Option<u32>) -> serde_json::Value {
        let now = self.now();
        let versions: Vec<serde_json::Value> = self
            .majors()
            .iter()
            .map(|m| {
                let compat = self
                    .policy
                    .compat
                    .get(&m.api_version)
                    .cloned()
                    .unwrap_or_else(|| Compat::default_for(m.api_version));
                serde_json::json!({
                    "api_version": m.api_version,
                    "state": m.state.as_str(),
                    "deprecated_at": m.deprecated_at.map(rfc3339),
                    "sunset_at": m.sunset_at.map(rfc3339),
                    "sunset_earliest_at": m
                        .sunset_earliest_at(now, path_major == Some(m.api_version))
                        .map(rfc3339),
                    "last_request_on": m.last_request_on.map(|d| d.to_string()),
                    "sunset_rule": SUNSET_RULE,
                    "compatible": {
                        "client_crate": compat.client_crate,
                        "cli": compat.cli,
                        "device_app_min": device_app_min,
                    },
                })
            })
            .collect();
        serde_json::json!({
            "service_version": env!("CARGO_PKG_VERSION"),
            "current": self.newest(),
            "supported": self.negotiable(),
            "deprecated": self.deprecated(),
            "sunset_rule": SUNSET_RULE,
            "versions": versions,
        })
    }

    /// `Deprecation` and `Sunset` headers for a response served on `major` now, if it's deprecated.
    pub fn deprecation_headers(&self, major: &Major) -> Vec<(&'static str, HeaderValue)> {
        let now = self.now();
        let (Some(deprecated_at), Some(sunset)) = (major.deprecated_at, major.sunset_earliest_at(now, true)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Ok(v) = HeaderValue::from_str(&format!("@{}", deprecated_at.unix_timestamp())) {
            out.push((DEPRECATION_HEADER, v));
        }
        if let Ok(v) = HeaderValue::from_str(&http_date(sunset)) {
            out.push((SUNSET_HEADER, v));
        }
        out
    }

    /// `410 api_version_sunset` for a request on a sunset major. `consequence` finishes the
    /// message: what no longer works because of it.
    pub fn sunset_error(&self, major: &Major, consequence: &str) -> AppError {
        let when = major.sunset_at.map(|t| format!(" on {}", t.date())).unwrap_or_default();
        let why = match major.deprecated_at {
            Some(t) => format!(
                "it was deprecated on {}, then went {SUNSET_QUIET_DAYS} consecutive days without a request",
                t.date()
            ),
            None => format!("it went {SUNSET_QUIET_DAYS} consecutive days without a request after it was deprecated"),
        };
        let newer = self.negotiable();
        let newer_list = newer.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        AppError::new(
            ErrorCode::ApiVersionSunset,
            format!(
                "API version {} was retired{when} ({why}), so {consequence}",
                major.api_version
            ),
        )
        .hint(format!(
            "Update: `honeycomb install 'extend'` for the CLI, a newer silicon-extend-client for Rust code, or the \
             latest Extend app on the device.{}",
            if newer.is_empty() {
                String::new()
            } else {
                format!(" The update agrees API version {newer_list} with Extend through GET /api/version.")
            }
        ))
        .details(serde_json::json!({
            "api_version": major.api_version,
            "sunset_at": major.sunset_at.map(rfc3339),
            "supported": newer,
        }))
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// An RFC 9110 IMF-fixdate, as the `Sunset` header takes.
pub fn http_date(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    t.format(time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    ))
    .unwrap_or_default()
}

/// The major a path is under: `/api/v2/devices` → 2. `None` for unversioned paths.
pub fn path_major(path: &str) -> Option<u32> {
    let rest = path.strip_prefix("/api/v")?;
    let digits = rest.split('/').next().unwrap_or_default();
    if digits.is_empty() || digits.len() > 4 || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The version layer (see the module docs). State: the [`Registry`].
pub async fn layer(State(registry): State<Arc<Registry>>, req: Request, next: Next) -> Response {
    let Some(requested) = path_major(req.uri().path()) else {
        let mut resp = next.run(req).await;
        resp.headers_mut()
            .insert(API_VERSION_HEADER, HeaderValue::from(registry.newest()));
        return resp;
    };
    let stamp = |mut resp: Response, v: u32| {
        resp.headers_mut().insert(API_VERSION_HEADER, HeaderValue::from(v));
        resp
    };
    let Some(major) = registry.major(requested) else {
        let served = registry.negotiable();
        let list = served.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let err = AppError::new(
            ErrorCode::ApiVersionUnsupported,
            format!("This service does not serve API version {requested} (the path starts with /api/v{requested}/); it serves {list}."),
        )
        .hint("Negotiate with GET /api/version (the CLI and silicon-extend-client do this when they start) and use the path of the version it agrees.")
        .details(serde_json::json!({"requested": requested, "service": served}));
        return stamp(err.into_response(), registry.newest());
    };
    if let Some(pin) = req.headers().get(API_VERSION_HEADER).and_then(|v| v.to_str().ok())
        && pin.trim() != requested.to_string()
    {
        let err = AppError::new(
            ErrorCode::ApiVersionMismatch,
            format!("The client pinned API version {pin}, but this path is version {requested}."),
        )
        .hint("Negotiate with GET /api/version and use the matching path.");
        return stamp(err.into_response(), requested);
    }
    if major.state == Lifecycle::Sunset {
        let err = registry.sunset_error(&major, &format!("/api/v{requested}/ paths no longer answer."));
        return stamp(err.into_response(), requested);
    }
    registry.count(requested);
    let mut resp = next.run(req).await;
    if major.state == Lifecycle::Deprecated {
        for (name, value) in registry.deprecation_headers(&major) {
            resp.headers_mut().insert(name, value);
        }
    }
    stamp(resp, requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn policy_defaults_and_overrides() {
        let p = Policy::parse(&[1], env(&[])).unwrap();
        assert!(p.deprecated.is_empty());
        assert_eq!(p.served, vec![1]);
        assert_eq!(p.compat[&1].client_crate, ">=1.0.0, <3.0.0");
        assert_eq!(p.compat[&1].cli, ">=1.0.0, <3.0.0");
        let p = Policy::parse(
            &[1, 2],
            env(&[
                ("EXTEND_DEPRECATED_API_VERSIONS", " 1 "),
                ("EXTEND_API_V2_CLI", ">=2.1.0, <3.0.0"),
            ]),
        )
        .unwrap();
        assert_eq!(p.deprecated, BTreeSet::from([1]));
        assert_eq!(p.compat[&2].cli, ">=2.1.0, <3.0.0");
        assert_eq!(p.compat[&2].client_crate, ">=2.0.0, <3.0.0");
    }

    #[test]
    fn policy_refusals_say_what_to_do() {
        let e = Policy::parse(&[1], env(&[("EXTEND_DEPRECATED_API_VERSIONS", "1")])).unwrap_err();
        assert!(e.to_string().contains("newest this build serves"), "{e}");
        let e = Policy::parse(&[1, 2], env(&[("EXTEND_DEPRECATED_API_VERSIONS", "3")])).unwrap_err();
        assert!(
            e.to_string().contains("does not serve") && e.to_string().contains("1, 2"),
            "{e}"
        );
        let e = Policy::parse(&[1, 2], env(&[("EXTEND_DEPRECATED_API_VERSIONS", "v1")])).unwrap_err();
        assert!(e.to_string().contains("whole numbers"), "{e}");
        let e = Policy::parse(&[1], env(&[("EXTEND_API_V1_CLI", "1.x")])).unwrap_err();
        assert!(
            e.to_string().contains("EXTEND_API_V1_CLI must be a version range"),
            "{e}"
        );
    }

    #[test]
    fn paths_name_their_major() {
        assert_eq!(path_major("/api/v1/devices"), Some(1));
        assert_eq!(path_major("/api/v2"), Some(2));
        assert_eq!(path_major("/api/v12/x"), Some(12));
        assert_eq!(path_major("/api/version"), None);
        assert_eq!(path_major("/api/v01/x"), None);
        assert_eq!(path_major("/api/v/x"), None);
        assert_eq!(path_major("/webhook"), None);
        assert_eq!(path_major("/internal/honeycomb/x"), None);
    }

    #[test]
    fn ranges() {
        assert!(valid_range(">=1.0.0, <2.0.0"));
        assert!(valid_range("=1.4.2"));
        assert!(valid_range("1.4.2"));
        assert!(!valid_range(">=1.0, <2"));
        assert!(!valid_range("^1"));
        assert!(!valid_range(""));
    }

    fn deprecated(at: OffsetDateTime, last: Option<Date>) -> Major {
        Major {
            api_version: 1,
            state: Lifecycle::Deprecated,
            deprecated_at: Some(at),
            sunset_at: None,
            last_request_on: last,
            served: true,
        }
    }

    #[test]
    fn sunset_waits_seven_quiet_days_after_deprecation_and_last_request() {
        let at = datetime!(2026-09-20 10:00 UTC);
        let now = datetime!(2026-09-21 12:00 UTC);
        // No requests since before the deprecation: 7 days from the deprecation.
        let m = deprecated(at, Some(time::macros::date!(2026 - 09 - 19)));
        assert_eq!(m.sunset_earliest_at(now, false), Some(datetime!(2026-09-27 10:00 UTC)));
        // A request on the 23rd: 7 days from the end of the 23rd.
        let m = deprecated(at, Some(time::macros::date!(2026 - 09 - 23)));
        assert_eq!(m.sunset_earliest_at(now, false), Some(datetime!(2026-10-01 00:00 UTC)));
        // The request being served now counts for today.
        let m = deprecated(at, None);
        assert_eq!(m.sunset_earliest_at(now, true), Some(datetime!(2026-09-29 00:00 UTC)));
        // Current and sunset majors have no pending sunset.
        let mut c = m.clone();
        c.state = Lifecycle::Current;
        assert_eq!(c.sunset_earliest_at(now, true), None);
    }

    #[test]
    fn sunset_header_is_an_imf_fixdate() {
        assert_eq!(
            http_date(datetime!(2026-10-01 00:00 UTC)),
            "Thu, 01 Oct 2026 00:00:00 GMT"
        );
    }
}
