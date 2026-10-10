//! Proofs Extend holds to act at other apps (Silicon Accounts User verification and App
//! verification), and the key that seals them at rest.
//!
//! - A User verification proof speaks for one account at one app with fixed scopes. Extend gets one
//!   while the account is using Extend (the subject token is that account's current access token
//!   at Extend; a Silicon's instruction, a Carbon's action, is the agreement), keeps the proof
//!   token in memory and the rotating `proof_refresh_token` sealed in `proof_grants`, and refreshes
//!   it single-flight (one lock per account, app and scopes) with an `Idempotency-Key` derived
//!   from the refresh token. Work that happens later (a file's self-destruct) uses the stored one.
//! - App verification proofs (Extend as itself, no account) are kept in memory only.
//! - When an account signs out of Extend, removes its access or is deleted, every proof held for
//!   it is revoked and forgotten ([`ProofStore::drop_account`]).

use std::collections::HashMap;
use std::sync::Arc;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use extend_protocol::ErrorCode;
use rand::Rng as _;
use silicon_accounts_client::{IssueAppVerification, IssueUserVerification, IssuedProof};
use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::accounts::Principal;
use crate::accounts::api::AccountsApi;
use crate::db::World;
use crate::error::{AppError, AppResult};

pub const BRIEFCASE: &str = "briefcase";
/// What Extend does in a Silicon's Briefcase for the files its commands make: store them, share
/// them with the Carbon who paired the device, read them back, and trash them at self-destruct.
/// The ids are Briefcase's own scope catalogue (the old endpoint ids).
pub const BRIEFCASE_WRITE_SCOPES: &[&str] = &[
    "briefcase.uploads.reserve",
    "briefcase.uploads.commit",
    "briefcase.uploads.status",
    "briefcase.uploads.cancel",
    "briefcase.files.read",
    "briefcase.invitations.create",
    "briefcase.entries.trash",
];
/// Reading a file a Silicon made, as the reader (the Silicon, the Carbon it was shared with, or
/// the Silicon's custodian).
pub const BRIEFCASE_READ_SCOPES: &[&str] = &["briefcase.files.read"];
pub const TING: &str = "ting";
pub const TING_SUBSCRIBE_SCOPES: &[&str] = &["tings.subscribe"];
pub const TING_SEND_SCOPES: &[&str] = &["tings.send"];

/// Refresh a proof token this long before it expires.
const REFRESH_MARGIN: time::Duration = time::Duration::seconds(60);

/// `EXTEND_DELEGATION_ENCRYPTION_KEY`: 32 bytes, unpadded base64url. Seals proof refresh tokens
/// (AES-256-GCM, with the account, app and scopes as associated data).
#[derive(Clone)]
pub struct GrantKey([u8; 32]);

impl std::fmt::Debug for GrantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantKey(<redacted>)")
    }
}

impl GrantKey {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value.trim())
            .ok()
            .and_then(|v| v.try_into().ok());
        bytes.map(Self).ok_or_else(|| {
            anyhow::anyhow!("EXTEND_DELEGATION_ENCRYPTION_KEY must be 32 bytes encoded as unpadded base64url")
        })
    }

    pub fn seal(&self, aad: &str, plain: &[u8]) -> AppResult<Vec<u8>> {
        let nonce: [u8; 12] = rand::rng().random();
        let cipher = Aes256Gcm::new_from_slice(&self.0).map_err(|_| crypto_error())?;
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| crypto_error())?;
        let mut out = nonce.to_vec();
        out.extend(encrypted);
        Ok(out)
    }

    pub fn open(&self, aad: &str, sealed: &[u8]) -> AppResult<Vec<u8>> {
        if sealed.len() < 28 {
            return Err(crypto_error());
        }
        Aes256Gcm::new_from_slice(&self.0)
            .map_err(|_| crypto_error())?
            .decrypt(
                Nonce::from_slice(&sealed[..12]),
                Payload {
                    msg: &sealed[12..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| crypto_error())
    }
}

fn crypto_error() -> AppError {
    AppError::new(
        ErrorCode::ServiceUnavailable,
        "Extend could not unlock a proof it saved. Check EXTEND_DELEGATION_ENCRYPTION_KEY on the server.",
    )
}

#[derive(Clone)]
struct Held {
    proof_id: String,
    token: String,
    expires_at: OffsetDateTime,
    refresh: Option<String>,
}

impl Held {
    fn usable(&self) -> bool {
        self.expires_at - REFRESH_MARGIN > OffsetDateTime::now_utc()
    }
}

type Key = (String, String, String);

pub struct ProofStore {
    pool: sqlx::PgPool,
    key: Option<GrantKey>,
    api: Arc<dyn AccountsApi>,
    table: String,
    held: Mutex<HashMap<Key, Held>>,
    flights: std::sync::Mutex<HashMap<Key, Arc<Mutex<()>>>>,
}

fn scopes_key(scopes: &[&str]) -> String {
    let mut s: Vec<&str> = scopes.to_vec();
    s.sort_unstable();
    s.dedup();
    s.join(" ")
}

fn aad(k: &Key) -> String {
    format!("extend-proof:{}:{}:{}", k.0, k.1, k.2)
}

fn held_of(p: &IssuedProof) -> Held {
    Held {
        proof_id: p.proof_id.clone(),
        token: p.proof_token.expose().to_owned(),
        expires_at: p
            .expires_at
            .unwrap_or_else(|| OffsetDateTime::now_utc() + time::Duration::minutes(5)),
        refresh: p.proof_refresh_token.as_ref().map(|r| r.expose().to_owned()),
    }
}

impl ProofStore {
    pub fn new(pool: sqlx::PgPool, key: Option<GrantKey>, api: Arc<dyn AccountsApi>) -> Self {
        Self {
            pool,
            key,
            api,
            table: World::production().t("proof_grants"),
            held: Mutex::default(),
            flights: std::sync::Mutex::default(),
        }
    }

    fn flight(&self, k: &Key) -> Arc<Mutex<()>> {
        let mut f = self.flights.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.retain(|key, lock| key == k || Arc::strong_count(lock) > 1);
        f.entry(k.clone()).or_default().clone()
    }

    async fn save(&self, k: &Key, h: &Held, refresh_expires: Option<OffsetDateTime>) -> AppResult<()> {
        self.held.lock().await.insert(k.clone(), h.clone());
        let (Some(key), Some(refresh)) = (&self.key, &h.refresh) else {
            return Ok(());
        };
        let refresh_cipher = key.seal(&aad(k), refresh.as_bytes())?;
        let token_cipher = key.seal(&aad(k), h.token.as_bytes())?;
        sqlx::query(sql!(
            "INSERT INTO {} (account_uuid, receiving_app, scopes, proof_id, token_cipher, token_expires_at, refresh_cipher, refresh_expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (account_uuid, receiving_app, scopes) DO UPDATE SET proof_id = EXCLUDED.proof_id,
                 token_cipher = EXCLUDED.token_cipher, token_expires_at = EXCLUDED.token_expires_at,
                 refresh_cipher = EXCLUDED.refresh_cipher, refresh_expires_at = EXCLUDED.refresh_expires_at, updated_at = now()",
            self.table
        ))
        .bind(&k.0)
        .bind(&k.1)
        .bind(&k.2)
        .bind(&h.proof_id)
        .bind(token_cipher)
        .bind(h.expires_at)
        .bind(refresh_cipher)
        .bind(refresh_expires)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The held proof for `k`: from memory, else unsealed from the database.
    async fn load(&self, k: &Key) -> AppResult<Option<Held>> {
        if let Some(h) = self.held.lock().await.get(k) {
            return Ok(Some(h.clone()));
        }
        let Some(key) = &self.key else { return Ok(None) };
        let row: Option<(String, Vec<u8>, OffsetDateTime, Option<Vec<u8>>)> = sqlx::query_as(sql!(
            "SELECT proof_id, token_cipher, token_expires_at, refresh_cipher FROM {}
             WHERE account_uuid = $1 AND receiving_app = $2 AND scopes = $3",
            self.table
        ))
        .bind(&k.0)
        .bind(&k.1)
        .bind(&k.2)
        .fetch_optional(&self.pool)
        .await?;
        let Some((proof_id, token, expires_at, refresh)) = row else {
            return Ok(None);
        };
        let text = |b: Vec<u8>| String::from_utf8(b).map_err(|_| crypto_error());
        let h = Held {
            proof_id,
            token: text(key.open(&aad(k), &token)?)?,
            expires_at,
            refresh: refresh.map(|r| key.open(&aad(k), &r).and_then(text)).transpose()?,
        };
        self.held.lock().await.insert(k.clone(), h.clone());
        Ok(Some(h))
    }

    async fn forget(&self, k: &Key) {
        self.held.lock().await.remove(k);
        let _ = sqlx::query(sql!(
            "DELETE FROM {} WHERE account_uuid = $1 AND receiving_app = $2 AND scopes = $3",
            self.table
        ))
        .bind(&k.0)
        .bind(&k.1)
        .bind(&k.2)
        .execute(&self.pool)
        .await;
    }

    /// Refreshes a held proof (the caller holds the key's flight lock).
    async fn refresh(&self, k: &Key, h: &Held) -> AppResult<Option<Held>> {
        let Some(refresh) = &h.refresh else { return Ok(None) };
        match self.api.refresh_proof(refresh).await {
            Ok(p) => {
                let fresh = held_of(&p);
                self.save(k, &fresh, p.refresh_expires_at).await?;
                Ok(Some(fresh))
            }
            Err(e) if e.code() == ErrorCode::AccessRemoved => {
                tracing::info!(
                    account = k.0,
                    app = k.1,
                    "a held proof ended in Silicon Accounts; forgetting it"
                );
                self.forget(k).await;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// A User verification proof token for `p` at `app` with `scopes`: the held one, refreshed, or
    /// a new one issued with `p`'s current access token.
    pub async fn for_user(&self, p: &Principal, app: &str, scopes: &[&str]) -> AppResult<String> {
        let k: Key = (p.uuid.clone(), app.to_owned(), scopes_key(scopes));
        let flight = self.flight(&k);
        let _turn = flight.lock().await;
        if let Some(h) = self.load(&k).await? {
            if h.usable() {
                return Ok(h.token);
            }
            if let Some(fresh) = self.refresh(&k, &h).await? {
                return Ok(fresh.token);
            }
        }
        let request = IssueUserVerification {
            subject_token: p.token.clone(),
            receiving_app: app.to_owned(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            access_ttl_seconds: None,
        };
        let issued = self
            .api
            .issue_user_verification(&request, &format!("extend-uv-{}", uuid::Uuid::now_v7()))
            .await?;
        let h = held_of(&issued);
        self.save(&k, &h, issued.refresh_expires_at).await?;
        Ok(h.token)
    }

    /// For work that happens later, with no live sign-in at hand: the proof held for `account`,
    /// refreshed as needed. `None` when Extend holds none (it gets one at the account's next use).
    pub async fn held(&self, account: &str, app: &str, scopes: &[&str]) -> AppResult<Option<String>> {
        let k: Key = (account.to_owned(), app.to_owned(), scopes_key(scopes));
        let flight = self.flight(&k);
        let _turn = flight.lock().await;
        let Some(h) = self.load(&k).await? else { return Ok(None) };
        if h.usable() {
            return Ok(Some(h.token));
        }
        Ok(self.refresh(&k, &h).await?.map(|h| h.token))
    }

    /// An App verification proof token for `app` (Extend as itself), kept in memory.
    pub async fn for_app(&self, app: &str, scopes: &[&str]) -> AppResult<String> {
        let k: Key = (String::new(), app.to_owned(), scopes_key(scopes));
        let flight = self.flight(&k);
        let _turn = flight.lock().await;
        if let Some(h) = self.held.lock().await.get(&k).filter(|h| h.usable()) {
            return Ok(h.token.clone());
        }
        let request = IssueAppVerification {
            receiving_app: app.to_owned(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            access_ttl_seconds: None,
        };
        let issued = self
            .api
            .issue_app_verification(&request, &format!("extend-av-{}", uuid::Uuid::now_v7()))
            .await?;
        let h = held_of(&issued);
        self.held.lock().await.insert(k, h.clone());
        Ok(h.token)
    }

    /// Revokes and forgets every proof Extend holds for `account` (its sign-in at Extend ended).
    pub async fn drop_account(&self, account: &str) {
        let mut ids: Vec<String> =
            sqlx::query_scalar(sql!("SELECT proof_id FROM {} WHERE account_uuid = $1", self.table))
                .bind(account)
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        {
            let mut held = self.held.lock().await;
            ids.extend(
                held.iter()
                    .filter(|(k, _)| k.0 == account)
                    .map(|(_, h)| h.proof_id.clone()),
            );
            held.retain(|k, _| k.0 != account);
        }
        ids.sort();
        ids.dedup();
        for id in &ids {
            if let Err(e) = self.api.revoke_proof(id).await {
                tracing::warn!(account, proof_id = id, error = %e, "revoking a proof failed; it ends with the account's sign-in anyway");
            }
        }
        let _ = sqlx::query(sql!("DELETE FROM {} WHERE account_uuid = $1", self.table))
            .bind(account)
            .execute(&self.pool)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_key_seals_with_associated_data() {
        let key = GrantKey::parse("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let sealed = key.seal("extend-proof:zQo:briefcase:x", b"sapr_secret").unwrap();
        assert_eq!(
            key.open("extend-proof:zQo:briefcase:x", &sealed).unwrap(),
            b"sapr_secret"
        );
        assert!(key.open("extend-proof:OTHER:briefcase:x", &sealed).is_err());
        assert!(GrantKey::parse("short").is_err());
    }

    #[test]
    fn scopes_are_keyed_in_one_order() {
        assert_eq!(scopes_key(&["b", "a", "b"]), "a b");
        assert_eq!(scopes_key(BRIEFCASE_READ_SCOPES), "briefcase.files.read");
    }
}
