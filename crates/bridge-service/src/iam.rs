//! Silicon IAM: login, live authorization, test-environment selection, OBO proofs and webhooks.
//!
//! Two implementations sit behind [`Iam`]: [`SdkIam`] uses the official `silicon-iam-client`
//! crate against a real IAM, and [`LocalIam`] stands in for development and tests (refused in
//! production by `Config`). Handlers never trust cached claims for long: authorization answers are
//! cached for at most 30 seconds and dropped the moment a webhook names the member.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bridge_protocol::model::{AuthSession, Member, MemberKind};
use bridge_protocol::{ErrorCode, ids};
use rand::Rng as _;
use sha2::{Digest as _, Sha256};
use silicon_iam_client::{Client as SdkClient, Credential, IdempotencyKey, Mutation, models};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// The test environment a request selected, and the secret that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestingSelection {
    pub environment_id: Uuid,
    pub name: String,
    pub secret: String,
}

/// A signed-in member acting in one team.
#[derive(Debug, Clone)]
pub struct Principal {
    pub member: Member,
    pub team: Option<String>,
    pub teams: Vec<String>,
    pub role: Option<String>,
    /// The access token, kept to mint OBO proofs for Briefcase and Ting on the member's behalf.
    pub token: String,
}

impl Principal {
    pub fn id(&self) -> &str {
        &self.member.id
    }
    pub fn is_silicon(&self) -> bool {
        self.member.kind == MemberKind::Silicon
    }
    pub fn is_carbon(&self) -> bool {
        self.member.kind == MemberKind::Carbon
    }
    pub fn team(&self) -> AppResult<&str> {
        self.team.as_deref().ok_or_else(|| {
            AppError::invalid("This request needs a team.").hint("Send the team handle in X-Org-ID, or pass --team <handle> to the CLI.")
        })
    }
}

/// A proof for one delegated request to another application.
#[derive(Debug, Clone)]
pub struct OboProof {
    pub access_proof: String,
    /// In a test environment, the other application's test secret and IAM test key.
    pub testing_app_secret: Option<String>,
    pub testing_iam_key: Option<String>,
}

/// A verified IAM webhook, reduced to what Bridge acts on.
#[derive(Debug, Clone)]
pub struct IamEvent {
    pub event_id: String,
    pub event_type: String,
    /// Public ids (`c:…`, `si:…`) named anywhere in the event.
    pub members: Vec<String>,
    /// Team handles named in the event.
    pub teams: Vec<String>,
    pub testing_environment_id: Option<Uuid>,
}

impl IamEvent {
    /// True for events that can end a member's access (logout, removal, revocation, deletion).
    pub fn ends_access(&self) -> bool {
        let t = self.event_type.to_ascii_lowercase();
        ["remov", "revok", "logout", "logged_out", "delet", "left", "suspend", "deactivat", "expire", "consent"]
            .iter()
            .any(|w| t.contains(w))
    }
}

#[async_trait]
pub trait Iam: Send + Sync {
    fn app_id(&self) -> &str;
    async fn login(&self, slt: &str, idempotency_key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession>;
    async fn refresh(&self, refresh_token: &str, idempotency_key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession>;
    async fn logout(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<()>;
    /// Live authorization for a bearer token, optionally in one team.
    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal>;
    /// Whether a Carbon or Silicon is an active member of a team.
    async fn member_active(&self, team: &str, member_id: &str, sel: Option<&TestingSelection>) -> AppResult<bool>;
    /// The Silicons in the principal's team (for choosing who gets access).
    async fn team_silicons(&self, principal: &Principal, sel: Option<&TestingSelection>) -> AppResult<Vec<bridge_protocol::model::TeamSilicon>>;
    /// Validates a test application secret and names its environment.
    async fn select_testing(&self, secret: &str) -> AppResult<(Uuid, String)>;
    #[allow(clippy::too_many_arguments)]
    async fn obo_proof(
        &self,
        principal: &Principal,
        audience: &str,
        endpoint_id: &str,
        metadata: serde_json::Value,
        method: &str,
        body: &[u8],
        sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof>;
    async fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent>;
}

/// Caches authorization answers for at most 30 seconds.
#[derive(Default)]
pub struct AuthCache {
    entries: RwLock<HashMap<(String, Option<String>, Option<Uuid>), (Instant, Principal)>>,
}

impl AuthCache {
    const TTL: Duration = Duration::from_secs(30);

    pub async fn get(&self, token: &str, team: Option<&str>, env: Option<Uuid>) -> Option<Principal> {
        let key = (ids::secret_digest(token), team.map(str::to_owned), env);
        let entries = self.entries.read().await;
        entries.get(&key).filter(|(at, _)| at.elapsed() < Self::TTL).map(|(_, p)| p.clone())
    }
    pub async fn put(&self, token: &str, team: Option<&str>, env: Option<Uuid>, p: Principal) {
        let key = (ids::secret_digest(token), team.map(str::to_owned), env);
        let mut entries = self.entries.write().await;
        if entries.len() > 50_000 {
            entries.retain(|_, (at, _)| at.elapsed() < Self::TTL);
        }
        entries.insert(key, (Instant::now(), p));
    }
    /// Drops every cached answer for these members (or everything when `members` is empty).
    pub async fn forget(&self, members: &[String]) {
        let mut entries = self.entries.write().await;
        if members.is_empty() {
            entries.clear();
        } else {
            entries.retain(|_, (_, p)| !members.iter().any(|m| m == &p.member.id));
        }
    }
    pub async fn forget_token(&self, token: &str) {
        let digest = ids::secret_digest(token);
        self.entries.write().await.retain(|(d, _, _), _| d != &digest);
    }
}

fn member_from_public_id(id: &str) -> AppResult<Member> {
    let kind = ids::member_kind(id).ok_or_else(|| AppError::new(ErrorCode::Unauthorized, format!("IAM returned an unusable member id {id:?}")))?;
    Ok(Member { kind, id: id.to_owned(), display_name: None })
}

// ───────────────────────────── Official client ─────────────────────────────

pub struct SdkIam {
    sdk: SdkClient,
    app_id: String,
    app_secret: String,
    verifier: Option<silicon_iam_client::WebhookVerifier>,
}

impl SdkIam {
    pub async fn connect(
        base_url: &str,
        app_id: &str,
        app_secret: &str,
        webhook: Option<(i64, String)>,
        previous: Option<(i64, String)>,
    ) -> anyhow::Result<Self> {
        let sdk = SdkClient::builder(base_url)?
            .credential(Credential::application(app_id, app_secret))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("silicon-bridge/", env!("CARGO_PKG_VERSION")))
            .auto_update(false)
            .build()?;
        sdk.system().negotiate().await?;
        let verifier = match webhook {
            Some((v, s)) => {
                let mut keyring =
                    silicon_iam_client::WebhookSecretKeyring::new(v, silicon_iam_client::WebhookSecret::new(s)?)?;
                if let Some((pv, ps)) = previous {
                    keyring.insert(pv, silicon_iam_client::WebhookSecret::new(ps)?)?;
                }
                Some(silicon_iam_client::WebhookVerifier::new(keyring))
            }
            None => None,
        };
        Ok(Self { sdk, app_id: app_id.to_owned(), app_secret: app_secret.to_owned(), verifier })
    }

    fn client(&self, sel: Option<&TestingSelection>) -> AppResult<SdkClient> {
        match sel {
            None => Ok(self.sdk.clone()),
            Some(s) => self
                .sdk
                .with_testing_application(&self.app_id, &s.secret)
                .map(|c| c.with_credential(Credential::application(&self.app_id, &s.secret)))
                .map_err(|e| AppError::new(ErrorCode::TestingSecretInvalid, format!("The test application secret was refused: {e}"))),
        }
    }

    fn session(&self, tokens: models::OAuthTokenResponse, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let actor = tokens.actor.ok_or_else(|| AppError::unavailable("Silicon IAM", "login returned no actor"))?;
        let member = member_from_public_id(&actor.public_id)?;
        Ok(AuthSession {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            token_type: "Bearer".into(),
            expires_in: tokens.expires_in,
            member,
            teams: tokens.org_id.into_iter().collect(),
            testing_environment: sel.map(|s| bridge_protocol::model::TestingEnvironment {
                environment_id: s.environment_id,
                name: s.name.clone(),
                state: "ready".into(),
                paired_devices: 0,
                device_limit: bridge_protocol::TEST_DEVICE_LIMIT,
            }),
        })
    }
}

fn mutation(key: &str) -> AppResult<Mutation> {
    // IAM wants 16+ characters; hash the caller's key so any valid key maps to a stable one.
    let digest = ids::hex_lower(&Sha256::digest(format!("silicon-bridge:{key}").as_bytes()));
    IdempotencyKey::parse(digest).map(Mutation::with_key).map_err(AppError::internal)
}

fn sdk_error(err: silicon_iam_client::Error) -> AppError {
    match &err {
        silicon_iam_client::Error::Api(api) if api.status == 401 || api.status == 400 => {
            AppError::new(ErrorCode::SltInvalid, format!("Silicon IAM refused the token: {} ({})", api.message, api.code))
                .hint("Generate a new short-lived token with the IAM CLI and run `bridge login <slt>` again.")
        }
        silicon_iam_client::Error::Api(api) if api.status == 403 => {
            AppError::new(ErrorCode::NotATeamMember, format!("Silicon IAM refused access: {} ({})", api.message, api.code))
        }
        silicon_iam_client::Error::RateLimited { .. } => {
            AppError::new(ErrorCode::RateLimited, "Silicon IAM is rate limiting Bridge; retry shortly.")
        }
        _ => AppError::unavailable("Silicon IAM", &err),
    }
}

#[async_trait]
impl Iam for SdkIam {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    async fn login(&self, slt: &str, key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        if slt.is_empty() || slt.len() > 4096 || !slt.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(AppError::invalid("slt must be 1–4096 printable characters."));
        }
        if sel.is_none() && ids::member_kind(slt).is_some() {
            return Err(AppError::new(ErrorCode::SltInvalid, "A member id works as a login only in a test environment.")
                .hint("Generate a short-lived token with the IAM CLI and pass that instead."));
        }
        let tokens = self.client(sel)?.oauth().login(&self.app_id, slt, &mutation(key)?).await.map_err(sdk_error)?;
        let mut session = self.session(tokens, sel)?;
        if let Ok(Some(list)) = self.client(sel)?.oauth().authorizations(&session.access_token).await {
            session.teams = list.into_iter().map(|a| a.org_id).collect();
            session.teams.sort();
            session.teams.dedup();
        }
        Ok(session)
    }

    async fn refresh(&self, token: &str, key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let tokens = self.client(sel)?.oauth().refresh(&self.app_id, token, &mutation(key)?).await.map_err(|e| {
            let mut err = sdk_error(e);
            if err.code() == ErrorCode::SltInvalid {
                err = AppError::new(ErrorCode::TokenExpired, "The refresh token is no longer accepted; the IAM session ended.")
                    .hint("Get a new short-lived token and run `bridge login <slt>`.");
            }
            err
        })?;
        let mut session = self.session(tokens, sel)?;
        if let Ok(Some(list)) = self.client(sel)?.oauth().authorizations(&session.access_token).await {
            session.teams = list.into_iter().map(|a| a.org_id).collect();
        }
        Ok(session)
    }

    async fn logout(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<()> {
        let hint = if token.starts_with("ort_") { Some("refresh_token".to_owned()) } else { Some("access_token".to_owned()) };
        let _ = hint;
        self.client(sel)?
            .oauth()
            .revoke(&models::OAuthRevocationRequest { token: token.to_owned(), token_type_hint: None }, &Mutation::new())
            .await
            .map_err(sdk_error)
    }

    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal> {
        let client = self.client(sel)?;
        let not_signed_in = || {
            AppError::new(ErrorCode::TokenExpired, "The access token is not active: it expired, was revoked, or belongs to another application.")
                .hint("Run `bridge login status`; if it fails, get a new short-lived token and run `bridge login <slt>`.")
        };
        let all = client.oauth().authorizations(token).await.map_err(sdk_error)?.ok_or_else(not_signed_in)?;
        let env = sel.map(|s| s.environment_id);
        let all: Vec<_> = all.into_iter().filter(|a| a.audience == self.app_id && a.testing_environment_id == env).collect();
        let first = all.first().ok_or_else(|| {
            AppError::new(ErrorCode::NotATeamMember, "The token reaches no active team membership for Bridge.")
                .hint("Select a team when approving the login in Silicon IAM.")
        })?;
        let public_id = first.public_id.clone().ok_or_else(|| {
            AppError::new(ErrorCode::Unauthorized, "IAM did not disclose who this token belongs to.")
                .hint("Approve the identity scope for Bridge in Silicon IAM.")
        })?;
        let member = member_from_public_id(&public_id)?;
        let mut teams: Vec<String> = all.iter().map(|a| a.org_id.clone()).collect();
        teams.sort();
        teams.dedup();
        let (team, role) = match team {
            None => (None, None),
            Some(t) => {
                let a = all.iter().find(|a| a.org_id == t).ok_or_else(|| {
                    AppError::new(ErrorCode::NotATeamMember, format!("{public_id} is not an active member of team {t:?} for Bridge."))
                        .hint(format!("Teams this login reaches: {}.", teams.join(", ")))
                })?;
                (Some(t.to_owned()), a.org_role.clone())
            }
        };
        Ok(Principal { member, team, teams, role, token: token.to_owned() })
    }

    async fn member_active(&self, team: &str, member_id: &str, sel: Option<&TestingSelection>) -> AppResult<bool> {
        let client = self.client(sel)?;
        let result = if member_id.starts_with("si:") {
            client.silicons().get(team, member_id).await.map(|s| matches!(s.status, models::SiliconStatus::Active))
        } else {
            client
                .members()
                .get(team, &format!("{member_id}[{team}]"))
                .await
                .map(|m| serde_json::to_value(&m.status).ok().and_then(|v| v.as_str().map(|s| s == "active")).unwrap_or(false))
        };
        match result {
            Ok(active) => Ok(active),
            Err(silicon_iam_client::Error::Api(api)) if api.status == 404 || api.status == 403 => Ok(false),
            Err(e) => Err(sdk_error(e)),
        }
    }

    async fn team_silicons(&self, principal: &Principal, sel: Option<&TestingSelection>) -> AppResult<Vec<bridge_protocol::model::TeamSilicon>> {
        let team = principal.team()?.to_owned();
        let client = self.client(sel)?.with_credential(Credential::bearer(&principal.token));
        let mut out = Vec::new();
        let mut paging = silicon_iam_client::Paging::new();
        for _ in 0..20 {
            let page = client.members().directory(&team, Some("id,name,display_name"), &paging).await.map_err(sdk_error)?;
            for m in page.items {
                if let Some(id) = m.id.filter(|i| i.starts_with("si:")) {
                    out.push(bridge_protocol::model::TeamSilicon { id, display_name: m.display_name.or(m.name) });
                }
            }
            match page.page.next_cursor.filter(|_| page.page.has_more) {
                Some(c) => paging = silicon_iam_client::Paging::new().after(c),
                None => break,
            }
        }
        Ok(out)
    }

    async fn select_testing(&self, secret: &str) -> AppResult<(Uuid, String)> {
        let invalid = |why: String| {
            AppError::new(ErrorCode::TestingSecretInvalid, format!("The test application secret was refused: {why}. Nothing ran in production."))
                .hint("Check the secret from Honeycomb, or leave testing to use production.")
        };
        if !ids::is_secret(ids::APP_SECRET_PREFIX, secret) {
            return Err(invalid("it must be ask_ followed by 43 characters".into()));
        }
        let client = self
            .sdk
            .with_testing_application(&self.app_id, secret)
            .map_err(|e| invalid(e.to_string()))?
            .with_credential(Credential::application(&self.app_id, secret));
        let ctx = client.applications().testing_context().await.map_err(|e| match e {
            silicon_iam_client::Error::Api(api) if api.status < 500 => invalid(format!("{} ({})", api.message, api.code)),
            other => AppError::unavailable("Silicon IAM", other),
        })?;
        if ctx.application.app_id != self.app_id {
            return Err(invalid(format!("it belongs to application {:?}, not Bridge", ctx.application.app_id)));
        }
        let name = ctx.environment.map(|e| e.name).unwrap_or_else(|| ctx.environment_id.to_string());
        let _ = &self.app_secret;
        Ok((ctx.environment_id, name))
    }

    async fn obo_proof(
        &self,
        principal: &Principal,
        audience: &str,
        endpoint_id: &str,
        metadata: serde_json::Value,
        method: &str,
        body: &[u8],
        sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof> {
        let client = self.client(sel)?;
        let catalog = client.obo().endpoints(audience).await.map_err(sdk_error)?;
        let request = models::OboExchangeRequest {
            org_id: principal.team.clone(),
            subject_token: principal.token.clone(),
            audience: audience.to_owned(),
            endpoint_id: endpoint_id.to_owned(),
            metadata,
            request: models::OboExchangeRequestBinding {
                method: method.to_owned(),
                body_sha256: silicon_iam_client::api::obo::body_sha256(body),
            },
        };
        let proof = client.obo().exchange_signed(&request, &catalog, &Mutation::new()).await.map_err(sdk_error)?;
        Ok(OboProof {
            access_proof: proof.access_proof,
            testing_app_secret: proof.testing_context.as_ref().map(|t| t.app_secret.clone()),
            testing_iam_key: proof.testing_context.map(|t| t.iam_test_key),
        })
    }

    async fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent> {
        let verifier = self.verifier.as_ref().ok_or_else(|| {
            AppError::new(ErrorCode::Unauthorized, "Bridge has no IAM webhook secret configured.")
        })?;
        let verified = verifier
            .verify(headers, body)
            .map_err(|e| AppError::new(ErrorCode::Unauthorized, format!("IAM webhook signature rejected: {e}")))?;
        let event = verified.event();
        let raw = serde_json::to_value(event).unwrap_or_default();
        let mut members = Vec::new();
        let mut teams = Vec::new();
        collect_ids(&raw, &mut members, &mut teams);
        Ok(IamEvent {
            event_id: verified.event_id().to_string(),
            event_type: event.event_type.clone(),
            members,
            teams,
            testing_environment_id: None,
        })
    }
}

/// Walks an event and collects every member public id and team handle it names.
fn collect_ids(v: &serde_json::Value, members: &mut Vec<String>, teams: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => {
            if ids::member_kind(s).is_some() && !s.contains('[') && !members.contains(s) {
                members.push(s.clone());
            }
            // Membership ids look like `c:alice[acme]`.
            if let (Some(open), true) = (s.find('['), s.ends_with(']')) {
                let (id, team) = (&s[..open], &s[open + 1..s.len() - 1]);
                if ids::member_kind(id).is_some() {
                    if !members.iter().any(|m| m == id) {
                        members.push(id.to_owned());
                    }
                    if !teams.iter().any(|t| t == team) {
                        teams.push(team.to_owned());
                    }
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_ids(x, members, teams)),
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                if (k == "org_id" || k == "organization_handle") && x.is_string() {
                    let t = x.as_str().unwrap_or_default().to_owned();
                    if !teams.contains(&t) {
                        teams.push(t);
                    }
                }
                collect_ids(x, members, teams);
            }
        }
        _ => {}
    }
}

// ───────────────────────────── Local stand-in ─────────────────────────────

#[derive(Debug, Clone)]
struct LocalToken {
    member: String,
    teams: Vec<String>,
    env: Option<Uuid>,
    refresh: bool,
}

/// A development IAM. Members come from `BRIDGE_LOCAL_MEMBERS` (or anyone, when that is empty);
/// the SLT is the member id, optionally with `@team+team`. Refused in production.
pub struct LocalIam {
    app_id: String,
    members: RwLock<HashMap<String, Vec<String>>>,
    open: bool,
    tokens: RwLock<HashMap<String, LocalToken>>,
    pool: sqlx::PgPool,
}

impl LocalIam {
    pub fn new(members: Vec<(String, Vec<String>)>, pool: sqlx::PgPool) -> Self {
        let open = members.is_empty();
        Self {
            app_id: "bridge".into(),
            members: RwLock::new(members.into_iter().collect()),
            open,
            tokens: RwLock::new(HashMap::new()),
            pool,
        }
    }

    /// Adds or removes a member's team (the dev stand-in for IAM membership changes).
    pub async fn set_member(&self, id: &str, teams: Option<Vec<String>>) {
        let mut m = self.members.write().await;
        match teams {
            Some(t) => {
                m.insert(id.to_owned(), t);
            }
            None => {
                m.remove(id);
            }
        }
    }

    /// Revokes every token a member holds (the dev stand-in for an IAM logout).
    pub async fn revoke_member(&self, id: &str) {
        self.tokens.write().await.retain(|_, t| t.member != id);
    }

    fn new_token(prefix: &str) -> String {
        let n: u128 = rand::rng().random();
        format!("{prefix}local_{n:032x}")
    }

    async fn teams_of(&self, id: &str) -> Option<Vec<String>> {
        let m = self.members.read().await;
        match m.get(id) {
            Some(t) => Some(t.clone()),
            None if self.open => Some(vec!["acme".into()]),
            None => None,
        }
    }

    async fn issue(&self, member: &str, teams: Vec<String>, env: Option<Uuid>, sel: Option<&TestingSelection>) -> AuthSession {
        let access = Self::new_token("oat_");
        let refresh = Self::new_token("ort_");
        let mut t = self.tokens.write().await;
        t.insert(access.clone(), LocalToken { member: member.to_owned(), teams: teams.clone(), env, refresh: false });
        t.insert(refresh.clone(), LocalToken { member: member.to_owned(), teams: teams.clone(), env, refresh: true });
        AuthSession {
            access_token: access,
            refresh_token: refresh,
            token_type: "Bearer".into(),
            expires_in: 3600,
            member: Member { kind: ids::member_kind(member).unwrap_or(MemberKind::Carbon), id: member.to_owned(), display_name: None },
            teams,
            testing_environment: sel.map(|s| bridge_protocol::model::TestingEnvironment {
                environment_id: s.environment_id,
                name: s.name.clone(),
                state: "ready".into(),
                paired_devices: 0,
                device_limit: bridge_protocol::TEST_DEVICE_LIMIT,
            }),
        }
    }
}

#[async_trait]
impl Iam for LocalIam {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    async fn login(&self, slt: &str, _key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let raw = slt.trim().trim_start_matches("local:");
        let (id, teams_hint) = raw.split_once('@').map_or((raw, None), |(a, b)| (a, Some(b)));
        if ids::member_kind(id).is_none() {
            return Err(AppError::new(ErrorCode::SltInvalid, format!("Local IAM expects a member id like c:alice or si:chef, got {slt:?}.")));
        }
        let known = self.teams_of(id).await.ok_or_else(|| {
            AppError::new(ErrorCode::SltInvalid, format!("{id} is not a member local IAM knows (BRIDGE_LOCAL_MEMBERS)."))
        })?;
        let teams = match teams_hint {
            Some(h) => h.split('+').filter(|t| known.iter().any(|k| k == t)).map(str::to_owned).collect(),
            None => known,
        };
        if teams.is_empty() {
            return Err(AppError::new(ErrorCode::NotATeamMember, format!("{id} is not in any of those teams.")));
        }
        Ok(self.issue(id, teams, sel.map(|s| s.environment_id), sel).await)
    }

    async fn refresh(&self, token: &str, _key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let old = self.tokens.write().await.remove(token).filter(|t| t.refresh).ok_or_else(|| {
            AppError::new(ErrorCode::TokenExpired, "The refresh token is no longer accepted.").hint("Run `bridge login <slt>` again.")
        })?;
        Ok(self.issue(&old.member, old.teams, old.env, sel).await)
    }

    async fn logout(&self, token: &str, _sel: Option<&TestingSelection>) -> AppResult<()> {
        let mut t = self.tokens.write().await;
        if let Some(found) = t.remove(token)
            && found.refresh {
                t.retain(|_, x| x.member != found.member);
            }
        Ok(())
    }

    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal> {
        let found = self.tokens.read().await.get(token).cloned().filter(|t| !t.refresh).ok_or_else(|| {
            AppError::new(ErrorCode::TokenExpired, "The access token is not active: it expired, was revoked, or never existed.")
                .hint("Run `bridge login <slt>` again.")
        })?;
        if found.env != sel.map(|s| s.environment_id) {
            return Err(AppError::new(
                ErrorCode::TokenExpired,
                "This token belongs to a different environment (production and test logins are kept apart).",
            ));
        }
        let current = self.teams_of(&found.member).await.unwrap_or_default();
        let teams: Vec<String> = found.teams.iter().filter(|t| current.contains(t)).cloned().collect();
        if teams.is_empty() {
            return Err(AppError::new(ErrorCode::NotATeamMember, format!("{} is no longer an active member of any team.", found.member)));
        }
        let team = match team {
            None => None,
            Some(t) if teams.iter().any(|x| x == t) => Some(t.to_owned()),
            Some(t) => {
                return Err(AppError::new(ErrorCode::NotATeamMember, format!("{} is not an active member of team {t:?}.", found.member))
                    .hint(format!("Teams this login reaches: {}.", teams.join(", "))));
            }
        };
        Ok(Principal {
            member: Member { kind: ids::member_kind(&found.member).unwrap_or(MemberKind::Carbon), id: found.member, display_name: None },
            team,
            teams,
            role: Some("member".into()),
            token: token.to_owned(),
        })
    }

    async fn member_active(&self, team: &str, member_id: &str, _sel: Option<&TestingSelection>) -> AppResult<bool> {
        Ok(ids::member_kind(member_id).is_some() && self.teams_of(member_id).await.is_some_and(|t| t.iter().any(|x| x == team)))
    }

    async fn team_silicons(&self, principal: &Principal, _sel: Option<&TestingSelection>) -> AppResult<Vec<bridge_protocol::model::TeamSilicon>> {
        let team = principal.team()?.to_owned();
        let members = self.members.read().await;
        let mut out: Vec<_> = members
            .iter()
            .filter(|(id, teams)| id.starts_with("si:") && teams.contains(&team))
            .map(|(id, _)| bridge_protocol::model::TeamSilicon { id: id.clone(), display_name: None })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    async fn select_testing(&self, secret: &str) -> AppResult<(Uuid, String)> {
        let invalid = || {
            AppError::new(ErrorCode::TestingSecretInvalid, "The test application secret is not registered with any active test environment. Nothing ran in production.")
                .hint("Check the secret, or leave testing to use production.")
        };
        if !ids::is_secret(ids::APP_SECRET_PREFIX, secret) {
            return Err(invalid());
        }
        let row: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT e.environment_id, e.name, e.state FROM bridge_global.local_test_apps a
             JOIN bridge_global.test_environments e USING (environment_id) WHERE a.secret_digest = $1",
        )
        .bind(ids::secret_digest(secret))
        .fetch_optional(&self.pool)
        .await?;
        let (id, name, _state) = row.ok_or_else(invalid)?;
        Ok((id, name))
    }

    async fn obo_proof(
        &self,
        principal: &Principal,
        audience: &str,
        endpoint_id: &str,
        _metadata: serde_json::Value,
        _method: &str,
        _body: &[u8],
        _sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof> {
        Ok(OboProof { access_proof: format!("obo_local:{audience}:{endpoint_id}:{}", principal.id()), testing_app_secret: None, testing_iam_key: None })
    }

    async fn verify_webhook(&self, _headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent> {
        // Local events are plain JSON: {"event_id","event_type","members":[...],"teams":[...]}.
        let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| AppError::invalid(format!("local IAM event is not JSON: {e}")))?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
        let list = |k: &str| {
            v.get(k).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default()
        };
        Ok(IamEvent {
            event_id: s("event_id"),
            event_type: s("event_type"),
            members: list("members"),
            teams: list("teams"),
            testing_environment_id: v.get("environment_id").and_then(|x| x.as_str()).and_then(|x| x.parse().ok()),
        })
    }
}

pub type DynIam = Arc<dyn Iam>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_members_and_teams() {
        let v = serde_json::json!({"aggregate":{"membership_id":"si:chef[acme]"},"data":{"actor":{"public_id":"c:alice"},"org_id":"acme"}});
        let (mut m, mut t) = (vec![], vec![]);
        collect_ids(&v, &mut m, &mut t);
        assert!(m.contains(&"si:chef".to_owned()));
        assert!(m.contains(&"c:alice".to_owned()));
        assert_eq!(t, vec!["acme".to_owned()]);
    }

    #[test]
    fn access_ending_events() {
        let e = |t: &str| IamEvent { event_id: "1".into(), event_type: t.into(), members: vec![], teams: vec![], testing_environment_id: None };
        assert!(e("organization.member.removed.v1").ends_access());
        assert!(e("session.revoked.v1").ends_access());
        assert!(!e("organization.updated.v1").ends_access());
    }
}
