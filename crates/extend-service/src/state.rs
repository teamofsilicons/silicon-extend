//! Shared state and the request extractors every handler uses.
//!
//! - [`Auth`]: a Carbon or Silicon signed in with Silicon Accounts (`Authorization: Bearer
//!   <access token>`, verified locally; see crate::accounts). [`Auth::live`] also asks Silicon
//!   Accounts whether the sign-in is still active, on the routes where a sign-out must take
//!   effect at once.
//! - [`DeviceAuth`]: a paired device app (`Authorization: Extend-Device <credential>`). The
//!   device wire is unchanged from 1.x.
//!
//! There are no Teams and no test environments: every request is about the `extend` schema
//! ([`World::production`]). A request that still sends `X-Testing-Application-Secret` is refused
//! ([`no_test_environments`]) rather than run somewhere it didn't mean.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Request};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use extend_protocol::{ErrorCode, TESTING_SECRET_HEADER, ids};
use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::accounts::{Accounts, Principal};
use crate::config::Config;
use crate::db::World;
use crate::error::{AppError, AppResult};
use crate::files::DynFiles;
use crate::hub::Hub;
use crate::proofs::ProofStore;
use crate::ting::DynNotifier;

pub struct AppState {
    pub cfg: Config,
    pub pool: PgPool,
    pub accounts: Arc<Accounts>,
    pub proofs: Arc<ProofStore>,
    pub files: DynFiles,
    pub notifier: DynNotifier,
    pub local_ting: Option<Arc<crate::ting::LocalNotifier>>,
    pub hub: Hub,
    pub http: reqwest::Client,
    /// The Silicon behind each running session: `ended_while_running` watches it, and the
    /// session's files are stored as it.
    pub session_principals: RwLock<HashMap<(String, String), Principal>>,
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

    /// Clears a counter (a successful pairing forgives earlier wrong guesses).
    pub async fn rate_reset(&self, key: &str) {
        self.limits.lock().await.remove(key);
    }
}

/// Refuses every API request that still selects a Honeycomb test environment: 4.0 has none, and
/// running it in production instead would surprise its sender.
pub async fn no_test_environments(req: Request, next: Next) -> Response {
    if req.headers().contains_key(TESTING_SECRET_HEADER) && is_api_path(req.uri().path()) {
        return AppError::new(
            ErrorCode::TestingSecretInvalid,
            format!(
                "Silicon Extend no longer has test environments, so the {TESTING_SECRET_HEADER} header selects nothing. \
                 Nothing ran."
            ),
        )
        .hint("Send the request without the header. To try Extend without touching anything real, use the development release of the CLI (`extend>dev`) against a local service.")
        .into_response();
    }
    next.run(req).await
}

/// Whether a path is part of the versioned API (`/api/v1/…`, `/api/v2/…`).
fn is_api_path(path: &str) -> bool {
    path.strip_prefix("/api/v").is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with('/')
    })
}

fn header<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts.headers.get(name).and_then(|v| v.to_str().ok())
}

/// The bearer token of a request, if any.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// A signed-in Carbon or Silicon.
pub struct Auth {
    pub world: World,
    pub p: Principal,
    /// The internal Silicon acting (`X-Silicon-ISI`), when the caller says. Extra context only.
    pub isi: Option<String>,
}

impl Auth {
    pub fn require_carbon(&self) -> AppResult<()> {
        if self.p.is_carbon() {
            Ok(())
        } else {
            Err(AppError::new(
                ErrorCode::CarbonOnly,
                format!("Only Carbons can do this; {} is a Silicon.", self.p.public_id()),
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
                    self.p.public_id()
                ),
            )
            .hint("Give a Silicon access with `extend device access grant <device_id> <si:id>`."))
        }
    }

    /// Asks Silicon Accounts whether this sign-in is still active (cached for at most 30 s and
    /// dropped by any webhook about the account): pairing, granting or removing access, removing a
    /// device, starting a session, commands, takeovers, requests, wake requests and file content.
    pub async fn live(&self, state: &AppState) -> AppResult<()> {
        state.accounts.require_live(&self.p).await
    }
}

impl FromRequestParts<Shared> for Auth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> AppResult<Self> {
        let token = bearer(&parts.headers).ok_or_else(|| {
            AppError::new(ErrorCode::NotSignedIn, "No access token was sent.").hint(
                "Sign in with `extend login` (Carbons) or `silicon-accounts login --app extend -q | extend login --slt-stdin` \
                 (Silicons), or at extend.teamofsilicons.com, then send Authorization: Bearer <access token>.",
            )
        })?;
        let p = state.accounts.authenticate(token).await?;
        let isi = header(parts, "x-silicon-isi")
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
            .map(str::to_owned);
        Ok(Self {
            world: World::production(),
            p,
            isi,
        })
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
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Unauthorized,
                    "Send the device credential as Authorization: Extend-Device <credential>.",
                )
            })?;
        let device_id = device_by_credential(state, cred).await?.ok_or_else(|| {
            AppError::new(
                ErrorCode::Unauthorized,
                "This device credential is not paired (the pair was revoked, removed or expired).",
            )
            .hint("Show the pairing screen and pair the device again.")
        })?;
        Ok(Self {
            world: World::production(),
            device_id,
        })
    }
}

/// Finds the pair a device credential belongs to.
pub async fn device_by_credential(state: &AppState, cred: &str) -> AppResult<Option<String>> {
    let digest = ids::secret_digest(cred);
    let world = World::production();
    let found: Option<(String, bool)> = sqlx::query_as(sql!(
        "SELECT device_id, COALESCE(credential_digest = $1, false) FROM {}
         WHERE (credential_digest = $1 OR next_credential_digest = $1) AND removed_at IS NULL",
        world.t("devices")
    ))
    .bind(&digest)
    .fetch_optional(&state.pool)
    .await?;
    let Some((id, current)) = found else {
        return Ok(None);
    };
    if !current {
        // The app already uses the credential a rotation gave it: that confirms it, and the old
        // one stops working.
        sqlx::query(sql!(
            "UPDATE {} SET credential_digest = next_credential_digest, next_credential_digest = NULL
             WHERE device_id = $1 AND next_credential_digest = $2",
            world.t("devices")
        ))
        .bind(&id)
        .bind(&digest)
        .execute(&state.pool)
        .await?;
        tracing::info!(
            device_id = id,
            "a device connected with its rotated credential; the old one no longer works"
        );
    }
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths() {
        assert!(is_api_path("/api/v1/device"));
        assert!(is_api_path("/api/v12/devices"));
        assert!(!is_api_path("/api/version"));
        assert!(!is_api_path("/api/v/x"));
        assert!(!is_api_path("/webhooks/accounts"));
    }

    #[test]
    fn bearer_tokens_are_read_from_either_case() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert("authorization", "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert("authorization", "bearer  def ".parse().unwrap());
        assert_eq!(bearer(&h), Some("def"));
        h.insert("authorization", "Proof sap_x".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }
}
