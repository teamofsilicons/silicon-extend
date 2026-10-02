//! Separate endpoint approval and encrypted durable credentials. Each world's schema owns its
//! requests and grants, so cleaning a testing world removes all delegated authority held here.
use crate::{
    db::World,
    error::{AppError, AppResult},
    iam::{OboProof, Principal, TestingSelection},
};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use extend_protocol::ErrorCode;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use silicon_iam_client::{Client, IdempotencyKey, Mutation, models};
use sqlx::{PgPool, Row as _};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub const MIGRATION: &str = r"
CREATE TABLE IF NOT EXISTS {s}.obo_requests (
 id uuid PRIMARY KEY, member_id text NOT NULL, org_id text NOT NULL, request_key uuid NOT NULL,
 request_hash text NOT NULL, payload bytea, authorization_id uuid, consent jsonb,
 code_hash text, completed boolean NOT NULL DEFAULT false, created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(member_id,org_id,request_key)
);
CREATE TABLE IF NOT EXISTS {s}.obo_grants (
 member_id text NOT NULL, org_id text NOT NULL, audience text NOT NULL, endpoint_id text NOT NULL,
 request_id uuid NOT NULL REFERENCES {s}.obo_requests(id), credentials bytea NOT NULL,
 updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(member_id,org_id,audience,endpoint_id)
);
";

#[derive(Clone)]
pub struct GrantKey([u8; 32]);
impl std::fmt::Debug for GrantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantKey(<redacted>)")
    }
}
impl GrantKey {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(value).ok().and_then(|v| v.try_into().ok());
        bytes.map(Self).ok_or_else(|| {
            anyhow::anyhow!("EXTEND_DELEGATION_ENCRYPTION_KEY must be 32 bytes encoded as unpadded base64url")
        })
    }
    fn seal(&self, aad: &str, plain: &[u8]) -> AppResult<Vec<u8>> {
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
    fn open(&self, aad: &str, cipher: &[u8]) -> AppResult<Vec<u8>> {
        if cipher.len() < 28 {
            return Err(crypto_error());
        }
        Aes256Gcm::new_from_slice(&self.0)
            .map_err(|_| crypto_error())?
            .decrypt(
                Nonce::from_slice(&cipher[..12]),
                Payload {
                    msg: &cipher[12..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| crypto_error())
    }
}
fn crypto_error() -> AppError {
    AppError::new(
        ErrorCode::ServiceUnavailable,
        "Extend could not unlock its saved feature approval. Check the delegation encryption key.",
    )
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionInput {
    pub endpoints: Vec<models::OboAuthorizationEndpoint>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteInput {
    pub code: String,
}

pub fn missing(audience: &str, endpoint: &str) -> AppError {
    AppError::new(ErrorCode::ConfirmationRequired, format!("Approve Extend's {audience} feature before using {endpoint}."))
        .hint(format!("Open Extend Settings → Permissions, or run `extend permission request {audience} {endpoint}`. Approve in IAM, then complete the request with the displayed code."))
        .details(json!({"permission_required":true,"audience":audience,"endpoint_id":endpoint}))
}
fn sdk_failure(error: silicon_iam_client::Error, audience: &str, endpoint: &str) -> AppError {
    // Do not log serialized IAM error bodies, authorization codes or token material.
    if matches!(error, silicon_iam_client::Error::Api(ref api) if matches!(api.status,400|401|403|404|409|410)) {
        if audience == "requested" {
            AppError::new(ErrorCode::ConfirmationRequired, "IAM did not accept this approval request or code.")
                .hint("Approve the request in IAM, then paste its code using the same Extend account, organization and testing environment. Start a new request if it expired.")
        } else {
            missing(audience, endpoint)
        }
    } else {
        AppError::new(
            ErrorCode::ServiceUnavailable,
            "IAM could not complete this feature approval. Retry the same request.",
        )
    }
}
pub fn allowed(audience: &str, endpoint: &str) -> bool {
    match audience {
        "briefcase" => matches!(
            endpoint,
            "briefcase.uploads.reserve"
                | "briefcase.uploads.commit"
                | "briefcase.uploads.status"
                | "briefcase.uploads.cancel"
                | "briefcase.files.read"
                | "briefcase.entries.trash"
                | "briefcase.invitations.create"
        ),
        "ting" => matches!(endpoint, "tings.send" | "subscriptions.register"),
        _ => false,
    }
}
fn world(sel: Option<&TestingSelection>) -> World {
    sel.map_or_else(World::production, |s| World::test(s.environment_id))
}
fn mutation(key: &str) -> AppResult<Mutation> {
    let hash = Sha256::digest(key.as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    let key = IdempotencyKey::parse(Uuid::from_bytes(bytes).to_string())
        .map_err(|_| AppError::invalid("Invalid permission retry key."))?;
    Ok(Mutation::with_key(key))
}
fn aad(w: &World, member: &str, org: &str, audience: &str, endpoint: &str) -> String {
    json!(["extend-obo-v1", w.schema, member, org, audience, endpoint]).to_string()
}
fn safe(pair: &models::OboTokenPair) -> Value {
    json!({"audience":pair.audience,"endpoint_id":pair.endpoint_id,"grant_id":pair.grant_id,"org_id":pair.org_id,"actor":pair.actor,"expires_at":pair.expires_at.format(&time::format_description::well_known::Rfc3339).ok()})
}
fn validate_pair(pair: &models::OboTokenPair, sel: Option<&TestingSelection>) -> AppResult<()> {
    let same_plane = match (sel, pair.testing_context.as_ref()) {
        (None, None) => true,
        (Some(_), Some(t)) => t.app_id == pair.audience && !t.app_secret.is_empty() && !t.iam_test_key.is_empty(),
        _ => false,
    };
    if !same_plane
        || pair.actor.as_ref().is_none_or(|a| a.public_id.is_empty())
        || pair.org_id.is_empty()
        || pair.access_token.is_empty()
        || pair.refresh_token.is_empty()
        || pair.grant_id.is_nil()
    {
        return Err(AppError::new(
            ErrorCode::ServiceUnavailable,
            "IAM returned incomplete or mismatched feature authority.",
        ));
    }
    Ok(())
}
#[derive(Clone)]
pub struct GrantStore {
    pool: PgPool,
    key: GrantKey,
}
impl GrantStore {
    pub fn new(pool: PgPool, key: GrantKey) -> Self {
        Self { pool, key }
    }
    pub async fn start(
        &self,
        client: &Client,
        p: &Principal,
        mut input: PermissionInput,
        key: Uuid,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Value> {
        if input.endpoints.is_empty()
            || input.endpoints.len() > 16
            || input.endpoints.iter().any(|e| !allowed(&e.audience, &e.endpoint_id))
        {
            return Err(AppError::invalid("Choose 1–16 supported Briefcase or Ting endpoints."));
        }
        input
            .endpoints
            .sort_by(|a, b| (&a.audience, &a.endpoint_id).cmp(&(&b.audience, &b.endpoint_id)));
        input
            .endpoints
            .dedup_by(|a, b| a.audience == b.audience && a.endpoint_id == b.endpoint_id);
        let w = world(sel);
        let org = p.team()?;
        let request_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&input).map_err(AppError::internal)?)
        );
        let id = Uuid::new_v4();
        let request = models::OboAuthorizationRequest {
            redirect_uri: None,
            state: None,
            subject_token: p.token.clone(),
            org_id: org.to_owned(),
            endpoints: input.endpoints,
        };
        let payload = self.key.seal(
            &aad(&w, p.id(), org, "request", &id.to_string()),
            &serde_json::to_vec(&request).map_err(|_| crypto_error())?,
        )?;
        // Commit pending identity before contacting IAM. An uncertain response retries the same
        // immutable encrypted payload and mutation, even if the browser login has refreshed.
        sqlx::query(sql!("INSERT INTO {} (id,member_id,org_id,request_key,request_hash,payload) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (member_id,org_id,request_key) DO NOTHING",w.t("obo_requests")))
            .bind(id).bind(p.id()).bind(org).bind(key).bind(&request_hash).bind(payload).execute(&self.pool).await?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(sql!(
            "SELECT * FROM {} WHERE member_id=$1 AND org_id=$2 AND request_key=$3 FOR UPDATE",
            w.t("obo_requests")
        ))
        .bind(p.id())
        .bind(org)
        .bind(key)
        .fetch_one(&mut *tx)
        .await?;
        if row.get::<String, _>("request_hash") != request_hash {
            return Err(AppError::invalid(
                "This retry key belongs to a different permission request.",
            ));
        }
        if let Some(consent) = row.get::<Option<Value>, _>("consent") {
            return Ok(consent);
        }
        let id: Uuid = row.get("id");
        let bytes: Vec<u8> = row.get("payload");
        let request: models::OboAuthorizationRequest = serde_json::from_slice(
            &self
                .key
                .open(&aad(&w, p.id(), org, "request", &id.to_string()), &bytes)?,
        )
        .map_err(|_| crypto_error())?;
        let answer = client
            .obo()
            .authorize(&request, &mutation(&format!("start:{id}"))?)
            .await
            .map_err(|e| sdk_failure(e, "requested", "endpoints"))?;
        if answer.actor.public_id != p.id() || answer.org_id != org {
            return Err(AppError::new(
                ErrorCode::NoAccess,
                "IAM returned a permission request for another account or organization.",
            ));
        }
        let url = answer
            .authorization_url
            .ok_or_else(|| AppError::new(ErrorCode::ServiceUnavailable, "IAM did not return a consent page."))?;
        let consent = json!({"id":id,"consent_url":url,"expires_at":answer.expires_at.format(&time::format_description::well_known::Rfc3339).ok()});
        sqlx::query(sql!(
            "UPDATE {} SET authorization_id=$2,consent=$3 WHERE id=$1",
            w.t("obo_requests")
        ))
        .bind(id)
        .bind(answer.id)
        .bind(&consent)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(consent)
    }
    pub async fn complete(
        &self,
        client: &Client,
        p: &Principal,
        id: Uuid,
        code: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Value> {
        if code.is_empty() || code.len() > 2048 {
            return Err(AppError::invalid("Paste the authorization code shown by IAM."));
        }
        let w = world(sel);
        let org = p.team()?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(sql!(
            "SELECT * FROM {} WHERE id=$1 AND member_id=$2 AND org_id=$3 FOR UPDATE",
            w.t("obo_requests")
        ))
        .bind(id)
        .bind(p.id())
        .bind(org)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::NoAccess,
                "This approval request is not available for the selected account and organization.",
            )
        })?;
        let hash = format!("{:x}", Sha256::digest(code.as_bytes()));
        if row.get::<Option<String>, _>("code_hash").is_some_and(|h| h != hash) {
            return Err(AppError::invalid(
                "Retry this approval with the same authorization code.",
            ));
        }
        if row.get::<bool, _>("completed") {
            drop(tx);
            return self.list(p, sel).await;
        }
        let iam_id: Uuid = row
            .get::<Option<Uuid>, _>("authorization_id")
            .ok_or_else(|| AppError::invalid("Start the approval request again with its original retry key."))?;
        let payload: Vec<u8> = row.get("payload");
        let request: models::OboAuthorizationRequest = serde_json::from_slice(
            &self
                .key
                .open(&aad(&w, p.id(), org, "request", &id.to_string()), &payload)?,
        )
        .map_err(|_| crypto_error())?;
        // The key binds to the exact code. A process crash before commit reuses this key.
        let answer = client
            .obo()
            .exchange_code(iam_id, code, &mutation(&format!("complete:{id}:{hash}"))?)
            .await
            .map_err(|e| sdk_failure(e, "requested", "endpoints"))?;
        if answer.items.len() != request.endpoints.len()
            || request.endpoints.iter().any(|e| {
                answer
                    .items
                    .iter()
                    .filter(|a| a.audience == e.audience && a.endpoint_id == e.endpoint_id)
                    .count()
                    != 1
            })
        {
            return Err(AppError::new(
                ErrorCode::ServiceUnavailable,
                "IAM returned a different endpoint set than the approval request.",
            ));
        }
        let mut views = Vec::new();
        for pair in answer.items {
            validate_pair(&pair, sel)?;
            let encrypted = self.key.seal(
                &aad(&w, p.id(), org, &pair.audience, &pair.endpoint_id),
                &serde_json::to_vec(&pair).map_err(|_| crypto_error())?,
            )?;
            sqlx::query(sql!("INSERT INTO {} (member_id,org_id,audience,endpoint_id,request_id,credentials) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (member_id,org_id,audience,endpoint_id) DO UPDATE SET credentials=EXCLUDED.credentials,request_id=EXCLUDED.request_id,updated_at=now()",w.t("obo_grants"))).bind(p.id()).bind(org).bind(&pair.audience).bind(&pair.endpoint_id).bind(id).bind(encrypted).execute(&mut *tx).await?;
            views.push(safe(&pair));
        }
        sqlx::query(sql!(
            "UPDATE {} SET code_hash=$2,completed=true,payload=NULL WHERE id=$1",
            w.t("obo_requests")
        ))
        .bind(id)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(json!({"items":views}))
    }
    pub async fn list(&self, p: &Principal, sel: Option<&TestingSelection>) -> AppResult<Value> {
        let w = world(sel);
        let org = p.team()?;
        let rows=sqlx::query(sql!("SELECT audience,endpoint_id,credentials FROM {} WHERE member_id=$1 AND org_id=$2 ORDER BY audience,endpoint_id",w.t("obo_grants"))).bind(p.id()).bind(org).fetch_all(&self.pool).await?;
        let mut items = Vec::new();
        for row in rows {
            let audience: String = row.get("audience");
            let endpoint: String = row.get("endpoint_id");
            let bytes: Vec<u8> = row.get("credentials");
            let pair: models::OboTokenPair =
                serde_json::from_slice(&self.key.open(&aad(&w, p.id(), org, &audience, &endpoint), &bytes)?)
                    .map_err(|_| crypto_error())?;
            items.push(safe(&pair));
        }
        Ok(json!({"items":items}))
    }
    pub async fn access(
        &self,
        client: &Client,
        p: &Principal,
        audience: &str,
        endpoint: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof> {
        let w = world(sel);
        let org = p.team()?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(sql!(
            "SELECT credentials FROM {} WHERE member_id=$1 AND org_id=$2 AND audience=$3 AND endpoint_id=$4 FOR UPDATE",
            w.t("obo_grants")
        ))
        .bind(p.id())
        .bind(org)
        .bind(audience)
        .bind(endpoint)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| missing(audience, endpoint))?;
        let binding = aad(&w, p.id(), org, audience, endpoint);
        let cipher: Vec<u8> = row.get("credentials");
        let mut pair: models::OboTokenPair =
            serde_json::from_slice(&self.key.open(&binding, &cipher)?).map_err(|_| crypto_error())?;
        validate_pair(&pair, sel)?;
        if pair.expires_at <= OffsetDateTime::now_utc() + Duration::seconds(30) {
            // Row lock serializes across processes; deterministic key survives lost responses and
            // transaction rollback without treating an uncertain rotation as refresh-token reuse.
            let answer = client
                .obo()
                .refresh(
                    &pair.refresh_token,
                    &mutation(&format!("refresh:{}:{}", pair.grant_id, pair.refresh_token))?,
                )
                .await
                .map_err(|e| sdk_failure(e, audience, endpoint))?;
            let next = answer
                .items
                .into_iter()
                .find(|a| a.grant_id == pair.grant_id && a.audience == audience && a.endpoint_id == endpoint)
                .ok_or_else(|| missing(audience, endpoint))?;
            validate_pair(&next, sel)?;
            if next.org_id != pair.org_id
                || next.actor.as_ref().map(|a| &a.public_id) != pair.actor.as_ref().map(|a| &a.public_id)
            {
                return Err(missing(audience, endpoint));
            }
            pair = next;
            let cipher = self
                .key
                .seal(&binding, &serde_json::to_vec(&pair).map_err(|_| crypto_error())?)?;
            sqlx::query(sql!("UPDATE {} SET credentials=$5,updated_at=now() WHERE member_id=$1 AND org_id=$2 AND audience=$3 AND endpoint_id=$4",w.t("obo_grants"))).bind(p.id()).bind(org).bind(audience).bind(endpoint).bind(cipher).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(OboProof {
            access_proof: pair.access_token,
            actor: pair.actor.map(|a| a.public_id),
            org_id: Some(pair.org_id),
            testing_app_secret: pair.testing_context.as_ref().map(|t| t.app_secret.clone()),
            testing_iam_key: pair.testing_context.map(|t| t.iam_test_key),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ciphertext_is_bound_to_world_subject_and_endpoint() {
        let key = GrantKey([7; 32]);
        let binding = aad(&World::production(), "si:a", "tos", "briefcase", "briefcase.files.read");
        let c = key.seal(&binding, b"secret refresh token").unwrap();
        assert_eq!(key.open(&binding, &c).unwrap(), b"secret refresh token");
        for other in [
            aad(
                &World::test(Uuid::new_v4()),
                "si:a",
                "tos",
                "briefcase",
                "briefcase.files.read",
            ),
            aad(&World::production(), "si:b", "tos", "briefcase", "briefcase.files.read"),
            aad(
                &World::production(),
                "si:a",
                "other",
                "briefcase",
                "briefcase.files.read",
            ),
            aad(
                &World::production(),
                "si:a",
                "tos",
                "briefcase",
                "briefcase.entries.trash",
            ),
        ] {
            assert!(key.open(&other, &c).is_err());
        }
        let mut corrupt = c;
        corrupt[12] ^= 1;
        assert!(key.open(&binding, &corrupt).is_err());
        assert!(!format!("{key:?}").contains("7"));
    }
    #[test]
    fn supported_catalog_excludes_retired_upload_and_arbitrary_authority() {
        assert!(!allowed("briefcase", "briefcase.files.create"));
        assert!(!allowed("ting", "types.register"));
        assert!(allowed("ting", "subscriptions.register"));
        assert!(allowed("briefcase", "briefcase.uploads.commit"));
    }
}
