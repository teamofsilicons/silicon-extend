//! Shared state and the request extractors every handler uses.
//!
//! # Which world a request is in
//!
//! A request is in production unless it carries `X-Testing-Application-Secret`. When it does, the
//! secret must select an open test environment, or the request is refused (`testing_secret_invalid`
//! or `testing_environment_not_ready`) — it never runs in production instead. [`selection_layer`]
//! applies this to every `/api/v{n}/` route, including the ones that don't sign anyone in
//! (enrollments, `/api/v1/iam`, reports, telemetry). `GET /api/version` stays unscoped: it is the
//! same capability handshake in every world, carries no data, and clients call it before they can
//! show a precise error.
//!
//! # When a test environment is open
//!
//! Honeycomb's `prepare` and `restore` leave the environment `preparing`: Extend's own part is
//! done, but test access opens only once Honeycomb confirms every service is ready. Honeycomb
//! confirms that to IAM (its `activate` phase), and IAM refuses the environment's app_secret until
//! then, so a live IAM answer that accepts the secret is the confirmation Extend acts on (the
//! participant `activate` instruction confirms it too). IAM is asked live for every environment
//! that isn't open yet; an open one's answer is reused for at most [`SELECTION_TTL`] and never
//! across a lifecycle change.
//!
//! # The clean fence
//!
//! Every request in a test world holds a read guard on that world's fence for as long as it runs;
//! `clean`, `disable` and `purge` take the write guard before they wipe or close the world, so a
//! request admitted before the lifecycle change can't write into the world after it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use extend_protocol::{ErrorCode, PairingCode, TEAM_HEADER, TESTING_SECRET_HEADER, ids};
use sqlx::PgPool;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use uuid::Uuid;

use crate::config::Config;
use crate::db::{self, World};
use crate::error::{AppError, AppResult};
use crate::files::DynFiles;
use crate::hub::Hub;
use crate::iam::{AuthCache, DynIam, LocalIam, Principal, TestingSelection};
use crate::ting::DynNotifier;

/// How long IAM's answer for an open test environment's secret is reused.
pub const SELECTION_TTL: Duration = Duration::from_secs(10);

pub struct AppState {
    pub cfg: Config,
    pub pool: PgPool,
    pub iam: DynIam,
    pub local_iam: Option<Arc<LocalIam>>,
    pub files: DynFiles,
    pub notifier: DynNotifier,
    pub local_ting: Option<Arc<crate::ting::LocalNotifier>>,
    pub hub: Hub,
    pub auth_cache: AuthCache,
    pub http: reqwest::Client,
    /// Test worlds whose schema is known to exist.
    pub ready_worlds: RwLock<std::collections::HashSet<String>>,
    /// Secret digest → IAM's answer for an open environment, reused for [`SELECTION_TTL`].
    pub selections: RwLock<HashMap<String, (std::time::Instant, TestingSelection)>>,
    /// Secret digest → the environment revision a cached selection was confirmed at; a lifecycle
    /// change (on any instance) moves the revision and so retires the cached answer.
    pub selection_revisions: RwLock<HashMap<String, i64>>,
    /// One fence per test world (see the module docs).
    pub fences: std::sync::Mutex<HashMap<Uuid, Arc<RwLock<()>>>>,
    /// The Silicon behind each running session, kept to store its files and re-check it on webhooks.
    pub session_principals: RwLock<HashMap<(String, String), (Principal, Option<TestingSelection>)>>,
    /// Sliding-window counters for rate limits (pairing guesses, enrollments, reports).
    pub limits: tokio::sync::Mutex<HashMap<String, Vec<std::time::Instant>>>,
    /// Positive answers of the owner-active check (crate::membership).
    pub owner_cache: crate::membership::OwnerCache,
    /// (world schema, member) with a Ting waiting for that member's login: the member is a
    /// possible sender of a pending Ting Extend holds no login for. Their next authenticated call
    /// sends it (see [`Auth`]). Rebuilt from the database at start and when a test world opens.
    pub waiting_logins: RwLock<std::collections::HashSet<(String, String)>>,
    /// (world schema, Carbon) whose Ting registration per Team was checked since the start.
    pub ting_checked: tokio::sync::Mutex<std::collections::HashSet<(String, String)>>,
}

pub type Shared = Arc<AppState>;

/// A read guard on a test world's fence: held while a request (or job, or webhook) may write to it.
pub type FenceGuard = OwnedRwLockReadGuard<()>;

/// The outcome of selecting a world, with the fence guard for a test world.
pub struct Selection {
    pub world: World,
    pub sel: Option<TestingSelection>,
    pub fence: Option<FenceGuard>,
}

/// A test environment's row, as selection needs it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnvironmentRow {
    pub state: String,
    pub name: String,
    pub environment_revision: i64,
}

impl AppState {
    /// Records one event under `key` and fails when more than `max` happened within `window`.
    pub async fn rate_limit(&self, key: String, max: usize, window: std::time::Duration, what: &str) -> AppResult<()> {
        let mut limits = self.limits.lock().await;
        if limits.len() > 100_000 {
            limits.retain(|_, v| v.last().is_some_and(|t| t.elapsed() < window));
        }
        let hits = limits.entry(key).or_default();
        hits.retain(|t| t.elapsed() < window);
        if hits.len() >= max {
            let wait = window.saturating_sub(hits[0].elapsed()).as_secs() + 1;
            return Err(AppError::new(
                ErrorCode::RateLimited,
                format!(
                    "Too many {what}; the limit is {max} per {} minutes.",
                    window.as_secs() / 60
                ),
            )
            .hint(format!("Retry in {wait} seconds."))
            .details(serde_json::json!({"retry_after_s": wait})));
        }
        hits.push(std::time::Instant::now());
        Ok(())
    }

    /// Fails when `key` already had `max` events within `window`, without recording one.
    pub async fn rate_peek(&self, key: &str, max: usize, window: std::time::Duration, what: &str) -> AppResult<()> {
        let limits = self.limits.lock().await;
        if let Some(hits) = limits.get(key) {
            let recent: Vec<_> = hits.iter().filter(|t| t.elapsed() < window).collect();
            if recent.len() >= max {
                let wait = window.saturating_sub(recent[0].elapsed()).as_secs() + 1;
                return Err(AppError::new(
                    ErrorCode::RateLimited,
                    format!(
                        "Too many {what}; the limit is {max} per {} minutes.",
                        window.as_secs() / 60
                    ),
                )
                .hint(format!("Retry in {wait} seconds."))
                .details(serde_json::json!({"retry_after_s": wait})));
            }
        }
        Ok(())
    }

    /// Remembers that pending Tings wait for one of these members' logins.
    pub async fn ting_waiting(&self, world: &World, members: &[String]) {
        if members.is_empty() {
            return;
        }
        let mut w = self.waiting_logins.write().await;
        for m in members {
            w.insert((world.schema.clone(), m.clone()));
        }
    }

    /// Clears a counter (a successful pairing forgives earlier wrong guesses).
    pub async fn rate_reset(&self, key: &str) {
        self.limits.lock().await.remove(key);
    }

    /// The fence of one test world.
    pub fn fence(&self, environment_id: Uuid) -> Arc<RwLock<()>> {
        let mut fences = self.fences.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        fences.entry(environment_id).or_default().clone()
    }

    /// Waits until nothing admitted into the world is still running, and keeps new work out
    /// until the guard is dropped.
    pub async fn fence_exclusive(&self, environment_id: Uuid) -> OwnedRwLockWriteGuard<()> {
        self.fence(environment_id).write_owned().await
    }

    /// For background work: `Some` (holding the fence of a test world) when the world is open and
    /// work may write to it, `None` when a test world is being prepared, cleaned, disabled or was
    /// removed. Production is always open.
    pub async fn world_open(&self, world: &World) -> Option<Option<FenceGuard>> {
        let Some(env) = world.environment_id else {
            return Some(None);
        };
        let guard = self.fence(env).read_owned().await;
        match self.environment(env).await {
            Ok(Some(row)) if row.state == "ready" => Some(Some(guard)),
            _ => None,
        }
    }

    pub async fn environment(&self, environment_id: Uuid) -> AppResult<Option<EnvironmentRow>> {
        Ok(sqlx::query_as(
            "SELECT state, name, environment_revision FROM extend_global.test_environments WHERE environment_id = $1",
        )
        .bind(environment_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Drops every cached IAM answer for one environment (after a lifecycle change).
    pub async fn forget_selections(&self, environment_id: Uuid) {
        let mut selections = self.selections.write().await;
        let gone: Vec<String> = selections
            .iter()
            .filter(|(_, (_, s))| s.environment_id == environment_id)
            .map(|(d, _)| d.clone())
            .collect();
        for d in &gone {
            selections.remove(d);
        }
        drop(selections);
        let mut revisions = self.selection_revisions.write().await;
        for d in &gone {
            revisions.remove(d);
        }
    }

    async fn forget_selection(&self, digest: &str) {
        self.selections.write().await.remove(digest);
        self.selection_revisions.write().await.remove(digest);
    }

    /// Resolves the `X-Testing-Application-Secret` value into a world, without taking the
    /// world's fence (a request under [`selection_layer`] already holds it; taking it twice could
    /// deadlock behind a waiting clean).
    pub async fn select_world(&self, secret: Option<&str>) -> AppResult<(World, Option<TestingSelection>)> {
        let s = self.select(secret, false).await?;
        Ok((s.world, s.sel))
    }

    /// Resolves the `X-Testing-Application-Secret` value into a world and holds that world's
    /// fence. `None` is production. A secret that doesn't select an open test environment is an
    /// error, never production.
    pub async fn select_world_fenced(&self, secret: Option<&str>) -> AppResult<Selection> {
        self.select(secret, true).await
    }

    async fn fence_read(&self, environment_id: Uuid, take: bool) -> Option<FenceGuard> {
        if take {
            Some(self.fence(environment_id).read_owned().await)
        } else {
            None
        }
    }

    async fn select(&self, secret: Option<&str>, take_fence: bool) -> AppResult<Selection> {
        let Some(secret) = secret else {
            return Ok(Selection {
                world: World::production(),
                sel: None,
                fence: None,
            });
        };
        let digest = ids::secret_digest(secret);
        // An open environment's recent IAM answer, if nothing changed since.
        let cached = self
            .selections
            .read()
            .await
            .get(&digest)
            .filter(|(at, _)| at.elapsed() < SELECTION_TTL)
            .map(|(_, s)| s.clone());
        if let Some(sel) = cached {
            let fence = self.fence_read(sel.environment_id, take_fence).await;
            let at = self.selection_revisions.read().await.get(&digest).copied();
            if let Some(env) = self.environment(sel.environment_id).await?
                && env.state == "ready"
                && Some(env.environment_revision) == at
            {
                return self.admit(sel, fence).await;
            }
            drop(fence);
            self.forget_selection(&digest).await;
        }
        // Ask IAM now: it answers for the environment's current state.
        let (environment_id, name) = match self.iam.select_testing(secret).await {
            Ok(found) => found,
            Err(e) => return Err(self.refused(&digest, e).await),
        };
        let sel = TestingSelection {
            environment_id,
            name,
            secret: secret.to_owned(),
        };
        let fence = self.fence_read(environment_id, take_fence).await;
        let mut env = self.environment(environment_id).await?;
        if let Some(row) = &env
            && row.state == "preparing"
        {
            // IAM accepting the secret is Honeycomb's readiness confirmation (module docs).
            let opened = sqlx::query(
                "UPDATE extend_global.test_environments SET state = 'ready'
                 WHERE environment_id = $1 AND state = 'preparing' AND environment_revision = $2",
            )
            .bind(environment_id)
            .bind(row.environment_revision)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if opened == 1 {
                tracing::info!(environment_id = %environment_id, "IAM confirmed the test environment is ready; test access is open");
            }
            env = self.environment(environment_id).await?;
        }
        let row = open_or_refuse(env, &sel.name)?;
        // Remember the confirmation: which environment the secret selects (for a precise answer
        // if IAM later refuses it while the environment is being restored), and IAM's test webhook
        // key digest (so signed test deliveries route after a restart).
        let _ = sqlx::query(
            "INSERT INTO extend_global.test_secret_bindings (secret_digest, environment_id) VALUES ($1, $2)
             ON CONFLICT (secret_digest) DO UPDATE SET environment_id = EXCLUDED.environment_id, confirmed_at = now()",
        )
        .bind(&digest)
        .bind(environment_id)
        .execute(&self.pool)
        .await;
        if let Some(d) = self.iam.test_webhook_digest(environment_id).await {
            let _ = sqlx::query(
                "UPDATE extend_global.test_environments SET webhook_key_digest = $2
                 WHERE environment_id = $1 AND webhook_key_digest IS DISTINCT FROM $2",
            )
            .bind(environment_id)
            .bind(d)
            .execute(&self.pool)
            .await;
        }
        self.selections
            .write()
            .await
            .insert(digest.clone(), (Instant::now(), sel.clone()));
        self.selection_revisions
            .write()
            .await
            .insert(digest, row.environment_revision);
        self.admit(sel, fence).await
    }

    /// Opens an admitted selection: makes sure the schema exists and reports activity.
    async fn admit(&self, sel: TestingSelection, fence: Option<FenceGuard>) -> AppResult<Selection> {
        let world = World::test(sel.environment_id);
        if !self.ready_worlds.read().await.contains(&world.schema) {
            db::ensure_world(&self.pool, &world).await?;
            self.ready_worlds.write().await.insert(world.schema.clone());
            crate::scheduler::rebuild_waiting(self, &world).await;
        }
        // Honeycomb decides expiry; Extend only reports activity.
        let _ = sqlx::query(
            "UPDATE extend_global.test_environments SET last_activity_at = now() WHERE environment_id = $1",
        )
        .bind(sel.environment_id)
        .execute(&self.pool)
        .await;
        Ok(Selection {
            world,
            sel: Some(sel),
            fence,
        })
    }

    /// IAM refused a secret. When Extend has seen IAM confirm this secret before, say precisely
    /// why its environment isn't open (IAM itself answers the same for every refusal).
    async fn refused(&self, digest: &str, err: AppError) -> AppError {
        self.forget_selection(digest).await;
        if err.code() != ErrorCode::TestingSecretInvalid {
            return err;
        }
        let known: Option<(String, String)> = sqlx::query_as(
            "SELECT e.state, e.name FROM extend_global.test_secret_bindings b
             JOIN extend_global.test_environments e USING (environment_id) WHERE b.secret_digest = $1",
        )
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        match known {
            Some((state, name)) if state == "preparing" => AppError::new(
                ErrorCode::TestingEnvironmentNotReady,
                format!(
                    "Test environment {name} is not open yet: Extend's part is ready, but Honeycomb has not confirmed \
                     that every service it needs is ready, so Silicon IAM still refuses its app_secret. Nothing ran in production."
                ),
            )
            .hint("Wait until Honeycomb reports the environment ready (after it is created or restored), then retry."),
            Some((state, name)) if state != "ready" => {
                open_or_refuse(Some(EnvironmentRow { state, name: name.clone(), environment_revision: 0 }), &name)
                    .err()
                    .unwrap_or(err)
            }
            _ => err,
        }
    }

    pub async fn authorize(
        &self,
        token: &str,
        team: Option<&str>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Principal> {
        let env = sel.map(|s| s.environment_id);
        if let Some(p) = self.auth_cache.get(token, team, env).await {
            return Ok(p);
        }
        let p = self.iam.authorize(token, team, sel).await?;
        self.auth_cache.put(token, team, env, p.clone()).await;
        Ok(p)
    }
}

/// `Ok` when a test environment with this row is open; the precise refusal otherwise.
fn open_or_refuse(env: Option<EnvironmentRow>, name: &str) -> AppResult<EnvironmentRow> {
    let Some(row) = env else {
        return Err(AppError::new(
            ErrorCode::TestingEnvironmentNotReady,
            format!(
                "Test environment {name} exists in Silicon IAM, but Honeycomb has not prepared Extend's part of it yet, \
                 so Extend has nowhere to put its data. Nothing ran in production."
            ),
        )
        .hint("Wait for Honeycomb to finish creating the environment (it prepares every service), then retry."));
    };
    match row.state.as_str() {
        "ready" => Ok(row),
        "preparing" => Err(AppError::new(
            ErrorCode::TestingEnvironmentNotReady,
            format!(
                "Test environment {name} is not open yet: Extend's part is ready, but Honeycomb has not confirmed that \
                 every service it needs is ready. Nothing ran in production."
            ),
        )
        .hint("Wait until Honeycomb reports the environment ready, then retry.")),
        "cleaning" => Err(AppError::new(
            ErrorCode::TestingEnvironmentNotReady,
            format!(
                "Test environment {name} is being cleaned by Honeycomb, so it is closed until the clean finishes. \
                 Nothing ran in production."
            ),
        )
        .hint("Retry once Honeycomb reports the clean finished. Devices paired into it are unpaired by the clean.")),
        "disabled" => Err(AppError::new(
            ErrorCode::TestingSecretInvalid,
            format!(
                "Test environment {name} is disabled by Honeycomb, so its app_secret selects nothing right now. \
                 Nothing ran in production."
            ),
        )
        .hint("Ask whoever manages the environment in Honeycomb to restore it, or leave testing to use production.")),
        _ => Err(AppError::new(
            ErrorCode::TestingSecretInvalid,
            format!("Test environment {name} was permanently removed by Honeycomb. Nothing ran in production."),
        )
        .hint(
            "Create a new test environment in Honeycomb and use its app_secret, or leave testing to use production.",
        )),
    }
}

/// The `X-Testing-Application-Secret` a request carries. A header that is present but empty,
/// repeated, or not printable is refused rather than read as "no secret" (production).
pub fn testing_secret_header(headers: &HeaderMap) -> AppResult<Option<String>> {
    let refuse = |why: &str| {
        AppError::new(
            ErrorCode::TestingSecretInvalid,
            format!(
                "The {TESTING_SECRET_HEADER} header is {why}, so Extend can't tell which test environment you meant. \
                 Nothing ran in production."
            ),
        )
        .hint(
            "Send the test application's app_secret from Honeycomb exactly once, or drop the header to use production.",
        )
    };
    let mut values = headers.get_all(TESTING_SECRET_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(refuse("sent more than once"));
    }
    let text = value
        .to_str()
        .map_err(|_| refuse("not printable text (an app_secret is ask_ followed by 43 letters, digits, - or _)"))?
        .trim();
    if text.is_empty() {
        return Err(refuse("empty"));
    }
    Ok(Some(text.to_owned()))
}

/// Whether a path is part of the versioned API (`/api/v1/…`, `/api/v2/…`).
fn is_api_path(path: &str) -> bool {
    path.strip_prefix("/api/v").is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with('/')
    })
}

/// What [`selection_layer`] resolved, for the extractors.
#[derive(Clone)]
pub struct Selected {
    pub world: World,
    pub sel: Option<TestingSelection>,
}

/// Applies the test-environment secret to every API route (see the module docs), holds the test
/// world's fence while the request runs, and keeps pairing codes inside the world they were made in.
pub async fn selection_layer(State(state): State<Shared>, mut req: Request, next: Next) -> Response {
    if !is_api_path(req.uri().path()) {
        return next.run(req).await;
    }
    let secret = match testing_secret_header(req.headers()) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let selection = match state.select_world_fenced(secret.as_deref()).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if secret.is_some() {
        req.extensions_mut().insert(Selected {
            world: selection.world.clone(),
            sel: selection.sel.clone(),
        });
    }
    if req.method() == Method::POST && req.uri().path().ends_with("/pairings") {
        let (mut parts, body) = req.into_parts();
        // Where a live code was made is told only to a signed-in Carbon of a team, the only caller
        // the claim itself would answer. Anyone else gets the claim's own refusal: this layer runs
        // right before the handler, whose first check is the same sign-in.
        let auth = match Auth::from_request_parts(&mut parts, &state).await {
            Ok(a) => a,
            Err(e) => return e.into_response(),
        };
        let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
            Ok(b) => b,
            Err(e) => {
                return AppError::invalid(format!("Could not read the pairing request (at most 64 KiB): {e}"))
                    .into_response();
            }
        };
        // Since 1.1 a claim needs no Team (devices belong to the Carbons who paired them).
        if auth.require_carbon().is_ok()
            && let Err(e) = crate::routes::enroll::claim_world_check(&state, &selection, &bytes, auth.p.id()).await
        {
            return e.into_response();
        }
        req = Request::from_parts(parts, axum::body::Body::from(bytes));
    }
    let resp = next.run(req).await;
    drop(selection.fence);
    resp
}

fn header<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts.headers.get(name).and_then(|v| v.to_str().ok())
}

fn bearer(parts: &Parts) -> Option<&str> {
    header(parts, "authorization").and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
}

/// The world a request is in (production, or the test environment its secret selects). No login.
pub struct Sel {
    pub world: World,
    pub sel: Option<TestingSelection>,
}

impl FromRequestParts<Shared> for Sel {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> AppResult<Self> {
        if let Some(s) = parts.extensions.get::<Selected>() {
            return Ok(Self {
                world: s.world.clone(),
                sel: s.sel.clone(),
            });
        }
        let secret = testing_secret_header(&parts.headers)?;
        let (world, sel) = state.select_world(secret.as_deref()).await?;
        Ok(Self { world, sel })
    }
}

/// A signed-in member, the world, and the team named by `X-Org-ID` if any.
pub struct Auth {
    pub world: World,
    pub sel: Option<TestingSelection>,
    pub p: Principal,
    /// The internal Silicon acting (`X-Silicon-ISI`), when the caller says. Extra context only.
    pub isi: Option<String>,
}

impl Auth {
    pub fn team(&self) -> AppResult<&str> {
        self.p.team()
    }
    pub fn require_carbon(&self) -> AppResult<()> {
        if self.p.is_carbon() {
            Ok(())
        } else {
            Err(AppError::new(
                ErrorCode::CarbonOnly,
                format!("Only Carbons can do this; {} is a Silicon.", self.p.id()),
            )
            .hint("Ask the Carbon who owns the device to do it on extend.teamofsilicons.com or with the extend CLI."))
        }
    }
    pub fn require_silicon(&self) -> AppResult<()> {
        if self.p.is_silicon() {
            Ok(())
        } else {
            Err(AppError::new(
                ErrorCode::SiliconOnly,
                format!(
                    "Only Silicons use devices through sessions; {} is a Carbon.",
                    self.p.id()
                ),
            )
            .hint("Give a Silicon access with `extend device access grant <device_id> <silicon_id>`."))
        }
    }
}

impl FromRequestParts<Shared> for Auth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> AppResult<Self> {
        let Sel { world, sel } = Sel::from_request_parts(parts, state).await?;
        let token = bearer(parts).ok_or_else(|| {
            AppError::new(ErrorCode::NotSignedIn, "No access token was sent.")
                .hint("Sign in with `extend login <slt>` (CLI) or at extend.teamofsilicons.com, then send Authorization: Bearer <token>.")
        })?;
        let team = header(parts, TEAM_HEADER).map(str::trim).filter(|s| !s.is_empty());
        if let Some(t) = team
            && (t.len() > 128 || !t.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err(AppError::invalid("X-Org-ID must be one team handle."));
        }
        let p = state.authorize(token, team, sel.as_ref()).await?;
        auth_hook(state, &world, &p).await;
        let isi = header(parts, "x-silicon-isi")
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
            .map(str::to_owned);
        Ok(Self { world, sel, p, isi })
    }
}

/// After a member's call is authenticated: sends the Tings that waited for their login (they are a
/// possible sender), and, the first time a Carbon calls after a start, registers them with Ting in
/// each Team their login reaches where they gave access and Extend has no record for them. Both run
/// in the background and check that the world is still open first. The check here is one lookup.
async fn auth_hook(state: &Shared, world: &World, p: &crate::iam::Principal) {
    let key = (world.schema.clone(), p.id().to_owned());
    let waiting = state.waiting_logins.read().await.contains(&key);
    let first = p.is_carbon() && state.ting_checked.lock().await.insert(key.clone());
    if !waiting && !first {
        return;
    }
    if waiting {
        state.waiting_logins.write().await.remove(&key);
    }
    let (state, world, p) = (state.clone(), world.clone(), p.clone());
    tokio::spawn(async move {
        let Some(_fence) = state.world_open(&world).await else {
            return;
        };
        if waiting {
            crate::scheduler::deliver_for(&state, &world, p.id()).await;
        }
        if first {
            let teams: Vec<String> = sqlx::query_scalar(sql!(
                "SELECT DISTINCT team FROM {} WHERE granted_by = $1",
                world.t("device_access")
            ))
            .bind(p.id())
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default();
            for team in teams.iter().filter(|t| p.teams.contains(t)) {
                crate::delivery::register_carbon_if_new(&state, &world, &p, team);
            }
        }
    });
}

/// A paired device, authenticated by its credential.
pub struct DeviceAuth {
    pub world: World,
    pub device_id: String,
    /// The test world's fence, when [`selection_layer`] doesn't already hold it for this request.
    pub fence: Option<FenceGuard>,
}

impl FromRequestParts<Shared> for DeviceAuth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> AppResult<Self> {
        let cred = header(parts, "authorization")
            .and_then(|v| v.strip_prefix("Extend-Device "))
            .map(str::trim)
            .filter(|c| ids::is_secret(ids::DEVICE_CREDENTIAL_PREFIX, c))
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Unauthorized,
                    "Send the device credential as Authorization: Extend-Device <credential>.",
                )
            })?;
        let not_paired = || {
            AppError::new(
                ErrorCode::Unauthorized,
                "This device credential is not paired (the pair was revoked, removed or expired).",
            )
            .hint("Show the pairing screen and pair the device again.")
        };
        let (world, device_id) = device_by_credential(state, cred).await?.ok_or_else(not_paired)?;
        let selected = parts.extensions.get::<Selected>().cloned();
        if let Some(s) = &selected
            && s.world != world
        {
            return Err(AppError::new(
                ErrorCode::TestingSecretInvalid,
                format!(
                    "This device is paired in {}, not in the environment the {TESTING_SECRET_HEADER} header selects.",
                    describe(&world)
                ),
            )
            .hint("Send the secret of the environment the device is paired in, or none for production."));
        }
        let Some(env) = world.environment_id else {
            return Ok(Self {
                world,
                device_id,
                fence: None,
            });
        };
        // The request holds the fence (unless the layer already does), then reads the state.
        let fence = match selected {
            Some(_) => None,
            None => Some(state.fence(env).read_owned().await),
        };
        let row = state.environment(env).await?.ok_or_else(not_paired)?;
        let keep = "This device stays paired; nothing to do on it: its Extend app reconnects by itself once the environment is open again.";
        match row.state.as_str() {
            "ready" => Ok(Self { world, device_id, fence }),
            "disabled" => Err(AppError::new(
                ErrorCode::TestingEnvironmentNotReady,
                format!("Test environment {} is disabled by Honeycomb, so its devices can't be used right now.", row.name),
            )
            .hint(keep)),
            "preparing" => Err(AppError::new(
                ErrorCode::TestingEnvironmentNotReady,
                format!(
                    "Test environment {} is waiting for Honeycomb to confirm that every service it needs is ready.",
                    row.name
                ),
            )
            .hint(keep)),
            "cleaning" => Err(AppError::new(
                ErrorCode::TestingEnvironmentNotReady,
                format!("Test environment {} is being cleaned by Honeycomb.", row.name),
            )
            .hint("The clean unpairs every device in it; pair the device again once Honeycomb reports the clean finished.")),
            _ => Err(not_paired()),
        }
    }
}

/// "production" or "test environment <id>", for messages.
pub fn describe(world: &World) -> String {
    match world.environment_id {
        None => "production".into(),
        Some(id) => format!("test environment {id}"),
    }
}

/// Finds which world a device credential belongs to: production, or a test environment that
/// still exists (open, being prepared, disabled or being cleaned — only removal ends the pair).
pub async fn device_by_credential(state: &AppState, cred: &str) -> AppResult<Option<(World, String)>> {
    let digest = ids::secret_digest(cred);
    let mut worlds = vec![World::production()];
    let envs: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT environment_id FROM extend_global.test_environments WHERE state IN ('ready', 'preparing', 'disabled', 'cleaning')",
    )
    .fetch_all(&state.pool)
    .await?;
    worlds.extend(envs.into_iter().map(|(id,)| World::test(id)));
    for world in worlds {
        let found: Option<(String, bool)> = sqlx::query_as(sql!(
            "SELECT device_id, COALESCE(credential_digest = $1, false) FROM {}
             WHERE (credential_digest = $1 OR next_credential_digest = $1) AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(&digest)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or(None);
        if let Some((id, current)) = found {
            if !current {
                // The app already uses the credential a rotation gave it: that confirms it, and
                // the old one stops working.
                sqlx::query(sql!(
                    "UPDATE {} SET credential_digest = next_credential_digest, next_credential_digest = NULL
                     WHERE device_id = $1 AND next_credential_digest = $2",
                    world.t("devices")
                ))
                .bind(&id)
                .bind(&digest)
                .execute(&state.pool)
                .await?;
                tracing::info!(world = %world.schema, device_id = id, "a device connected with its rotated credential; the old one no longer works");
            }
            return Ok(Some((world, id)));
        }
    }
    Ok(None)
}

/// A pairing code's world, parsed from a claim body, for [`selection_layer`].
pub fn claimed_code(body: &[u8]) -> Option<PairingCode> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("data")?.get("pairing_code")?.as_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_secret_header_is_never_read_as_production() {
        let mut h = HeaderMap::new();
        assert_eq!(testing_secret_header(&h).unwrap(), None);
        h.insert(TESTING_SECRET_HEADER, "".parse().unwrap());
        assert_eq!(
            testing_secret_header(&h).unwrap_err().code(),
            ErrorCode::TestingSecretInvalid
        );
        h.insert(TESTING_SECRET_HEADER, "   ".parse().unwrap());
        assert_eq!(
            testing_secret_header(&h).unwrap_err().code(),
            ErrorCode::TestingSecretInvalid
        );
        h.insert(
            TESTING_SECRET_HEADER,
            axum::http::HeaderValue::from_bytes(b"ask_\xe9t\xe9").unwrap(),
        );
        assert_eq!(
            testing_secret_header(&h).unwrap_err().code(),
            ErrorCode::TestingSecretInvalid
        );
        h.insert(TESTING_SECRET_HEADER, " ask_abc ".parse().unwrap());
        assert_eq!(testing_secret_header(&h).unwrap().as_deref(), Some("ask_abc"));
        h.append(TESTING_SECRET_HEADER, "ask_def".parse().unwrap());
        let e = testing_secret_header(&h).unwrap_err();
        assert!(e.0.message.contains("more than once"), "{}", e.0.message);
    }

    #[test]
    fn api_paths() {
        assert!(is_api_path("/api/v1/iam"));
        assert!(is_api_path("/api/v12/devices"));
        assert!(!is_api_path("/api/version"));
        assert!(!is_api_path("/api/v/x"));
        assert!(!is_api_path("/internal/honeycomb/x"));
        assert!(!is_api_path("/webhook"));
    }

    #[test]
    fn claimed_codes_are_normalised() {
        let body = br#"{"type":"pairing","data":{"pairing_code":"4f9c2a","name":"x"}}"#;
        assert_eq!(claimed_code(body).unwrap().as_str(), "4F9C2A");
        assert!(claimed_code(br#"{"data":{"pairing_code":"nope"}}"#).is_none());
    }
}
