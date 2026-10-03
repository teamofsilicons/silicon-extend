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
use extend_protocol::model::{AuthSession, Member, MemberKind};
use extend_protocol::{ErrorCode, ids};
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
    /// The app login token. Separate feature approvals are required for OBO operations.
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
            AppError::invalid("This request needs a team.")
                .hint("Send the team handle in X-Org-ID, or pass --team <handle> to the CLI.")
        })
    }
}

/// IAM's answer to "is this member still in this Team?", as far as Extend may act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// IAM answered with the member's directory entry.
    Active,
    /// IAM answered 404 for the member's entry, right after the same reader read its own entry in
    /// the Team, which proves the reader can read the Team's directory: a definite "not a member".
    Gone,
    /// IAM couldn't say: a 403 (the reader's scopes don't cover it, or IAM hides the entry), any
    /// other error, or no signed-in member to ask. Extend deletes nothing on this.
    Unknown,
}

impl Membership {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }
}

/// A proof for one delegated request to another application.
#[derive(Clone)]
pub struct OboProof {
    pub actor: Option<String>,
    pub org_id: Option<String>,
    pub access_proof: String,
    /// In a test environment, the other application's test secret and IAM test key.
    pub testing_app_secret: Option<String>,
    pub testing_iam_key: Option<String>,
}

impl std::fmt::Debug for OboProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OboProof")
            .field("actor", &self.actor)
            .field("org_id", &self.org_id)
            .finish_non_exhaustive()
    }
}

/// A verified IAM webhook, reduced to what Extend acts on.
#[derive(Debug, Clone)]
pub struct IamEvent {
    pub event_id: String,
    pub event_type: String,
    /// Public ids (`c:…`, `si:…`) named anywhere in the event.
    pub members: Vec<String>,
    /// Team handles named in the event.
    pub teams: Vec<String>,
    /// Memberships (`member`, `team`) the signed event itself reports as removed. Extend asks
    /// IAM first and uses these only when IAM can't answer (the event is newer than any event
    /// applied for its aggregate, see routes/webhook.rs).
    pub removed: Vec<(String, String)>,
    pub testing_environment_id: Option<Uuid>,
}

impl IamEvent {
    /// True for events that can end a member's access (logout, removal, revocation, deletion).
    pub fn ends_access(&self) -> bool {
        let t = self.event_type.to_ascii_lowercase();
        [
            "remov",
            "revok",
            "logout",
            "logged_out",
            "delet",
            "left",
            "suspend",
            "deactivat",
            "expire",
            "consent",
        ]
        .iter()
        .any(|w| t.contains(w))
    }
}

#[async_trait]
pub trait Iam: Send + Sync {
    fn app_id(&self) -> &str;
    async fn login(&self, slt: &str, idempotency_key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession>;
    async fn refresh(
        &self,
        refresh_token: &str,
        idempotency_key: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<AuthSession>;
    async fn logout(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<()>;
    /// Live authorization for a bearer token, optionally in one team.
    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal>;
    /// Whether a Carbon or Silicon is an active member of a team. IAM answers directory questions
    /// only for a member's own login, so `reader` is a signed-in member of that team whose access
    /// token asks (the Carbon granting access, or the member itself during a webhook re-check).
    async fn member_active(
        &self,
        team: &str,
        member_id: &str,
        reader: Option<&Principal>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<bool>;
    /// Whether `member_id` is still in `team`, read as `reader`, in the three states of
    /// [`Membership`]. Unlike [`Iam::member_active`] (which only ever refuses a grant), this answer
    /// can end access, so a 403, an error or a missing reader is `Unknown`, never "not a member".
    async fn membership(
        &self,
        team: &str,
        member_id: &str,
        reader: Option<&Principal>,
        sel: Option<&TestingSelection>,
    ) -> Membership;
    /// The Silicons in the principal's team (for choosing who gets access).
    async fn team_silicons(
        &self,
        principal: &Principal,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Vec<extend_protocol::model::TeamSilicon>>;
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
    async fn permission_start(
        &self,
        _p: &Principal,
        _input: crate::obo::PermissionInput,
        _key: Uuid,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<serde_json::Value> {
        Err(AppError::invalid(
            "Feature permissions require SDK IAM; local development does not simulate consent.",
        ))
    }
    async fn permission_complete(
        &self,
        _p: &Principal,
        _id: Uuid,
        _code: &str,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<serde_json::Value> {
        Err(AppError::invalid("Feature permissions require SDK IAM."))
    }
    async fn permissions(&self, _p: &Principal, _sel: Option<&TestingSelection>) -> AppResult<serde_json::Value> {
        Ok(serde_json::json!({"items":[]}))
    }
    async fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent>;
    /// Whose login `token` (an access or a refresh token) is, asked live before the token is
    /// revoked. `Ok(None)` when IAM no longer accepts the token (it expired or was already revoked);
    /// `Err` when IAM could not be asked.
    async fn identify_context(
        &self,
        token: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Option<(Member, String)>> {
        match self.authorize(token, None, sel).await {
            Ok(p) => Ok(p.team.clone().map(|org| (p.member, org))),
            Err(e) if refuses_token(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
    async fn identify(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<Option<Member>> {
        match self.authorize(token, None, sel).await {
            Ok(p) => Ok(Some(p.member)),
            Err(e) if refuses_token(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
    /// The SHA-256 (hex) of IAM's test webhook key for `environment_id`, when IAM disclosed it
    /// while confirming a test application secret. Extend stores it so signed test deliveries
    /// still find their environment after a restart.
    async fn test_webhook_digest(&self, _environment_id: Uuid) -> Option<String> {
        None
    }
    /// Teaches the webhook verifier a test webhook key digest Extend stored earlier.
    async fn remember_test_webhook_key(&self, _digest: &str, _environment_id: Uuid) {}
}

/// Whether an IAM answer means "this token is not (or no longer) accepted", as opposed to IAM
/// being unreachable.
pub fn refuses_token(e: &AppError) -> bool {
    matches!(
        e.code(),
        ErrorCode::TokenExpired | ErrorCode::NotATeamMember | ErrorCode::Unauthorized | ErrorCode::SltInvalid
    )
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
        entries
            .get(&key)
            .filter(|(at, _)| at.elapsed() < Self::TTL)
            .map(|(_, p)| p.clone())
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
    /// The most recently authorized principal in `env` that `pick` accepts. Its token may since
    /// have expired; callers treat an IAM refusal as "cannot tell".
    pub async fn latest(&self, env: Option<Uuid>, pick: impl Fn(&Principal) -> bool) -> Option<Principal> {
        let entries = self.entries.read().await;
        entries
            .iter()
            .filter(|((_, _, e), (_, p))| *e == env && pick(p))
            .max_by_key(|(_, (at, _))| *at)
            .map(|(_, (_, p))| p.clone())
    }
    /// Every token Extend saw `member` use in `env` recently (before a webhook forgets them).
    pub async fn tokens_of(&self, member: &str, env: Option<Uuid>) -> Vec<String> {
        let entries = self.entries.read().await;
        let mut out: Vec<String> = entries
            .iter()
            .filter(|((_, _, e), (_, p))| *e == env && p.id() == member)
            .map(|(_, (_, p))| p.token.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }
    pub async fn forget_token(&self, token: &str) {
        let digest = ids::secret_digest(token);
        self.entries.write().await.retain(|(d, _, _), _| d != &digest);
    }
}

fn member_from_public_id(id: &str) -> AppResult<Member> {
    let kind = ids::member_kind(id).ok_or_else(|| {
        AppError::new(
            ErrorCode::Unauthorized,
            format!("IAM returned an unusable member id {id:?}"),
        )
    })?;
    Ok(Member {
        kind,
        id: id.to_owned(),
        display_name: None,
    })
}

// ───────────────────────────── Official client ─────────────────────────────

pub struct SdkIam {
    delegations: Option<crate::obo::GrantStore>,
    sdk: SdkClient,
    app_id: String,
    app_secret: String,
    verifier: Option<silicon_iam_client::WebhookVerifier>,
    /// SHA-256 (hex) of each test environment's webhook key, from IAM's testing context, so a
    /// signed test delivery can be routed to its environment and never to production.
    test_webhook_keys: RwLock<HashMap<String, Uuid>>,
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
            .user_agent(concat!("silicon-extend/", env!("CARGO_PKG_VERSION")))
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
        Ok(Self {
            delegations: None,
            sdk,
            app_id: app_id.to_owned(),
            app_secret: app_secret.to_owned(),
            verifier,
            test_webhook_keys: RwLock::default(),
        })
    }

    pub fn with_delegations(mut self, pool: sqlx::PgPool, key: crate::obo::GrantKey) -> Self {
        self.delegations = Some(crate::obo::GrantStore::new(pool, key));
        self
    }
    fn delegations(&self) -> AppResult<&crate::obo::GrantStore> {
        self.delegations.as_ref().ok_or_else(|| {
            AppError::new(
                ErrorCode::ServiceUnavailable,
                "Feature approval storage is not configured.",
            )
        })
    }
    fn client(&self, sel: Option<&TestingSelection>) -> AppResult<SdkClient> {
        match sel {
            None => Ok(self.sdk.clone()),
            Some(s) => self
                .sdk
                .with_testing_application(&self.app_id, &s.secret)
                .map(|c| c.with_credential(Credential::application(&self.app_id, &s.secret)))
                .map_err(|e| {
                    AppError::new(
                        ErrorCode::TestingSecretInvalid,
                        format!("The test application secret was refused: {e}"),
                    )
                }),
        }
    }

    fn session(&self, tokens: models::OAuthTokenResponse, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let actor = tokens
            .actor
            .ok_or_else(|| AppError::unavailable("Silicon IAM", "login returned no actor"))?;
        let member = member_from_public_id(&actor.public_id)?;
        let org = tokens.org_id.clone().filter(|o| !o.is_empty()).ok_or_else(|| {
            AppError::new(
                ErrorCode::TokenExpired,
                "This login is not bound to an organization. Sign in again and choose one organization.",
            )
        })?;
        Ok(AuthSession {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            token_type: "Bearer".into(),
            expires_in: tokens.expires_in,
            member,
            teams: vec![org],
            testing_environment: sel.map(|s| extend_protocol::model::TestingEnvironment {
                environment_id: s.environment_id,
                name: s.name.clone(),
                state: "ready".into(),
                paired_devices: 0,
                device_limit: extend_protocol::TEST_DEVICE_LIMIT,
            }),
        })
    }
}

fn mutation(key: &str) -> AppResult<Mutation> {
    // IAM wants 16+ characters; hash the caller's key so any valid key maps to a stable one.
    let digest = ids::hex_lower(&Sha256::digest(format!("silicon-extend:{key}").as_bytes()));
    IdempotencyKey::parse(digest)
        .map(Mutation::with_key)
        .map_err(AppError::internal)
}

/// Maps an error from a login (SLT exchange): IAM's 400/401 means the SLT itself was refused.
fn login_error(err: silicon_iam_client::Error) -> AppError {
    match &err {
        silicon_iam_client::Error::Api(api) if api.status == 401 || api.status == 400 => AppError::new(
            ErrorCode::SltInvalid,
            format!("Silicon IAM refused the token: {} ({})", api.message, api.code),
        )
        .hint("Generate a new short-lived token with the IAM CLI and run `extend login <slt>` again."),
        _ => sdk_error(err),
    }
}

/// Maps an error from any other IAM call. A 401 here is about the credential Extend presented on
/// the member's behalf (their access token), never about an SLT.
fn sdk_error(err: silicon_iam_client::Error) -> AppError {
    match &err {
        silicon_iam_client::Error::Api(api) if api.status == 401 => AppError::new(
            ErrorCode::TokenExpired,
            format!(
                "Silicon IAM no longer accepts this login: {} ({})",
                api.message, api.code
            ),
        )
        .hint("Run `extend login status`; if it fails, get a new short-lived token and run `extend login <slt>`."),
        silicon_iam_client::Error::Api(api) if api.status == 403 => AppError::new(
            ErrorCode::NotATeamMember,
            format!("Silicon IAM refused access: {} ({})", api.message, api.code),
        ),
        silicon_iam_client::Error::RateLimited { .. } => AppError::new(
            ErrorCode::RateLimited,
            "Silicon IAM is rate limiting Extend; retry shortly.",
        ),
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
            return Err(AppError::new(
                ErrorCode::SltInvalid,
                "A member id works as a login only in a test environment.",
            )
            .hint("Generate a short-lived token with the IAM CLI and pass that instead."));
        }
        let tokens = self
            .client(sel)?
            .oauth()
            .login(&self.app_id, slt, &mutation(key)?)
            .await
            .map_err(login_error)?;
        let session = self.session(tokens, sel)?;
        Ok(session)
    }

    async fn refresh(&self, token: &str, key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let tokens = self
            .client(sel)?
            .oauth()
            .refresh(&self.app_id, token, &mutation(key)?)
            .await
            .map_err(|e| {
                let mut err = login_error(e);
                if err.code() == ErrorCode::SltInvalid {
                    err = AppError::new(
                        ErrorCode::TokenExpired,
                        "The refresh token is no longer accepted; the IAM session ended.",
                    )
                    .hint("Get a new short-lived token and run `extend login <slt>`.");
                }
                err
            })?;
        let session = self.session(tokens, sel)?;
        Ok(session)
    }

    async fn logout(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<()> {
        let hint = if token.starts_with("ort_") {
            Some("refresh_token".to_owned())
        } else {
            Some("access_token".to_owned())
        };
        let _ = hint;
        self.client(sel)?
            .oauth()
            .revoke(
                &models::OAuthRevocationRequest {
                    token: token.to_owned(),
                    token_type_hint: None,
                },
                &Mutation::new(),
            )
            .await
            .map_err(sdk_error)
    }

    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal> {
        let client = self.client(sel)?;
        let not_signed_in = || {
            AppError::new(
                ErrorCode::TokenExpired,
                "The access token is not active: it expired, was revoked, or belongs to another application.",
            )
            .hint("Run `extend login status`; if it fails, get a new short-lived token and run `extend login <slt>`.")
        };
        let all = client
            .oauth()
            .authorizations(token)
            .await
            .map_err(sdk_error)?
            .ok_or_else(not_signed_in)?;
        let env = sel.map(|s| s.environment_id);
        let all: Vec<_> = all
            .into_iter()
            .filter(|a| a.audience == self.app_id && a.testing_environment_id == env)
            .collect();
        if all.len() > 1 {
            return Err(AppError::new(
                ErrorCode::TokenExpired,
                "This older login spans organizations. Sign in again and choose one organization.",
            ));
        }
        let first = all.first().ok_or_else(|| {
            AppError::new(
                ErrorCode::NotATeamMember,
                "The token reaches no active team membership for Extend.",
            )
            .hint("Select a team when approving the login in Silicon IAM.")
        })?;
        let public_id = first.public_id.clone().ok_or_else(|| {
            AppError::new(
                ErrorCode::Unauthorized,
                "IAM did not disclose who this token belongs to.",
            )
            .hint("Approve the identity scope for Extend in Silicon IAM.")
        })?;
        let member = member_from_public_id(&public_id)?;
        let mut teams: Vec<String> = all.iter().map(|a| a.org_id.clone()).collect();
        teams.sort();
        teams.dedup();
        let (team, role) = match team {
            None => (Some(first.org_id.clone()), first.org_role.clone()),
            Some(t) => {
                let a = all.iter().find(|a| a.org_id == t).ok_or_else(|| {
                    AppError::new(
                        ErrorCode::NotATeamMember,
                        format!("{public_id} is not an active member of team {t:?} for Extend."),
                    )
                    .hint(format!("Teams this login reaches: {}.", teams.join(", ")))
                })?;
                (Some(t.to_owned()), a.org_role.clone())
            }
        };
        Ok(Principal {
            member,
            team,
            teams,
            role,
            token: token.to_owned(),
        })
    }

    async fn member_active(
        &self,
        team: &str,
        member_id: &str,
        reader: Option<&Principal>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<bool> {
        if ids::member_kind(member_id).is_none() {
            return Ok(false);
        }
        // The application credential alone cannot read a team's directory; IAM answers only for a
        // member's application access token, limited to the teams that member selected.
        let reader =
            reader.ok_or_else(|| AppError::unavailable("Silicon IAM", "no signed-in member of the team to ask"))?;
        let client = self.client(sel)?.with_credential(Credential::bearer(&reader.token));
        // Directory entries exist only for active, visible memberships; membership ids are `id[team]`.
        match client
            .members()
            .directory_member(team, &format!("{member_id}[{team}]"), Some("id,org"))
            .await
        {
            Ok(entry) => Ok(entry.id.as_deref() == Some(member_id) && entry.org.is_some_and(|o| o.id.as_str() == team)),
            Err(silicon_iam_client::Error::Api(api)) if api.status == 404 || api.status == 403 => Ok(false),
            Err(e) => Err(sdk_error(e)),
        }
    }

    async fn membership(
        &self,
        team: &str,
        member_id: &str,
        reader: Option<&Principal>,
        sel: Option<&TestingSelection>,
    ) -> Membership {
        let Some(reader) = reader else {
            return Membership::Unknown;
        };
        if ids::member_kind(member_id).is_none() {
            return Membership::Unknown;
        }
        let Ok(client) = self.client(sel) else {
            return Membership::Unknown;
        };
        let client = client.with_credential(Credential::bearer(&reader.token));
        // First the reader's own entry: only a reader that can read the Team's directory makes a
        // 404 for the member mean "not a member" rather than "hidden from this reader".
        match client
            .members()
            .directory_member(team, &format!("{}[{team}]", reader.id()), Some("id,org"))
            .await
        {
            Ok(entry) if entry.id.as_deref() == Some(reader.id()) => {}
            Ok(_) => return Membership::Unknown,
            Err(e) => {
                tracing::debug!(team, reader = reader.id(), error = %e, "IAM refused the reader's own directory entry");
                return Membership::Unknown;
            }
        }
        match client
            .members()
            .directory_member(team, &format!("{member_id}[{team}]"), Some("id,org"))
            .await
        {
            Ok(entry)
                if entry.id.as_deref() == Some(member_id)
                    && entry.org.as_ref().is_some_and(|o| o.id.as_str() == team) =>
            {
                Membership::Active
            }
            Ok(_) => Membership::Unknown,
            Err(silicon_iam_client::Error::Api(api)) if api.status == 404 => Membership::Gone,
            Err(e) => {
                tracing::debug!(team, member = member_id, reader = reader.id(), error = %e, "IAM couldn't say whether a member is active");
                Membership::Unknown
            }
        }
    }

    async fn team_silicons(
        &self,
        principal: &Principal,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Vec<extend_protocol::model::TeamSilicon>> {
        let team = principal.team()?.to_owned();
        let client = self.client(sel)?.with_credential(Credential::bearer(&principal.token));
        let mut out = Vec::new();
        let mut paging = silicon_iam_client::Paging::new();
        for _ in 0..20 {
            // IAM's field selector accepts name,id,role,org,tags,trust; display_name comes with name.
            let page = client
                .members()
                .directory(&team, Some("id,name,org"), &paging)
                .await
                .map_err(sdk_error)?;
            for m in page.items {
                if m.org.as_ref().is_some_and(|o| o.id.as_str() != team) {
                    continue;
                }
                if let Some(id) = m.id.filter(|i| i.starts_with("si:")) {
                    out.push(extend_protocol::model::TeamSilicon {
                        id,
                        display_name: m.display_name.or(m.name),
                        team: None,
                    });
                }
            }
            match page.page.next_cursor.filter(|_| page.page.has_more) {
                Some(c) => paging = silicon_iam_client::Paging::new().after(c),
                None => break,
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    async fn select_testing(&self, secret: &str) -> AppResult<(Uuid, String)> {
        let invalid = |why: String| {
            AppError::new(
                ErrorCode::TestingSecretInvalid,
                format!(
                    "Silicon IAM refused the test application secret: {why}. The secret is wrong or revoked, or \
                     its test environment is not open yet (IAM opens it only after Honeycomb confirms every \
                     service is ready). Nothing ran in production."
                ),
            )
            .hint(
                "Check the app_secret in Honeycomb. If the environment was just created or restored, wait until \
                 Honeycomb reports it ready and retry; or leave testing to use production.",
            )
        };
        if !ids::is_secret(ids::APP_SECRET_PREFIX, secret) {
            return Err(AppError::new(
                ErrorCode::TestingSecretInvalid,
                "The test application secret is malformed: it must be ask_ followed by 43 characters. Nothing ran in production.",
            )
            .hint("Copy the app_secret from Honeycomb again, or leave testing to use production."));
        }
        let client = self
            .sdk
            .with_testing_application(&self.app_id, secret)
            .map_err(|e| invalid(e.to_string()))?
            .with_credential(Credential::application(&self.app_id, secret));
        let ctx = client.applications().testing_context().await.map_err(|e| match e {
            silicon_iam_client::Error::Api(api) if api.status < 500 => {
                invalid(format!("{} ({})", api.message, api.code))
            }
            other => AppError::unavailable("Silicon IAM", other),
        })?;
        if ctx.application.app_id != self.app_id {
            return Err(invalid(format!(
                "it belongs to application {:?}, not Extend",
                ctx.application.app_id
            )));
        }
        if let Some(digest) = ctx.webhook_key_digest.as_deref() {
            self.test_webhook_keys
                .write()
                .await
                .insert(digest.to_ascii_lowercase(), ctx.environment_id);
        }
        let name = ctx
            .environment
            .map(|e| e.name)
            .unwrap_or_else(|| ctx.environment_id.to_string());
        let _ = &self.app_secret;
        Ok((ctx.environment_id, name))
    }

    async fn permission_start(
        &self,
        p: &Principal,
        input: crate::obo::PermissionInput,
        key: Uuid,
        sel: Option<&TestingSelection>,
    ) -> AppResult<serde_json::Value> {
        self.delegations()?.start(&self.client(sel)?, p, input, key, sel).await
    }
    async fn permission_complete(
        &self,
        p: &Principal,
        id: Uuid,
        code: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<serde_json::Value> {
        self.delegations()?.complete(&self.client(sel)?, p, id, code, sel).await
    }
    async fn permissions(&self, p: &Principal, sel: Option<&TestingSelection>) -> AppResult<serde_json::Value> {
        self.delegations()?.list(p, sel).await
    }
    async fn obo_proof(
        &self,
        principal: &Principal,
        audience: &str,
        endpoint_id: &str,
        _metadata: serde_json::Value,
        _method: &str,
        _body: &[u8],
        sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof> {
        self.delegations()?
            .access(&self.client(sel)?, principal, audience, endpoint_id, sel)
            .await
    }

    async fn identify_context(
        &self,
        token: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Option<(Member, String)>> {
        let hint = if token.starts_with("ort_") {
            models::TokenIntrospectionRequestTokenTypeHint::RefreshToken
        } else {
            models::TokenIntrospectionRequestTokenTypeHint::AccessToken
        };
        let inspected = self
            .client(sel)?
            .oauth()
            .introspect(
                &models::TokenIntrospectionRequest {
                    token: token.to_owned(),
                    token_type_hint: Some(hint),
                },
                None,
            )
            .await
            .map_err(sdk_error)?;
        if !inspected.active {
            return Ok(None);
        }
        // A token for another application says nothing about Extend's sessions.
        if inspected.client_id.as_deref() != Some(self.app_id.as_str()) {
            return Ok(None);
        }
        match (inspected.public_id, inspected.org_id) {
            (Some(id), Some(org)) => Ok(Some((member_from_public_id(&id)?, org))),
            _ => Ok(None),
        }
    }

    async fn identify(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<Option<Member>> {
        Ok(self.identify_context(token, sel).await?.map(|(member, _)| member))
    }

    async fn test_webhook_digest(&self, environment_id: Uuid) -> Option<String> {
        self.test_webhook_keys
            .read()
            .await
            .iter()
            .find(|(_, env)| **env == environment_id)
            .map(|(d, _)| d.clone())
    }

    async fn remember_test_webhook_key(&self, digest: &str, environment_id: Uuid) {
        self.test_webhook_keys
            .write()
            .await
            .insert(digest.to_ascii_lowercase(), environment_id);
    }

    async fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent> {
        let verifier = self
            .verifier
            .as_ref()
            .ok_or_else(|| AppError::new(ErrorCode::Unauthorized, "Extend has no IAM webhook secret configured."))?;
        let (event_id, event_type, raw) = match verifier.verify(headers, body) {
            Ok(verified) => {
                let event = verified.event();
                (
                    verified.event_id().to_string(),
                    event.event_type.clone(),
                    serde_json::to_value(event).unwrap_or_default(),
                )
            }
            // The verifier checks the headers, timestamp and HMAC over the exact bytes before it
            // parses, so InvalidPayload means an authenticated delivery whose shape this SDK does
            // not accept. IAM 4 sends public ids as aggregate ids (`"aggregate":{"id":"si:sous"}`),
            // which SDK 4.0.0 rejects because it expects a UUID; read those events ourselves.
            Err(silicon_iam_client::WebhookError::InvalidPayload) => {
                tracing::debug!("IAM webhook signature verified; event read by Extend (the SDK refused its shape)");
                authenticated_event(headers, body)?
            }
            Err(e) => {
                tracing::warn!(error = %e, "IAM webhook rejected");
                return Err(AppError::new(
                    ErrorCode::Unauthorized,
                    format!("IAM webhook signature rejected: {e}"),
                ));
            }
        };
        tracing::debug!(event = %raw, "verified IAM webhook");
        // A test delivery belongs to exactly one test environment; it must never touch production.
        let testing_environment_id = match testing_key(body) {
            None => None,
            Some(key) => {
                use subtle::ConstantTimeEq as _;
                let digest = ids::hex_lower(&Sha256::digest(key.as_bytes()));
                let known = self.test_webhook_keys.read().await;
                let found = known
                    .iter()
                    .find(|(d, _)| bool::from(d.as_bytes().ct_eq(digest.as_bytes())))
                    .map(|(_, env)| *env);
                Some(found.ok_or_else(|| {
                    AppError::new(
                        ErrorCode::ServiceUnavailable,
                        "This signed IAM test delivery carries a test webhook key Extend doesn't know yet (no test \
                         application secret for its environment has been confirmed with IAM since it was prepared), \
                         so Extend can't tell which test environment it belongs to and applied nothing.",
                    )
                    .hint("IAM retries the delivery; it routes once the environment's app_secret has been used with Extend.")
                })?)
            }
        };
        let mut members = Vec::new();
        let mut teams = Vec::new();
        collect_ids(&raw, &mut members, &mut teams);
        Ok(IamEvent {
            event_id,
            event_type,
            members,
            teams,
            removed: removed_memberships(&raw),
            testing_environment_id,
        })
    }
}

/// Reads an IAM event whose signature the official verifier already authenticated but whose shape
/// it refused. The body's event id must still match the signed routing header.
fn authenticated_event(headers: &http::HeaderMap, body: &[u8]) -> AppResult<(String, String, serde_json::Value)> {
    let bad = || AppError::new(ErrorCode::Unauthorized, "The IAM webhook body is not an IAM event.");
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| bad())?;
    // A test delivery wraps the event as {"test": {"testing_key", "metadata", "data"}}; the key is
    // never kept in the event Extend logs or stores.
    let (meta, data) = match value.get("test") {
        Some(t) => (
            t.get("metadata").cloned().ok_or_else(bad)?,
            t.get("data").cloned().ok_or_else(bad)?,
        ),
        None => (value.clone(), value.get("data").cloned().ok_or_else(bad)?),
    };
    let event_id = meta
        .get("event_id")
        .and_then(|v| v.as_str())
        .ok_or_else(bad)?
        .to_owned();
    let event_type = meta
        .get("event_type")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(bad)?
        .to_owned();
    let header = headers
        .get("x-silicon-iam-event-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(bad)?;
    if !header.eq_ignore_ascii_case(&event_id) || !data.is_object() {
        return Err(bad());
    }
    let raw = serde_json::json!({
        "event_id": event_id,
        "event_type": event_type,
        "occurred_at": meta.get("occurred_at"),
        "organization_id": meta.get("organization_id"),
        "aggregate": meta.get("aggregate"),
        "data": data,
    });
    Ok((event_id, event_type, raw))
}

/// The SHA-256 (hex) of the test environment key a signed test delivery carries, if it is one.
pub fn testing_key_digest(body: &[u8]) -> Option<String> {
    testing_key(body).map(|k| ids::hex_lower(&Sha256::digest(k.as_bytes())))
}

/// The test environment key a signed test delivery carries, if it is one.
fn testing_key(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("test")?.get("testing_key")?.as_str().map(str::to_owned)
}

/// Memberships a member event reports as removed: `data.current.members[].resource` with
/// `status: removed` (or `authorization: removed`) and a `member[team]` membership id.
fn removed_memberships(event: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let members = event
        .pointer("/data/current/members")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    for m in members {
        let resource = m.get("resource").cloned().unwrap_or_default();
        let removed = resource.get("status").and_then(|v| v.as_str()) == Some("removed")
            || m.get("authorization").and_then(|v| v.as_str()) == Some("removed");
        let Some(membership) = resource.get("membership_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let (true, Some(open)) = (removed, membership.find('['))
            && membership.ends_with(']')
        {
            let (id, team) = (&membership[..open], &membership[open + 1..membership.len() - 1]);
            if ids::member_kind(id).is_some() && !team.is_empty() {
                out.push((id.to_owned(), team.to_owned()));
            }
        }
    }
    out
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

/// How the local IAM answers directory reads made to decide on membership ([`Iam::membership`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderMode {
    /// Answers from its member list whoever asks (and without a reader): always definite.
    Open,
    /// Like a strict real IAM (`EXTEND_LOCAL_IAM_READERS=strict`): no reader, or a reader that
    /// isn't in the Team, can't tell; a Silicon reading a Carbon's entry gets 403 (unknown).
    Strict,
    /// Tests: IAM hides Carbons from Silicon readers with a 404, so a Silicon reader hears "gone"
    /// for an active Carbon. Everything else as `Strict`.
    HideCarbonsFromSilicons,
}

/// A development IAM. Members come from `EXTEND_LOCAL_MEMBERS` (or anyone, when that is empty);
/// the SLT is the member id, optionally with `@team+team`. Refused in production.
pub struct LocalIam {
    app_id: String,
    members: RwLock<HashMap<String, Vec<String>>>,
    open: bool,
    tokens: RwLock<HashMap<String, LocalToken>>,
    pool: sqlx::PgPool,
    readers: std::sync::RwLock<ReaderMode>,
}

impl LocalIam {
    pub fn new(members: Vec<(String, Vec<String>)>, pool: sqlx::PgPool) -> Self {
        let open = members.is_empty();
        Self {
            app_id: "extend".into(),
            members: RwLock::new(members.into_iter().collect()),
            open,
            tokens: RwLock::new(HashMap::new()),
            pool,
            readers: std::sync::RwLock::new(ReaderMode::Open),
        }
    }

    /// Changes how directory reads for [`Iam::membership`] are answered.
    pub fn set_reader_mode(&self, mode: ReaderMode) {
        *self.readers.write().unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
    }

    fn reader_mode(&self) -> ReaderMode {
        *self.readers.read().unwrap_or_else(std::sync::PoisonError::into_inner)
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

    async fn issue(
        &self,
        member: &str,
        teams: Vec<String>,
        env: Option<Uuid>,
        sel: Option<&TestingSelection>,
    ) -> AuthSession {
        let access = Self::new_token("oat_");
        let refresh = Self::new_token("ort_");
        let mut t = self.tokens.write().await;
        t.insert(
            access.clone(),
            LocalToken {
                member: member.to_owned(),
                teams: teams.clone(),
                env,
                refresh: false,
            },
        );
        t.insert(
            refresh.clone(),
            LocalToken {
                member: member.to_owned(),
                teams: teams.clone(),
                env,
                refresh: true,
            },
        );
        AuthSession {
            access_token: access,
            refresh_token: refresh,
            token_type: "Bearer".into(),
            expires_in: 3600,
            member: Member {
                kind: ids::member_kind(member).unwrap_or(MemberKind::Carbon),
                id: member.to_owned(),
                display_name: None,
            },
            teams,
            testing_environment: sel.map(|s| extend_protocol::model::TestingEnvironment {
                environment_id: s.environment_id,
                name: s.name.clone(),
                state: "ready".into(),
                paired_devices: 0,
                device_limit: extend_protocol::TEST_DEVICE_LIMIT,
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
            return Err(AppError::new(
                ErrorCode::SltInvalid,
                format!("Local IAM expects a member id like c:alice or si:chef, got {slt:?}."),
            ));
        }
        let known = self.teams_of(id).await.ok_or_else(|| {
            AppError::new(
                ErrorCode::SltInvalid,
                format!("{id} is not a member local IAM knows (EXTEND_LOCAL_MEMBERS)."),
            )
        })?;
        let teams = match teams_hint {
            Some(h) => h
                .split('+')
                .filter(|t| known.iter().any(|k| k == t))
                .map(str::to_owned)
                .collect(),
            None => known,
        };
        if teams.is_empty() {
            return Err(AppError::new(
                ErrorCode::NotATeamMember,
                format!("{id} is not in any of those teams."),
            ));
        }
        Ok(self.issue(id, teams, sel.map(|s| s.environment_id), sel).await)
    }

    async fn refresh(&self, token: &str, _key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        let old = self
            .tokens
            .write()
            .await
            .remove(token)
            .filter(|t| t.refresh)
            .ok_or_else(|| {
                AppError::new(ErrorCode::TokenExpired, "The refresh token is no longer accepted.")
                    .hint("Run `extend login <slt>` again.")
            })?;
        Ok(self.issue(&old.member, old.teams, old.env, sel).await)
    }

    async fn logout(&self, token: &str, _sel: Option<&TestingSelection>) -> AppResult<()> {
        let mut t = self.tokens.write().await;
        if let Some(found) = t.remove(token)
            && found.refresh
        {
            // Only this world's logins: production and each test environment are separate IAM
            // planes, so signing out of a test environment leaves the production login alone.
            t.retain(|_, x| x.member != found.member || x.env != found.env || x.teams != found.teams);
        }
        Ok(())
    }

    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal> {
        let found = self
            .tokens
            .read()
            .await
            .get(token)
            .cloned()
            .filter(|t| !t.refresh)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::TokenExpired,
                    "The access token is not active: it expired, was revoked, or never existed.",
                )
                .hint("Run `extend login <slt>` again.")
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
            return Err(AppError::new(
                ErrorCode::NotATeamMember,
                format!("{} is no longer an active member of any team.", found.member),
            ));
        }
        let team = match team {
            None => None,
            Some(t) if teams.iter().any(|x| x == t) => Some(t.to_owned()),
            Some(t) => {
                return Err(AppError::new(
                    ErrorCode::NotATeamMember,
                    format!("{} is not an active member of team {t:?}.", found.member),
                )
                .hint(format!("Teams this login reaches: {}.", teams.join(", "))));
            }
        };
        Ok(Principal {
            member: Member {
                kind: ids::member_kind(&found.member).unwrap_or(MemberKind::Carbon),
                id: found.member,
                display_name: None,
            },
            team,
            teams,
            role: Some("member".into()),
            token: token.to_owned(),
        })
    }

    async fn member_active(
        &self,
        team: &str,
        member_id: &str,
        _reader: Option<&Principal>,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<bool> {
        Ok(ids::member_kind(member_id).is_some()
            && self
                .teams_of(member_id)
                .await
                .is_some_and(|t| t.iter().any(|x| x == team)))
    }

    async fn membership(
        &self,
        team: &str,
        member_id: &str,
        reader: Option<&Principal>,
        _sel: Option<&TestingSelection>,
    ) -> Membership {
        let Some(kind) = ids::member_kind(member_id) else {
            return Membership::Unknown;
        };
        let listed = |teams: Option<Vec<String>>| teams.is_some_and(|t| t.iter().any(|x| x == team));
        let mode = self.reader_mode();
        if mode != ReaderMode::Open {
            let Some(reader) = reader else {
                return Membership::Unknown;
            };
            // The reader's own entry first, as SdkIam reads it.
            if !listed(self.teams_of(reader.id()).await) {
                return Membership::Unknown;
            }
            if reader.is_silicon() && kind == MemberKind::Carbon {
                match mode {
                    ReaderMode::HideCarbonsFromSilicons => return Membership::Gone,
                    _ => return Membership::Unknown,
                }
            }
        }
        if listed(self.teams_of(member_id).await) {
            Membership::Active
        } else {
            Membership::Gone
        }
    }

    async fn team_silicons(
        &self,
        principal: &Principal,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<Vec<extend_protocol::model::TeamSilicon>> {
        let team = principal.team()?.to_owned();
        let members = self.members.read().await;
        let mut out: Vec<_> = members
            .iter()
            .filter(|(id, teams)| id.starts_with("si:") && teams.contains(&team))
            .map(|(id, _)| extend_protocol::model::TeamSilicon {
                id: id.clone(),
                display_name: None,
                team: None,
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    async fn select_testing(&self, secret: &str) -> AppResult<(Uuid, String)> {
        let invalid = || {
            AppError::new(
                ErrorCode::TestingSecretInvalid,
                "The local IAM stand-in knows no open test application with this secret: it is wrong, or its \
                 environment isn't open yet (register it with POST /dev/iam/test-apps). Nothing ran in production.",
            )
            .hint("Check the secret. If the environment was just created or restored, wait until it is ready; or leave testing to use production.")
        };
        if !ids::is_secret(ids::APP_SECRET_PREFIX, secret) {
            return Err(invalid());
        }
        // Like IAM, refuse a test application that isn't open (active) right now: IAM keeps a
        // Honeycomb environment's applications suspended until Honeycomb confirms every service is
        // ready, and then answers exactly as it does for a wrong secret.
        let row: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT e.environment_id, e.name, e.state FROM extend_global.local_test_apps a
             JOIN extend_global.test_environments e USING (environment_id) WHERE a.secret_digest = $1 AND a.active",
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
        Ok(OboProof {
            actor: Some(principal.id().to_owned()),
            org_id: principal.team.clone(),
            access_proof: format!("obo_local:{audience}:{endpoint_id}:{}", principal.id()),
            testing_app_secret: None,
            testing_iam_key: None,
        })
    }

    async fn identify_context(
        &self,
        token: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Option<(Member, String)>> {
        Ok(self
            .tokens
            .read()
            .await
            .get(token)
            .filter(|t| t.env == sel.map(|s| s.environment_id))
            .and_then(|t| {
                t.teams.first().map(|org| {
                    (
                        Member {
                            kind: ids::member_kind(&t.member).unwrap_or(MemberKind::Carbon),
                            id: t.member.clone(),
                            display_name: None,
                        },
                        org.clone(),
                    )
                })
            }))
    }

    async fn identify(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<Option<Member>> {
        Ok(self
            .tokens
            .read()
            .await
            .get(token)
            .filter(|t| t.env == sel.map(|s| s.environment_id))
            .map(|t| Member {
                kind: ids::member_kind(&t.member).unwrap_or(MemberKind::Carbon),
                id: t.member.clone(),
                display_name: None,
            }))
    }

    async fn verify_webhook(&self, _headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent> {
        // Local events are plain JSON: {"event_id","event_type","members":[...],"teams":[...]},
        // optionally with "removed": [["si:x","acme"]] and "aggregate": {"id","version"}.
        let v: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| AppError::invalid(format!("local IAM event is not JSON: {e}")))?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
        let list = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        Ok(IamEvent {
            event_id: s("event_id"),
            event_type: s("event_type"),
            members: list("members"),
            teams: list("teams"),
            removed: v
                .get("removed")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|pair| Some((pair.get(0)?.as_str()?.to_owned(), pair.get(1)?.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
            testing_environment_id: v
                .get("environment_id")
                .and_then(|x| x.as_str())
                .and_then(|x| x.parse().ok()),
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

    /// The shape IAM 4 actually delivered for `DELETE /organizations/acme/silicons/si:sous`
    /// (captured from a real IAM in e2e/real-iam), minus volatile ids.
    fn silicon_removed() -> serde_json::Value {
        serde_json::json!({
            "spec_version": "1.0",
            "event_id": "01a0db14-39a6-7f20-bb3d-7c3da59e1a4e",
            "event_type": "organization.silicon.removed.v1",
            "occurred_at": "2026-09-26T00:18:51.000000Z",
            "organization_id": "3f1b1a52-7a55-4c55-8f4e-0b1d9d7a5c01",
            "aggregate": {"type": "silicon", "id": "si:sous", "version": 2},
            "data": {"changed_fields": ["membership.status"], "current": {"members": [{"authorization": "removed",
                "resource": {"id": "b01e3da0-cd83-459a-b65d-4729051f92fb", "membership_id": "si:sous[acme]",
                    "principal_type": "silicon", "status": "removed", "type": "organization_membership", "version": 2}}]}}
        })
    }

    fn event_headers(id: &str) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        h.insert("x-silicon-iam-event-id", id.parse().unwrap());
        h
    }

    #[test]
    fn reads_signed_events_the_sdk_refuses() {
        let body = serde_json::to_vec(&silicon_removed()).unwrap();
        let (id, kind, raw) =
            authenticated_event(&event_headers("01a0db14-39a6-7f20-bb3d-7c3da59e1a4e"), &body).unwrap();
        assert_eq!(id, "01a0db14-39a6-7f20-bb3d-7c3da59e1a4e");
        assert_eq!(kind, "organization.silicon.removed.v1");
        assert_eq!(
            removed_memberships(&raw),
            vec![("si:sous".to_owned(), "acme".to_owned())]
        );
        let (mut m, mut t) = (vec![], vec![]);
        collect_ids(&raw, &mut m, &mut t);
        assert_eq!((m, t), (vec!["si:sous".to_owned()], vec!["acme".to_owned()]));
        // The body's event id must match the signed routing header.
        assert!(authenticated_event(&event_headers("01a0db14-0000-7000-8000-000000000000"), &body).is_err());
        assert!(testing_key(&body).is_none());
    }

    #[test]
    fn reads_test_envelopes_without_keeping_the_key() {
        let e = silicon_removed();
        let wrapped = serde_json::json!({"test": {"testing_key": "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6", "metadata": {
            "spec_version": "1.0", "event_id": e["event_id"], "event_type": e["event_type"], "occurred_at": e["occurred_at"],
            "organization_id": e["organization_id"], "aggregate": e["aggregate"], "environment_id": null, "generation": 1},
            "data": e["data"]}});
        let body = serde_json::to_vec(&wrapped).unwrap();
        assert_eq!(testing_key(&body).as_deref(), Some("A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6"));
        let (_, _, raw) = authenticated_event(&event_headers("01a0db14-39a6-7f20-bb3d-7c3da59e1a4e"), &body).unwrap();
        assert!(!raw.to_string().contains("A1b2C3d4"));
        assert_eq!(removed_memberships(&raw).len(), 1);
    }

    #[test]
    fn active_members_are_not_removed() {
        let mut e = silicon_removed();
        e["data"]["current"]["members"][0]["authorization"] = "active".into();
        e["data"]["current"]["members"][0]["resource"]["status"] = "active".into();
        assert!(removed_memberships(&e).is_empty());
    }

    fn test_selection() -> TestingSelection {
        TestingSelection {
            environment_id: Uuid::new_v4(),
            name: "checkout".into(),
            secret: ids::new_secret(ids::APP_SECRET_PREFIX),
        }
    }

    /// A LocalIam that never touches its (lazy) pool in these tests.
    fn local(members: &[(&str, &[&str])]) -> LocalIam {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .unwrap();
        LocalIam::new(
            members
                .iter()
                .map(|(id, teams)| ((*id).to_owned(), teams.iter().map(|t| (*t).to_owned()).collect()))
                .collect(),
            pool,
        )
    }

    #[tokio::test]
    async fn test_plane_login_refuses_unknown_and_inactive_member_ids() {
        let iam = local(&[("c:alice", &["acme"]), ("si:chef", &["acme"])]);
        let sel = test_selection();
        // A known, active test member signs in with just their id.
        let s = iam.login("si:chef", "k1", Some(&sel)).await.unwrap();
        assert_eq!(s.member.id, "si:chef");
        assert_eq!(s.testing_environment.unwrap().environment_id, sel.environment_id);
        // Unknown ids are refused.
        let e = iam.login("c:nobody", "k2", Some(&sel)).await.unwrap_err();
        assert_eq!(e.code(), ErrorCode::SltInvalid);
        assert!(e.0.message.contains("c:nobody"), "{}", e.0.message);
        // So is a member who is no longer active in any team, or who was removed entirely.
        iam.set_member("si:chef", Some(vec![])).await;
        assert_eq!(
            iam.login("si:chef", "k3", Some(&sel)).await.unwrap_err().code(),
            ErrorCode::NotATeamMember
        );
        iam.set_member("c:alice", None).await;
        assert_eq!(
            iam.login("c:alice", "k4", Some(&sel)).await.unwrap_err().code(),
            ErrorCode::SltInvalid
        );
        // Anything that isn't a member id is refused too.
        assert_eq!(
            iam.login("alice", "k5", Some(&sel)).await.unwrap_err().code(),
            ErrorCode::SltInvalid
        );
    }

    #[tokio::test]
    async fn the_member_id_shortcut_never_reaches_production_iam() {
        let sdk = SdkClient::builder("http://127.0.0.1:9")
            .unwrap()
            .credential(Credential::application("extend", "ask_unused"))
            .auto_update(false)
            .build()
            .unwrap();
        let iam = SdkIam {
            delegations: None,
            sdk,
            app_id: "extend".into(),
            app_secret: "ask_unused".into(),
            verifier: None,
            test_webhook_keys: RwLock::default(),
        };
        // Production: refused before IAM is asked (nothing listens on :9, so asking would fail
        // with service_unavailable instead).
        for id in ["c:alice", "si:chef"] {
            let e = iam.login(id, "key", None).await.unwrap_err();
            assert_eq!(e.code(), ErrorCode::SltInvalid, "{id}");
            assert!(e.0.message.contains("only in a test environment"), "{}", e.0.message);
        }
        // In a test environment the id goes to IAM, which decides (here: unreachable).
        let e = iam.login("c:alice", "key", Some(&test_selection())).await.unwrap_err();
        assert_ne!(e.code(), ErrorCode::SltInvalid);
    }

    #[tokio::test]
    async fn local_identify_names_the_owner_of_a_refresh_token_in_its_world_only() {
        let iam = local(&[("si:chef", &["acme"])]);
        let sel = test_selection();
        let s = iam.login("si:chef", "k", Some(&sel)).await.unwrap();
        assert_eq!(
            iam.identify(&s.refresh_token, Some(&sel)).await.unwrap().unwrap().id,
            "si:chef"
        );
        assert_eq!(
            iam.identify(&s.access_token, Some(&sel)).await.unwrap().unwrap().id,
            "si:chef"
        );
        assert!(iam.identify(&s.refresh_token, None).await.unwrap().is_none());
        iam.logout(&s.refresh_token, Some(&sel)).await.unwrap();
        assert!(iam.identify(&s.refresh_token, Some(&sel)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn local_logout_in_a_test_environment_leaves_the_production_login_alone() {
        let iam = local(&[("c:alice", &["acme"])]);
        let sel = test_selection();
        let prod = iam.login("c:alice", "k1", None).await.unwrap();
        let test = iam.login("c:alice", "k2", Some(&sel)).await.unwrap();
        iam.logout(&test.refresh_token, Some(&sel)).await.unwrap();
        // The test login is gone…
        assert!(iam.authorize(&test.access_token, None, Some(&sel)).await.is_err());
        // …and the same Carbon's production login still works and still refreshes.
        assert_eq!(
            iam.authorize(&prod.access_token, None, None).await.unwrap().member.id,
            "c:alice"
        );
        iam.refresh(&prod.refresh_token, "k3", None).await.unwrap();
    }

    #[test]
    fn access_ending_events() {
        let e = |t: &str| IamEvent {
            event_id: "1".into(),
            event_type: t.into(),
            members: vec![],
            teams: vec![],
            removed: vec![],
            testing_environment_id: None,
        };
        assert!(e("organization.member.removed.v1").ends_access());
        assert!(e("session.revoked.v1").ends_access());
        assert!(!e("organization.updated.v1").ends_access());
    }
}
