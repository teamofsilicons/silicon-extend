//! Shared state and the request extractors every handler uses.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use extend_protocol::{ErrorCode, TEAM_HEADER, TESTING_SECRET_HEADER, ids};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::config::Config;
use crate::db::{self, World};
use crate::error::{AppError, AppResult};
use crate::files::DynFiles;
use crate::hub::Hub;
use crate::iam::{AuthCache, DynIam, LocalIam, Principal, TestingSelection};
use crate::ting::DynNotifier;

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
    /// Secret digest → selection, so a test secret is checked with IAM once a minute, not per request.
    pub selections: RwLock<HashMap<String, (std::time::Instant, TestingSelection)>>,
    /// The Silicon behind each running session, kept to store its files and re-check it on webhooks.
    pub session_principals: RwLock<HashMap<(String, String), (Principal, Option<TestingSelection>)>>,
    /// Sliding-window counters for rate limits (pairing guesses, enrollments, reports).
    pub limits: tokio::sync::Mutex<HashMap<String, Vec<std::time::Instant>>>,
}

pub type Shared = Arc<AppState>;

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
            return Err(AppError::new(ErrorCode::RateLimited, format!("Too many {what}; the limit is {max} per {} minutes.", window.as_secs() / 60))
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
                return Err(AppError::new(ErrorCode::RateLimited, format!("Too many {what}; the limit is {max} per {} minutes.", window.as_secs() / 60))
                    .hint(format!("Retry in {wait} seconds."))
                    .details(serde_json::json!({"retry_after_s": wait})));
            }
        }
        Ok(())
    }

    /// Clears a counter (a successful pairing forgives earlier wrong guesses).
    pub async fn rate_reset(&self, key: &str) {
        self.limits.lock().await.remove(key);
    }

    /// Resolves the `X-Testing-Application-Secret` header into a world.
    pub async fn select_world(&self, secret: Option<&str>) -> AppResult<(World, Option<TestingSelection>)> {
        let Some(secret) = secret.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok((World::production(), None));
        };
        let digest = ids::secret_digest(secret);
        let cached = self.selections.read().await.get(&digest).filter(|(at, _)| at.elapsed().as_secs() < 60).map(|(_, s)| s.clone());
        let sel = match cached {
            Some(s) => s,
            None => {
                let (environment_id, name) = self.iam.select_testing(secret).await?;
                let s = TestingSelection { environment_id, name, secret: secret.to_owned() };
                self.selections.write().await.insert(digest, (std::time::Instant::now(), s.clone()));
                s
            }
        };
        let state: Option<(String, String)> =
            sqlx::query_as("SELECT state, name FROM extend_global.test_environments WHERE environment_id = $1")
                .bind(sel.environment_id)
                .fetch_optional(&self.pool)
                .await?;
        match state.as_ref().map(|(s, _)| s.as_str()) {
            Some("ready") => {}
            Some("cleaning") | Some("preparing") | None => {
                return Err(AppError::new(
                    ErrorCode::TestingEnvironmentNotReady,
                    format!("Test environment {} is not ready in Extend yet (Honeycomb has not finished preparing or cleaning it).", sel.name),
                )
                .hint("Wait for Honeycomb to report the environment ready, then retry."));
            }
            Some(other) => {
                return Err(AppError::new(
                    ErrorCode::TestingSecretInvalid,
                    format!("Test environment {} is {other}; nothing ran in production.", sel.name),
                ));
            }
        }
        let world = World::test(sel.environment_id);
        if !self.ready_worlds.read().await.contains(&world.schema) {
            db::ensure_world(&self.pool, &world).await?;
            self.ready_worlds.write().await.insert(world.schema.clone());
        }
        // Honeycomb decides expiry; Extend only reports activity.
        let _ = sqlx::query("UPDATE extend_global.test_environments SET last_activity_at = now() WHERE environment_id = $1")
            .bind(sel.environment_id)
            .execute(&self.pool)
            .await;
        Ok((world, Some(sel)))
    }

    pub async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal> {
        let env = sel.map(|s| s.environment_id);
        if let Some(p) = self.auth_cache.get(token, team, env).await {
            return Ok(p);
        }
        let p = self.iam.authorize(token, team, sel).await?;
        self.auth_cache.put(token, team, env, p.clone()).await;
        Ok(p)
    }
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
        let (world, sel) = state.select_world(header(parts, TESTING_SECRET_HEADER)).await?;
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
            Err(AppError::new(ErrorCode::CarbonOnly, format!("Only Carbons can do this; {} is a Silicon.", self.p.id()))
                .hint("Ask the Carbon who owns the device to do it on extend.teamofsilicons.com or with the extend CLI."))
        }
    }
    pub fn require_silicon(&self) -> AppResult<()> {
        if self.p.is_silicon() {
            Ok(())
        } else {
            Err(AppError::new(ErrorCode::SiliconOnly, format!("Only Silicons use devices through sessions; {} is a Carbon.", self.p.id()))
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
            && (t.len() > 128 || !t.bytes().all(|b| b.is_ascii_graphic())) {
                return Err(AppError::invalid("X-Org-ID must be one team handle."));
            }
        let p = state.authorize(token, team, sel.as_ref()).await?;
        let isi = header(parts, "x-silicon-isi").map(str::trim).filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)).map(str::to_owned);
        Ok(Self { world, sel, p, isi })
    }
}

/// A paired device, authenticated by its credential.
pub struct DeviceAuth {
    pub world: World,
    pub device_id: String,
}

impl FromRequestParts<Shared> for DeviceAuth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> AppResult<Self> {
        let cred = header(parts, "authorization")
            .and_then(|v| v.strip_prefix("Extend-Device "))
            .map(str::trim)
            .filter(|c| ids::is_secret(ids::DEVICE_CREDENTIAL_PREFIX, c))
            .ok_or_else(|| AppError::new(ErrorCode::Unauthorized, "Send the device credential as Authorization: Extend-Device <credential>."))?;
        let (world, device_id) = device_by_credential(state, cred).await?.ok_or_else(|| {
            AppError::new(ErrorCode::Unauthorized, "This device credential is not paired (the pair was revoked, removed or expired).")
                .hint("Show the pairing screen and pair the device again.")
        })?;
        Ok(Self { world, device_id })
    }
}

/// Finds which world a device credential belongs to.
pub async fn device_by_credential(state: &AppState, cred: &str) -> AppResult<Option<(World, String)>> {
    let digest = ids::secret_digest(cred);
    let mut worlds = vec![World::production()];
    let envs: Vec<(Uuid,)> = sqlx::query_as("SELECT environment_id FROM extend_global.test_environments WHERE state = 'ready'")
        .fetch_all(&state.pool)
        .await?;
    worlds.extend(envs.into_iter().map(|(id,)| World::test(id)));
    for world in worlds {
        let found: Option<(String,)> = sqlx::query_as(sql!(
            "SELECT device_id FROM {} WHERE credential_digest = $1 AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(&digest)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or(None);
        if let Some((id,)) = found {
            return Ok(Some((world, id)));
        }
    }
    Ok(None)
}
