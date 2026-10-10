//! A Silicon Accounts stand-in for development and tests (`EXTEND_ACCOUNTS_MODE=local`, refused
//! in production). It signs real EdDSA access tokens with its own key, so Extend verifies them
//! exactly as it verifies Silicon Accounts' (JWKS, `iss`, `aud`, `exp`, `nbf`), and it answers
//! introspection, lookups and proofs from memory. There are no Teams: an account is a Carbon or a
//! Silicon, and a Silicon has one custodian.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey as _;
use extend_protocol::ErrorCode;
use extend_protocol::model::MemberKind;
use rand::Rng as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use silicon_accounts_client::{
    AccountSummary, Introspection, IssueAppVerification, IssueUserVerification, IssuedProof, Jwk, Jwks,
};

use super::api::AccountsApi;
use crate::error::{AppError, AppResult};

pub const LOCAL_KID: &str = "extend-local-1";
/// How long the stand-in's short-lived tokens last, like Silicon Accounts' (120 s, single use).
pub const SLT_TTL_S: i64 = 120;
/// How long the access tokens of the stand-in's sign-ins last, like Silicon Accounts' (30 min).
pub const ACCESS_TTL_S: i64 = 1800;
/// The short-lived token grant of Silicon Accounts' token endpoint.
pub const SLT_GRANT: &str = "urn:silicon:params:oauth:grant-type:slt";
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// One account the stand-in knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAccount {
    pub uuid: String,
    pub kind: MemberKind,
    pub id: String,
    pub display_name: String,
    /// A Silicon's custodian (a Carbon's uuid).
    pub custodian: Option<String>,
    /// `active` or `deleted`.
    pub status: String,
}

#[derive(Debug, Clone)]
struct LocalProof {
    proof_id: String,
    user: Option<String>,
    issuing_app: String,
    receiving_app: String,
    scopes: Vec<String>,
    refresh: String,
    family: Option<String>,
    revoked: bool,
}

#[derive(Default)]
struct Inner {
    accounts: HashMap<String, LocalAccount>,
    /// Sign-in families (`fid`) that were revoked (signed out, access removed, deleted).
    revoked_families: Vec<String>,
    /// fid → account uuid.
    families: HashMap<String, String>,
    proofs: HashMap<String, LocalProof>,
    /// proof token → proof id.
    proof_tokens: HashMap<String, String>,
    /// Every proof request, for tests: `{"kind", "receiving_app", "scopes", "user"}`.
    issued: Vec<Value>,
    /// Short-lived tokens for Extend the stand-in minted (`slt_local_…`).
    slts: HashMap<String, LocalSlt>,
    /// Refresh tokens of the stand-in's sign-ins (`sar_local_…`).
    refresh_tokens: HashMap<String, LocalRefresh>,
}

#[derive(Debug, Clone)]
struct LocalSlt {
    uuid: String,
    expires_at: i64,
    used: bool,
}

#[derive(Debug, Clone)]
struct LocalRefresh {
    uuid: String,
    family: String,
    used: bool,
}

/// A refusal of the stand-in's token endpoint, in the OAuth shape Silicon Accounts answers
/// (`{"error", "error_description"}`), with the HTTP status it would use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRefusal {
    pub status: u16,
    pub error: &'static str,
    pub description: String,
}

impl OAuthRefusal {
    fn grant(description: impl Into<String>) -> Self {
        Self {
            status: 400,
            error: "invalid_grant",
            description: description.into(),
        }
    }
}

pub struct LocalAccounts {
    pub issuer: String,
    pub app_id: String,
    signing: SigningKey,
    inner: Mutex<Inner>,
}

impl LocalAccounts {
    pub fn new(issuer: &str, app_id: &str) -> Self {
        let seed: [u8; 32] = rand::rng().random();
        Self {
            issuer: issuer.trim_end_matches('/').to_owned(),
            app_id: app_id.to_owned(),
            signing: SigningKey::from_bytes(&seed),
            inner: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The uuid the stand-in gives a new account with this id: 8 base62 characters derived from
    /// the id, so tests can name accounts by id and still know their uuid.
    pub fn uuid_for(id: &str) -> String {
        let digest = Sha256::digest(format!("extend-local:{}", id.trim().to_ascii_lowercase()).as_bytes());
        digest[..8]
            .iter()
            .map(|b| BASE62[usize::from(*b) % BASE62.len()] as char)
            .collect()
    }

    /// The key set Extend verifies the stand-in's tokens with.
    pub fn jwks_value(&self) -> Jwks {
        let x = URL_SAFE_NO_PAD.encode(self.signing.verifying_key().as_bytes());
        Jwks::new(vec![Jwk::ed25519(LOCAL_KID, x)])
    }

    /// Creates the account (`c:…` or `si:…`) if it is new, and returns it. A Silicon gets
    /// `custodian` (a Carbon id), created too if needed; without one, `c:custodian-of-<handle>`.
    pub fn ensure(&self, id: &str, custodian: Option<&str>) -> AppResult<LocalAccount> {
        let id = id.trim().to_ascii_lowercase();
        let kind = extend_protocol::ids::member_kind(&id).ok_or_else(|| {
            AppError::invalid(format!("{id:?} is not an account id; ids look like c:ada or si:scout."))
        })?;
        let custodian_uuid = match kind {
            MemberKind::Silicon => {
                let handle = id.trim_start_matches("si:");
                let c = custodian.map_or_else(|| format!("c:custodian-of-{handle}"), str::to_owned);
                Some(self.ensure(&c, None)?.uuid)
            }
            MemberKind::Carbon => None,
        };
        let mut inner = self.lock();
        if let Some(found) = inner.accounts.values().find(|a| a.id == id && a.status != "deleted") {
            let mut found = found.clone();
            if let (Some(c), true) = (custodian_uuid, custodian.is_some()) {
                found.custodian = Some(c);
                inner.accounts.insert(found.uuid.clone(), found.clone());
            }
            return Ok(found);
        }
        let account = LocalAccount {
            uuid: Self::uuid_for(&id),
            kind,
            display_name: display_name_of(&id),
            id,
            custodian: custodian_uuid,
            status: "active".into(),
        };
        inner.accounts.insert(account.uuid.clone(), account.clone());
        Ok(account)
    }

    pub fn account(&self, uuid: &str) -> Option<LocalAccount> {
        self.lock().accounts.get(uuid).cloned()
    }

    /// Changes what the stand-in knows about an account (tests: id changes, custodian changes,
    /// deletion). Extend learns about it through the webhook a test then delivers.
    pub fn update(&self, uuid: &str, change: impl FnOnce(&mut LocalAccount)) {
        if let Some(a) = self.lock().accounts.get_mut(uuid) {
            change(a);
        }
    }

    /// Ends every sign-in of an account (its tokens stop introspecting as active).
    pub fn sign_out(&self, uuid: &str) {
        let mut inner = self.lock();
        let families: Vec<String> = inner
            .families
            .iter()
            .filter(|(_, u)| *u == uuid)
            .map(|(f, _)| f.clone())
            .collect();
        inner.revoked_families.extend(families);
        for p in inner.proofs.values_mut() {
            if p.user.as_deref() == Some(uuid) {
                p.revoked = true;
            }
        }
    }

    /// Signs an access token for `account`, valid for `ttl_s` seconds (negative: already expired).
    pub fn mint(&self, account: &LocalAccount, ttl_s: i64) -> String {
        self.mint_with(account, ttl_s, &self.app_id, &self.issuer, LOCAL_KID)
    }

    /// [`Self::mint`] with any audience, issuer and key id (tests of refused tokens).
    pub fn mint_with(&self, account: &LocalAccount, ttl_s: i64, aud: &str, iss: &str, kid: &str) -> String {
        let family = format!("fam_{}", random_token(16));
        self.lock().families.insert(family.clone(), account.uuid.clone());
        self.mint_in_family(account, ttl_s, aud, iss, kid, &family)
    }

    fn mint_in_family(
        &self,
        account: &LocalAccount,
        ttl_s: i64,
        aud: &str,
        iss: &str,
        kid: &str,
        family: &str,
    ) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let claims = json!({
            "iss": iss, "sub": account.uuid, "aud": aud, "exp": now + ttl_s, "iat": now - 1, "nbf": now - 1,
            "jti": random_token(12), "kind": match account.kind { MemberKind::Carbon => "carbon", MemberKind::Silicon => "silicon" },
            "id": account.id, "mid": format!("{aud}:{}", account.uuid), "fid": family, "scope": "profile",
        });
        self.sign(&claims, kid)
    }

    /// Signs any claims with the stand-in's key (tests of malformed tokens).
    pub fn sign(&self, claims: &Value, kid: &str) -> String {
        let der = self
            .signing
            .to_pkcs8_der()
            .map(|d| d.as_bytes().to_vec())
            .unwrap_or_default();
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
        header.kid = Some(kid.to_owned());
        jsonwebtoken::encode(&header, claims, &jsonwebtoken::EncodingKey::from_ed_der(&der)).unwrap_or_default()
    }

    /// A short-lived token for Extend, as `silicon-accounts login --app extend -q` prints one: it
    /// works once, within [`SLT_TTL_S`] seconds, at the token endpoint ([`Self::exchange_slt`]).
    pub fn mint_slt(&self, account: &LocalAccount) -> String {
        self.mint_slt_with_ttl(account, SLT_TTL_S)
    }

    /// [`Self::mint_slt`] with another lifetime (tests: negative is already expired).
    pub fn mint_slt_with_ttl(&self, account: &LocalAccount, ttl_s: i64) -> String {
        let token = format!("slt_local_{}", random_token(32));
        let expires_at = time::OffsetDateTime::now_utc().unix_timestamp() + ttl_s;
        self.lock().slts.insert(
            token.clone(),
            LocalSlt {
                uuid: account.uuid.clone(),
                expires_at,
                used: false,
            },
        );
        token
    }

    /// The token endpoint's short-lived token grant for Extend's public client (`client_id` alone):
    /// a new sign-in with an access token and a rotating refresh token, answered in Silicon
    /// Accounts' shape. A token works once, whether the exchange succeeds or not.
    pub fn exchange_slt(&self, slt: &str, client_id: &str) -> Result<Value, OAuthRefusal> {
        self.check_client(client_id)?;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let account = {
            let mut inner = self.lock();
            let Some(entry) = inner.slts.get_mut(slt.trim()) else {
                return Err(OAuthRefusal::grant(
                    "The short-lived token is not known: it is mistyped or was never issued.",
                ));
            };
            if entry.used {
                return Err(OAuthRefusal::grant(
                    "The short-lived token was already used; each one works once.",
                ));
            }
            entry.used = true;
            if entry.expires_at < now {
                return Err(OAuthRefusal::grant(format!(
                    "The short-lived token expired at {} (they last {SLT_TTL_S} seconds).",
                    rfc3339(entry.expires_at)
                )));
            }
            let uuid = entry.uuid.clone();
            inner.accounts.get(&uuid).filter(|a| a.status == "active").cloned()
        };
        let account = account.ok_or_else(|| {
            OAuthRefusal::grant("The account the short-lived token was issued to is no longer active.")
        })?;
        let family = format!("fam_{}", random_token(16));
        self.lock().families.insert(family.clone(), account.uuid.clone());
        Ok(self.sign_in_answer(&account, &family))
    }

    /// The token endpoint's refresh grant for Extend's public client. The refresh token rotates on
    /// every use; presenting a used one ends the whole sign-in (`refresh_token_reuse`), as Silicon
    /// Accounts does.
    pub fn refresh_grant(&self, refresh_token: &str, client_id: &str) -> Result<Value, OAuthRefusal> {
        self.check_client(client_id)?;
        let account = {
            let mut inner = self.lock();
            let Some(entry) = inner.refresh_tokens.get(refresh_token.trim()).cloned() else {
                return Err(OAuthRefusal::grant(
                    "The refresh token is not known: it is mistyped or was never issued.",
                ));
            };
            if inner.revoked_families.contains(&entry.family) {
                return Err(OAuthRefusal::grant(
                    "The sign-in this refresh token belongs to was revoked.",
                ));
            }
            if entry.used {
                inner.revoked_families.push(entry.family.clone());
                return Err(OAuthRefusal::grant(
                    "The refresh token was already used, so its sign-in was ended (refresh_token_reuse).",
                ));
            }
            if let Some(e) = inner.refresh_tokens.get_mut(refresh_token.trim()) {
                e.used = true;
            }
            inner
                .accounts
                .get(&entry.uuid)
                .filter(|a| a.status == "active")
                .cloned()
                .map(|a| (a, entry.family))
        };
        let (account, family) =
            account.ok_or_else(|| OAuthRefusal::grant("The account this sign-in belongs to is no longer active."))?;
        Ok(self.sign_in_answer(&account, &family))
    }

    /// Ends the sign-in behind a refresh token or an access token the stand-in issued; false when it
    /// knows neither (the revoke endpoint answers 200 either way, like Silicon Accounts).
    pub fn revoke_sign_in(&self, token: &str) -> bool {
        let token = token.trim();
        let family = self
            .lock()
            .refresh_tokens
            .get(token)
            .map(|r| r.family.clone())
            .or_else(|| self.claims_of(token).and_then(|c| c["fid"].as_str().map(str::to_owned)));
        match family {
            Some(f) => {
                self.lock().revoked_families.push(f);
                true
            }
            None => false,
        }
    }

    fn check_client(&self, client_id: &str) -> Result<(), OAuthRefusal> {
        if client_id.trim() == self.app_id {
            return Ok(());
        }
        Err(OAuthRefusal {
            status: 401,
            error: "invalid_client",
            description: format!(
                "client_id {client_id:?} isn't an app this Silicon Accounts stand-in signs in to; it serves {} only.",
                self.app_id
            ),
        })
    }

    /// A token response for a sign-in (`family`): a fresh access token and refresh token, and the
    /// account as Silicon Accounts shows it to the app.
    fn sign_in_answer(&self, account: &LocalAccount, family: &str) -> Value {
        let access = self.mint_in_family(account, ACCESS_TTL_S, &self.app_id, &self.issuer, LOCAL_KID, family);
        let refresh = format!("sar_local_{}", random_token(32));
        let mut inner = self.lock();
        inner.refresh_tokens.insert(
            refresh.clone(),
            LocalRefresh {
                uuid: account.uuid.clone(),
                family: family.to_owned(),
                used: false,
            },
        );
        let custodian = account
            .custodian
            .as_ref()
            .and_then(|c| inner.accounts.get(c))
            .map(|c| json!({"uuid": c.uuid, "id": c.id, "kind": "carbon", "display_name": c.display_name}));
        let membership = format!("{}:{}", self.app_id, account.uuid);
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TTL_S,
            "refresh_token": refresh,
            "refresh_token_expires_at": rfc3339(now + 900 * 86_400),
            "scope": "profile",
            "membership_id": membership,
            "account": {
                "uuid": account.uuid,
                "membership_id": membership,
                "kind": match account.kind { MemberKind::Carbon => "carbon", MemberKind::Silicon => "silicon" },
                "id": account.id,
                "display_name": account.display_name,
                "pfp_url": format!("http://127.0.0.1/pfp/{}", account.uuid),
                "custodian": custodian,
            },
        })
    }

    /// Every proof the stand-in issued, oldest first: `{"kind", "receiving_app", "scopes", "user"}`.
    pub fn issued_proofs(&self) -> Vec<Value> {
        self.lock().issued.clone()
    }

    /// Whether a proof token the stand-in issued is still valid (what a receiving app would see).
    pub fn proof_valid(&self, proof_token: &str) -> bool {
        let inner = self.lock();
        inner
            .proof_tokens
            .get(proof_token)
            .and_then(|id| inner.proofs.get(id))
            .is_some_and(|p| !p.revoked && p.family.as_ref().is_none_or(|f| !inner.revoked_families.contains(f)))
    }

    fn claims_of(&self, token: &str) -> Option<Value> {
        let jwks = self.jwks_value();
        let opts = silicon_accounts_client::VerifyOptions::for_app(&self.app_id).with_issuer(self.issuer.clone());
        let claims = silicon_accounts_client::verify_access_token(&jwks, token, &opts).ok()?;
        serde_json::to_value(claims).ok()
    }

    fn summary(a: &LocalAccount, inner: &Inner) -> AccountSummary {
        let custodian = a
            .custodian
            .as_ref()
            .and_then(|c| inner.accounts.get(c))
            .map(|c| json!({"uuid": c.uuid, "id": c.id, "kind": "carbon", "display_name": c.display_name}));
        let value = json!({
            "uuid": a.uuid,
            "kind": match a.kind { MemberKind::Carbon => "carbon", MemberKind::Silicon => "silicon" },
            "id": if a.status == "deleted" { String::new() } else { a.id.clone() },
            "display_name": a.display_name,
            "pfp_url": format!("http://127.0.0.1/pfp/{}", a.uuid),
            "status": a.status,
            "custodian": custodian,
        });
        // AccountSummary is #[non_exhaustive]: built from the same JSON Silicon Accounts answers.
        serde_json::from_value(value).unwrap_or_else(|e| panic!("local account summary: {e}"))
    }

    fn issue(
        &self,
        user: Option<String>,
        family: Option<String>,
        receiving_app: &str,
        scopes: &[String],
    ) -> IssuedProof {
        let mut inner = self.lock();
        let proof_id = uuid::Uuid::now_v7().to_string();
        let token = format!("sap_local_{}", random_token(24));
        let refresh = format!("sapr_local_{}", random_token(24));
        inner.issued.push(json!({
            "kind": if user.is_some() { "user_verification" } else { "app_verification" },
            "receiving_app": receiving_app, "scopes": scopes, "user": user,
        }));
        inner.proofs.insert(
            proof_id.clone(),
            LocalProof {
                proof_id: proof_id.clone(),
                user: user.clone(),
                issuing_app: self.app_id.clone(),
                receiving_app: receiving_app.to_owned(),
                scopes: scopes.to_vec(),
                refresh: refresh.clone(),
                family,
                revoked: false,
            },
        );
        inner.proof_tokens.insert(token.clone(), proof_id.clone());
        let user_json = user
            .as_ref()
            .and_then(|u| inner.accounts.get(u))
            .map(|a| json!({"uuid": a.uuid, "id": a.id, "kind": match a.kind { MemberKind::Carbon => "carbon", MemberKind::Silicon => "silicon" }}));
        proof_answer(
            &proof_id,
            &token,
            &refresh,
            &self.app_id,
            receiving_app,
            user_json,
            scopes,
        )
    }
}

fn proof_answer(
    proof_id: &str,
    token: &str,
    refresh: &str,
    issuing_app: &str,
    receiving_app: &str,
    user: Option<Value>,
    scopes: &[String],
) -> IssuedProof {
    let fmt = |t: time::OffsetDateTime| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    let now = time::OffsetDateTime::now_utc();
    serde_json::from_value(json!({
        "proof_id": proof_id,
        "kind": if user.is_some() { "user_verification" } else { "app_verification" },
        "proof_token": token,
        "expires_at": fmt(now + time::Duration::minutes(30)),
        "proof_refresh_token": refresh,
        "refresh_expires_at": fmt(now + time::Duration::days(900)),
        "issuing_app": issuing_app,
        "receiving_app": receiving_app,
        "user": user,
        "scopes": scopes,
    }))
    .unwrap_or_else(|e| panic!("local proof answer: {e}"))
}

/// A unix time as RFC 3339 with milliseconds, the way Silicon Accounts writes times.
fn rfc3339(unix_s: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix_s)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

fn display_name_of(id: &str) -> String {
    let handle = id.split_once(':').map_or(id, |(_, h)| h);
    let mut c = handle.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn random_token(n: usize) -> String {
    let mut rng = rand::rng();
    (0..n)
        .map(|_| BASE62[rng.random_range(0..BASE62.len())] as char)
        .collect()
}

#[async_trait]
impl AccountsApi for LocalAccounts {
    async fn jwks(&self) -> AppResult<Jwks> {
        Ok(self.jwks_value())
    }

    async fn introspect(&self, token: &str) -> AppResult<Introspection> {
        let active = self.claims_of(token).filter(|c| {
            let inner = self.lock();
            let family_live = c["fid"]
                .as_str()
                .is_none_or(|f| !inner.revoked_families.iter().any(|r| r == f));
            let account_live = c["sub"]
                .as_str()
                .and_then(|u| inner.accounts.get(u))
                .is_some_and(|a| a.status == "active");
            family_live && account_live
        });
        let value = match active {
            Some(c) => json!({"active": true, "sub": c["sub"], "aud": c["aud"], "exp": c["exp"], "iat": c["iat"],
                              "kind": c["kind"], "id": c["id"], "token_type": "access_token", "iss": self.issuer}),
            None => json!({"active": false}),
        };
        serde_json::from_value(value).map_err(AppError::internal)
    }

    async fn lookup(&self, uuid: &str) -> AppResult<Option<AccountSummary>> {
        let inner = self.lock();
        Ok(inner.accounts.get(uuid).map(|a| Self::summary(a, &inner)))
    }

    async fn lookup_by_id(&self, id: &str) -> AppResult<Option<AccountSummary>> {
        let id = id.trim().to_ascii_lowercase();
        let inner = self.lock();
        Ok(inner
            .accounts
            .values()
            .find(|a| a.id == id && a.status != "deleted")
            .map(|a| Self::summary(a, &inner)))
    }

    async fn revoke_token(&self, token: &str) -> AppResult<()> {
        self.revoke_sign_in(token);
        Ok(())
    }

    async fn issue_user_verification(&self, req: &IssueUserVerification, _key: &str) -> AppResult<IssuedProof> {
        let claims = self.claims_of(&req.subject_token).ok_or_else(|| {
            AppError::new(
                ErrorCode::TokenExpired,
                "Silicon Accounts (local stand-in) refused the subject token: it is not a live access token for extend.",
            )
        })?;
        let user = claims["sub"].as_str().map(str::to_owned);
        let family = claims["fid"].as_str().map(str::to_owned);
        if family
            .as_ref()
            .is_some_and(|f| self.lock().revoked_families.contains(f))
        {
            return Err(AppError::new(
                ErrorCode::TokenExpired,
                "Silicon Accounts (local stand-in) refused the subject token: that sign-in ended.",
            ));
        }
        Ok(self.issue(user, family, &req.receiving_app, &req.scopes))
    }

    async fn issue_app_verification(&self, req: &IssueAppVerification, _key: &str) -> AppResult<IssuedProof> {
        Ok(self.issue(None, None, &req.receiving_app, &req.scopes))
    }

    async fn refresh_proof(&self, proof_refresh_token: &str) -> AppResult<IssuedProof> {
        let found = self
            .lock()
            .proofs
            .values()
            .find(|p| p.refresh == proof_refresh_token)
            .cloned();
        let Some(p) = found.filter(|p| !p.revoked) else {
            return Err(AppError::new(
                ErrorCode::AccessRemoved,
                "Silicon Accounts (local stand-in) ended the proof Extend held (410 proof_revoked).",
            ));
        };
        let mut inner = self.lock();
        let token = format!("sap_local_{}", random_token(24));
        let refresh = format!("sapr_local_{}", random_token(24));
        if let Some(stored) = inner.proofs.get_mut(&p.proof_id) {
            stored.refresh = refresh.clone();
        }
        inner.proof_tokens.insert(token.clone(), p.proof_id.clone());
        let user = p
            .user
            .as_ref()
            .and_then(|u| inner.accounts.get(u))
            .map(|a| json!({"uuid": a.uuid, "id": a.id}));
        Ok(proof_answer(
            &p.proof_id,
            &token,
            &refresh,
            &p.issuing_app,
            &p.receiving_app,
            user,
            &p.scopes,
        ))
    }

    async fn revoke_proof(&self, proof_id: &str) -> AppResult<()> {
        if let Some(p) = self.lock().proofs.get_mut(proof_id) {
            p.revoked = true;
        }
        Ok(())
    }
}
