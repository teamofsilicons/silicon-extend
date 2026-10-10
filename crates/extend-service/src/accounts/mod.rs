//! Silicon Accounts: who is calling, and what Extend knows about every account it has seen.
//!
//! - Every account route takes `Authorization: Bearer <access token>`: an EdDSA JWT Silicon
//!   Accounts issued to Extend (`aud` = Extend's app id, `iss` = `ACCOUNTS_URL`). It is verified
//!   locally against the cached JWKS, which is fetched again (at most every 10 s) when a token names
//!   a key Extend doesn't have yet ([`Accounts::authenticate`]).
//! - Revocation: a token issued before the account's `revoked_before` (set by the
//!   `membership.signed_out`, `membership.access_removed` and `account.deleted` webhooks) is
//!   refused at once; the sensitive routes also ask Silicon Accounts live, with the answer cached
//!   for at most 30 s and dropped by any webhook about the account ([`Accounts::require_live`]).
//! - Extend keys everything on the account `uuid` and shows the current public id. The `accounts`
//!   table caches each account's id, name, photo, custodian and status, from token claims,
//!   lookups (at most 600 a minute, the Accounts limit) and webhooks ([`directory`]).

pub mod api;
pub mod directory;
pub mod local;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use extend_protocol::ErrorCode;
use extend_protocol::model::{Member, MemberKind};
use silicon_accounts_client::{Claims, Jwks, TokenError, VerifyOptions};
use tokio::sync::{Mutex, RwLock};

pub use directory::AccountRow;

use crate::error::{AppError, AppResult};
use api::AccountsApi;

/// How long Silicon Accounts' live answer about a token is reused.
pub const INTROSPECTION_TTL: Duration = Duration::from_secs(30);
/// The fewest seconds between two JWKS fetches caused by an unknown `kid`.
pub const JWKS_REFETCH_EVERY: Duration = Duration::from_secs(10);
/// A fetched JWKS is refreshed in the background after this long.
pub const JWKS_MAX_AGE: Duration = Duration::from_secs(3600);

/// A signed-in Carbon or Silicon, from a verified access token. (`Debug` leaves the token out.)
#[derive(Clone)]
pub struct Principal {
    /// The permanent Silicon Accounts uuid: what Extend stores.
    pub uuid: String,
    pub kind: MemberKind,
    /// The current public id (`c:ada`, `si:scout`): what Extend shows.
    pub id: String,
    pub display_name: Option<String>,
    /// The access token itself: the subject token of User verification proofs, and what
    /// introspection checks.
    pub token: String,
    /// `iat` of the token (unix seconds).
    pub issued_at: Option<i64>,
    /// The sign-in family (`fid`): one sign-in of the account at Extend (a machine's CLI, the
    /// website), across its refreshes.
    pub family: Option<String>,
    /// The scopes the sign-in was granted (`scope`, space-separated), as Silicon Accounts issued them.
    pub scope: Option<String>,
}

impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Principal")
            .field("uuid", &self.uuid)
            .field("kind", &self.kind)
            .field("id", &self.id)
            .field("display_name", &self.display_name)
            .field("token", &"<redacted>")
            .field("issued_at", &self.issued_at)
            .field("family", &self.family)
            .field("scope", &self.scope)
            .finish()
    }
}

impl Principal {
    pub fn uuid(&self) -> &str {
        &self.uuid
    }
    pub fn public_id(&self) -> &str {
        &self.id
    }
    pub fn is_silicon(&self) -> bool {
        self.kind == MemberKind::Silicon
    }
    pub fn is_carbon(&self) -> bool {
        self.kind == MemberKind::Carbon
    }
    /// The actor Extend records (activity rows): kind and uuid.
    pub fn actor(&self) -> Member {
        actor(self.kind, &self.uuid)
    }
}

/// An actor for the activity log, by uuid (rows store uuids; views show current ids).
pub fn actor(kind: MemberKind, uuid: &str) -> Member {
    Member::new(kind, uuid)
}

#[derive(Default)]
struct JwksCache {
    keys: Option<Jwks>,
    fetched_at: Option<Instant>,
    last_refetch: Option<Instant>,
}

pub struct Accounts {
    pub app_id: String,
    /// `ACCOUNTS_URL`: the `iss` every accepted token carries.
    pub issuer: String,
    api: Arc<dyn AccountsApi>,
    /// The stand-in, in local mode (tests mint tokens with it).
    pub local: Option<Arc<local::LocalAccounts>>,
    jwks: Mutex<JwksCache>,
    /// token digest → (checked at, active, account uuid).
    introspections: RwLock<HashMap<String, (Instant, bool, String)>>,
    pub directory: directory::Directory,
}

impl Accounts {
    pub fn new(
        app_id: &str,
        issuer: &str,
        api: Arc<dyn AccountsApi>,
        local: Option<Arc<local::LocalAccounts>>,
        pool: sqlx::PgPool,
    ) -> Arc<Self> {
        Arc::new(Self {
            app_id: app_id.to_owned(),
            issuer: issuer.trim_end_matches('/').to_owned(),
            directory: directory::Directory::new(pool, api.clone()),
            api,
            local,
            jwks: Mutex::default(),
            introspections: RwLock::default(),
        })
    }

    pub fn api(&self) -> &Arc<dyn AccountsApi> {
        &self.api
    }

    /// Fetches the signing keys once at start, so the first request doesn't wait for them. A
    /// failure is logged; the first request fetches again.
    pub async fn prefetch(&self) {
        if let Err(e) = self.keys(None).await {
            tracing::warn!(error = %e, "could not fetch Silicon Accounts' signing keys at start; retrying on the first request");
        }
    }

    /// The cached JWKS; fetched when missing, older than [`JWKS_MAX_AGE`], or (at most every
    /// [`JWKS_REFETCH_EVERY`]) when `want_kid` is not in it.
    async fn keys(&self, want_kid: Option<&str>) -> AppResult<Jwks> {
        let mut cache = self.jwks.lock().await;
        let fresh = cache.fetched_at.is_some_and(|t| t.elapsed() < JWKS_MAX_AGE);
        let missing_kid = match (&cache.keys, want_kid) {
            (Some(k), Some(kid)) => k.find(kid).is_none(),
            _ => false,
        };
        let may_refetch = cache.last_refetch.is_none_or(|t| t.elapsed() >= JWKS_REFETCH_EVERY);
        if cache.keys.is_none() || !fresh || (missing_kid && may_refetch) {
            if missing_kid {
                cache.last_refetch = Some(Instant::now());
            }
            match self.api.jwks().await {
                Ok(keys) => {
                    cache.keys = Some(keys);
                    cache.fetched_at = Some(Instant::now());
                }
                Err(e) if cache.keys.is_none() => return Err(e),
                Err(e) => {
                    tracing::warn!(error = %e, "refreshing Silicon Accounts' signing keys failed; using the cached ones")
                }
            }
        }
        cache
            .keys
            .clone()
            .ok_or_else(|| AppError::unavailable("Silicon Accounts", "no signing keys"))
    }

    /// Verifies an access token and returns who it belongs to.
    pub async fn authenticate(&self, token: &str) -> AppResult<Principal> {
        let token = token.trim();
        let kid = jsonwebtoken::decode_header(token).ok().and_then(|h| h.kid);
        let options = VerifyOptions::for_app(&self.app_id).with_issuer(self.issuer.clone());
        let mut keys = self.keys(kid.as_deref()).await?;
        let mut verified = silicon_accounts_client::verify_access_token(&keys, token, &options);
        if let Err(silicon_accounts_client::Error::Token(TokenError::UnknownKey { kid: Some(k) })) = &verified {
            // Keys rotate: fetch again (rate limited) and retry once.
            keys = self.keys(Some(k)).await?;
            verified = silicon_accounts_client::verify_access_token(&keys, token, &options);
        }
        let claims = verified.map_err(|e| token_refused(&e, &self.app_id, &self.issuer))?;
        let principal = principal_of(&claims, token)?;
        let row = self.directory.on_sign_in(&principal).await?;
        if let Some(before) = row.revoked_before
            && principal.issued_at.is_none_or(|iat| iat < before.unix_timestamp())
        {
            return Err(AppError::new(
                ErrorCode::TokenExpired,
                format!(
                    "This sign-in of {} ended at {}: the account signed out of Extend in Silicon Accounts, removed Extend's access, or was deleted.",
                    principal.id,
                    before.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
                ),
            )
            .hint("Sign in to Extend again: `extend login` (Carbons), `silicon-accounts login --app extend -q | extend login --slt-stdin` (Silicons), or on the website."));
        }
        if row.status == "deleted" {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                format!("The account {} was deleted in Silicon Accounts.", principal.id),
            ));
        }
        Ok(Principal {
            id: if row.id.is_empty() { principal.id } else { row.id },
            display_name: row.display_name.or(principal.display_name),
            ..principal
        })
    }

    /// Asks Silicon Accounts whether the sign-in behind `p` is still active (cached for at most
    /// [`INTROSPECTION_TTL`]). Used on the routes where a sign-out must take effect at once.
    pub async fn require_live(&self, p: &Principal) -> AppResult<()> {
        let digest = extend_protocol::ids::secret_digest(&p.token);
        if let Some((at, active, _)) = self.introspections.read().await.get(&digest)
            && at.elapsed() < INTROSPECTION_TTL
        {
            return if *active { Ok(()) } else { Err(inactive(p)) };
        }
        let answer = self.api.introspect(&p.token).await.map_err(|e| {
            AppError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "Extend couldn't confirm with Silicon Accounts that {}'s sign-in is still active ({}), so it didn't do this.",
                    p.id, e.0.message
                ),
            )
            .hint("Retry in a moment.")
        })?;
        let active = answer.active && answer.sub.as_deref() == Some(p.uuid.as_str());
        let mut cache = self.introspections.write().await;
        if cache.len() > 50_000 {
            cache.retain(|_, (at, _, _)| at.elapsed() < INTROSPECTION_TTL);
        }
        cache.insert(digest, (Instant::now(), active, p.uuid.clone()));
        if active { Ok(()) } else { Err(inactive(p)) }
    }

    /// Drops every cached live answer for an account (a webhook named it).
    pub async fn forget(&self, uuid: &str) {
        self.introspections.write().await.retain(|_, (_, _, u)| u != uuid);
        self.directory.forget(uuid).await;
    }
}

fn inactive(p: &Principal) -> AppError {
    AppError::new(
        ErrorCode::TokenExpired,
        format!("Silicon Accounts says this sign-in of {} is no longer active.", p.id),
    )
    .hint("Sign in to Extend again: `extend login` (Carbons), `silicon-accounts login --app extend -q | extend login --slt-stdin` (Silicons), or on the website.")
}

/// Why a token was refused, as Extend says it.
fn token_refused(e: &silicon_accounts_client::Error, app_id: &str, issuer: &str) -> AppError {
    let sign_in = "Sign in to Extend again: `extend login` (Carbons), `silicon-accounts login --app extend -q | extend login --slt-stdin` (Silicons), or on the website.";
    match e {
        silicon_accounts_client::Error::Token(TokenError::Expired { .. }) => AppError::new(
            ErrorCode::TokenExpired,
            "The access token expired (Silicon Accounts access tokens live 30 minutes).",
        )
        .hint("Refresh it with the refresh token and retry; the extend CLI and website do this by themselves."),
        silicon_accounts_client::Error::Token(TokenError::WrongAudience { .. }) => AppError::new(
            ErrorCode::Unauthorized,
            format!("The access token was issued to another app, not to {app_id}."),
        )
        .hint(sign_in),
        silicon_accounts_client::Error::Token(TokenError::WrongIssuer { .. }) => AppError::new(
            ErrorCode::Unauthorized,
            format!("The access token was not issued by the Silicon Accounts this Extend trusts ({issuer})."),
        )
        .hint(sign_in),
        silicon_accounts_client::Error::Token(t) => AppError::new(
            ErrorCode::Unauthorized,
            format!("The access token was refused: {}", t.message()),
        )
        .hint(sign_in),
        other => AppError::new(
            ErrorCode::Unauthorized,
            format!("The access token was refused: {}", other.message()),
        )
        .hint(sign_in),
    }
}

fn principal_of(claims: &Claims, token: &str) -> AppResult<Principal> {
    let kind = match claims.kind {
        Some(silicon_accounts_client::AccountKind::Carbon) => MemberKind::Carbon,
        Some(silicon_accounts_client::AccountKind::Silicon) => MemberKind::Silicon,
        None => {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "The access token has no `kind` claim, so Extend can't tell a Carbon from a Silicon.",
            ));
        }
    };
    if claims.sub.trim().is_empty() || claims.sub.len() > 64 {
        return Err(AppError::new(
            ErrorCode::Unauthorized,
            "The access token has no usable `sub` claim.",
        ));
    }
    Ok(Principal {
        uuid: claims.sub.clone(),
        kind,
        id: claims.id.clone().unwrap_or_default(),
        display_name: None,
        token: token.to_owned(),
        issued_at: claims.iat,
        family: claims.fid.clone(),
        scope: claims.scope.clone().filter(|s| !s.trim().is_empty()),
    })
}
