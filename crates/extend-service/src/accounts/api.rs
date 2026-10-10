//! What Extend asks Silicon Accounts, behind one trait: the official `silicon-accounts-client`
//! against a real Accounts ([`SdkApi`]), or the in-process stand-in for development and tests
//! ([`super::local::LocalAccounts`], refused in production by `Config`).

use std::time::Duration;

use async_trait::async_trait;
use extend_protocol::ErrorCode;
use silicon_accounts_client::{
    AccountSummary, AccountsClient, Introspection, IssueAppVerification, IssueUserVerification, IssuedProof, Jwks,
    ProofRef,
};

use crate::error::{AppError, AppResult};

#[async_trait]
pub trait AccountsApi: Send + Sync {
    /// The signing keys (`/.well-known/jwks.json`).
    async fn jwks(&self) -> AppResult<Jwks>;
    /// `POST /v1/oauth/introspect` with Extend's app credentials.
    async fn introspect(&self, token: &str) -> AppResult<Introspection>;
    /// `GET /v1/accounts/{uuid}`; `None` when Accounts knows no such account.
    async fn lookup(&self, uuid: &str) -> AppResult<Option<AccountSummary>>;
    /// `GET /v1/accounts/by-id/{id}` (current ids only); `None` when no account has that id.
    async fn lookup_by_id(&self, id: &str) -> AppResult<Option<AccountSummary>>;
    /// `POST /v1/oauth/revoke` with Extend's app credentials (signs that sign-in out of Extend).
    async fn revoke_token(&self, token: &str) -> AppResult<()>;
    /// `POST /v1/proofs/user-verification`.
    async fn issue_user_verification(
        &self,
        req: &IssueUserVerification,
        idempotency_key: &str,
    ) -> AppResult<IssuedProof>;
    /// `POST /v1/proofs/app-verification`.
    async fn issue_app_verification(&self, req: &IssueAppVerification, idempotency_key: &str)
    -> AppResult<IssuedProof>;
    /// `POST /v1/proofs/refresh` (the refresh token rotates; reusing an old one revokes the proof).
    async fn refresh_proof(&self, proof_refresh_token: &str) -> AppResult<IssuedProof>;
    /// `POST /v1/proofs/revoke` by proof id.
    async fn revoke_proof(&self, proof_id: &str) -> AppResult<()>;
}

/// The official client against a real Silicon Accounts, with Extend's app credentials.
pub struct SdkApi {
    client: AccountsClient,
    /// For the one call the client crate can't make safely: a proof refresh with an
    /// `Idempotency-Key` derived from the refresh token (so a lost answer can be retried without
    /// tripping reuse detection, which would revoke the proof).
    http: reqwest::Client,
    api_url: String,
    app_id: String,
    app_secret: String,
}

impl SdkApi {
    /// `api_url` is where Extend reaches Accounts server to server (`ACCOUNTS_API_URL`, else
    /// `ACCOUNTS_URL`). Plain http is accepted only for this machine (the local stack).
    pub fn new(api_url: &str, app_id: &str, app_secret: &str) -> anyhow::Result<Self> {
        let client = AccountsClient::builder()
            .base_url(api_url)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("silicon-extend/", env!("CARGO_PKG_VERSION")))
            .telemetry(false)
            .build()
            .map_err(|e| anyhow::anyhow!("ACCOUNTS_API_URL / ACCOUNTS_URL {api_url:?}: {}", e.message()))?;
        Ok(Self {
            client,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent(concat!("silicon-extend/", env!("CARGO_PKG_VERSION")))
                .build()?,
            api_url: api_url.trim().trim_end_matches('/').to_owned(),
            app_id: app_id.to_owned(),
            app_secret: app_secret.to_owned(),
        })
    }

    fn app(&self) -> silicon_accounts_client::AppClient<'_> {
        self.client.as_app(&self.app_id, &self.app_secret)
    }
}

/// Turns a Silicon Accounts client error into Extend's: what happened, why, and what to do.
pub fn accounts_error(doing: &str, e: &silicon_accounts_client::Error) -> AppError {
    let detail = match e.hint() {
        Some(h) => format!("{} ({h})", e.message()),
        None => e.message(),
    };
    match (e.status(), e.code()) {
        (Some(401), _) | (_, "invalid_client") => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("Silicon Accounts refused Extend's app credentials while {doing}: {detail}"),
        )
        .hint("This is a configuration problem on the Extend server (EXTEND_APP_ID / EXTEND_APP_SECRET); report it with `extend report`."),
        (Some(429), _) => AppError::new(
            ErrorCode::RateLimited,
            format!("Silicon Accounts is rate limiting Extend while {doing}."),
        )
        .hint("Retry in a minute."),
        (Some(s), _) if s >= 500 => AppError::unavailable("Silicon Accounts", format!("{doing}: {detail}")),
        (None, _) => AppError::unavailable("Silicon Accounts", format!("{doing}: {detail}")),
        _ => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("Silicon Accounts refused {doing}: {detail}"),
        )
        .hint("Retry in a moment. If it keeps failing, report it with `extend report`."),
    }
}

/// The `Idempotency-Key` of a proof refresh: derived from the refresh token, so a retry after a
/// lost answer replays the same new tokens instead of presenting a used refresh token.
pub fn refresh_idempotency_key(proof_refresh_token: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(proof_refresh_token.trim().as_bytes());
    format!("refresh-{}", &extend_protocol::ids::hex_lower(&digest)[..32])
}

fn not_found(e: &silicon_accounts_client::Error) -> bool {
    e.status() == Some(404) || matches!(e.code(), "not_found" | "account_not_found")
}

#[async_trait]
impl AccountsApi for SdkApi {
    async fn jwks(&self) -> AppResult<Jwks> {
        self.client
            .jwks()
            .await
            .map_err(|e| accounts_error("fetching its signing keys", &e))
    }

    async fn introspect(&self, token: &str) -> AppResult<Introspection> {
        self.app()
            .introspect(token)
            .await
            .map_err(|e| accounts_error("checking that a sign-in is still active", &e))
    }

    async fn lookup(&self, uuid: &str) -> AppResult<Option<AccountSummary>> {
        match self.app().lookup(uuid).await {
            Ok(a) => Ok(Some(a)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(accounts_error("looking up an account", &e)),
        }
    }

    async fn lookup_by_id(&self, id: &str) -> AppResult<Option<AccountSummary>> {
        match self.app().lookup_by_id(id).await {
            Ok(a) => Ok(Some(a)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(accounts_error(&format!("looking up {id}"), &e)),
        }
    }

    async fn revoke_token(&self, token: &str) -> AppResult<()> {
        self.app()
            .revoke(token)
            .await
            .map_err(|e| accounts_error("revoking a sign-in", &e))
    }

    async fn issue_user_verification(
        &self,
        req: &IssueUserVerification,
        idempotency_key: &str,
    ) -> AppResult<IssuedProof> {
        self.app()
            .issue_user_verification(req, Some(idempotency_key))
            .await
            .map_err(|e| {
                accounts_error(
                    &format!("issuing a User verification proof for {}", req.receiving_app),
                    &e,
                )
            })
    }

    async fn issue_app_verification(
        &self,
        req: &IssueAppVerification,
        idempotency_key: &str,
    ) -> AppResult<IssuedProof> {
        self.app()
            .issue_app_verification(req, Some(idempotency_key))
            .await
            .map_err(|e| {
                accounts_error(
                    &format!("issuing an App verification proof for {}", req.receiving_app),
                    &e,
                )
            })
    }

    async fn refresh_proof(&self, proof_refresh_token: &str) -> AppResult<IssuedProof> {
        let key = refresh_idempotency_key(proof_refresh_token);
        let resp = self
            .http
            .post(format!("{}/v1/proofs/refresh", self.api_url))
            .basic_auth(&self.app_id, Some(&self.app_secret))
            .header("Idempotency-Key", key)
            .json(&serde_json::json!({ "proof_refresh_token": proof_refresh_token.trim() }))
            .send()
            .await
            .map_err(|e| AppError::unavailable("Silicon Accounts", format!("refreshing a proof: {e}")))?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if (200..300).contains(&status) {
            return serde_json::from_value(body).map_err(|e| {
                AppError::unavailable("Silicon Accounts", format!("an unexpected proof refresh answer: {e}"))
            });
        }
        let code = body
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or("no code");
        let message = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("no details");
        Err(match status {
            400 | 410 => AppError::new(
                ErrorCode::AccessRemoved,
                format!("Silicon Accounts ended the proof Extend held ({status} {code}: {message})."),
            )
            .hint("A new proof is issued at the account's next use of Extend.")
            .details(serde_json::json!({"accounts_status": status, "accounts_code": code})),
            401 => AppError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "Silicon Accounts refused Extend's app credentials while refreshing a proof ({code}: {message})."
                ),
            )
            .hint("This is a configuration problem on the Extend server; report it with `extend report`."),
            _ => AppError::unavailable(
                "Silicon Accounts",
                format!("refreshing a proof answered {status} {code}: {message}"),
            ),
        })
    }

    async fn revoke_proof(&self, proof_id: &str) -> AppResult<()> {
        match self.app().revoke_proof(&ProofRef::Id(proof_id.to_owned())).await {
            Ok(()) => Ok(()),
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(accounts_error("revoking a proof", &e)),
        }
    }
}
