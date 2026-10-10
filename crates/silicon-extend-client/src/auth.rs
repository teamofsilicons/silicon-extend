//! Signing in to Extend with Silicon Accounts, the way Extend's own tools do it: as a *public
//! client*, with `client_id` = `extend` and no secret (a secret shipped inside a CLI isn't secret).
//!
//! - **Carbons** use the device flow. [`SignIn::start_device`] returns a code to show; the Carbon
//!   approves it on the Silicon Accounts site, on any device; [`SignIn::wait_for_device`] polls,
//!   honouring `interval` and `slow_down`, and gives up when the code expires (10 minutes).
//! - **Silicons** never see a page. They mint a short-lived token (`slt_…`, single use, 2 minutes,
//!   for Extend only) with `silicon-accounts login --app extend -q` and hand it over;
//!   [`SignIn::exchange_slt`] exchanges it. Every refusal says exactly why ([`AuthError::code`]:
//!   `slt_already_used`, `slt_expired`, `slt_wrong_app`, …), and the token is never echoed.
//! - [`SignIn::refresh`] rotates the refresh token. The old one stops working at once, and
//!   presenting it again revokes the whole sign-in, so refresh once per token (single-flight, under
//!   a lock if several processes share it) and store the new pair before using it.
//! - [`SignIn::revoke`] ends the sign-in (RFC 7009).
//!
//! The access token is a Silicon Accounts JWT with `aud` = `extend`, valid for 30 minutes: pass it
//! to [`crate::Client::authed`]. Where the tokens are kept is the caller's business (the `extend`
//! CLI keeps them in `$SILICON_HOME/.extend/auth.json`, mode 0600).
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use silicon_extend_client::auth::SignIn;
//! let sign_in = SignIn::new("https://accounts.teamofsilicons.com")?;
//! // A Silicon: the token comes from `silicon-accounts login --app extend -q`.
//! let tokens = sign_in.exchange_slt("slt_…").await?;
//! let account = tokens.account.as_ref().expect("token responses carry the account");
//! println!("signed in as {} ({})", account.id, account.uuid);
//! # Ok(()) }
//! ```

use std::fmt;
use std::time::Duration;

use extend_protocol::model::MemberKind;
use serde::{Deserialize, Serialize};
use silicon_accounts_client::{AccountKind, AccountsClient, DevicePoll as AccountsPoll, TokenResponse};
use time::OffsetDateTime;

/// Production Silicon Accounts.
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
/// The grant a short-lived token is exchanged with.
pub const SLT_GRANT: &str = "urn:silicon:params:oauth:grant-type:slt";
/// What a Silicon runs to get a short-lived token for Extend.
pub const MINT_SLT: &str = "silicon-accounts login --app extend -q";
/// The whole Silicon sign-in, as one line to copy.
pub const SILICON_SIGN_IN: &str = "silicon-accounts login --app extend -q | extend login --slt-stdin";
/// How long a request to Silicon Accounts may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A token or code that must never end up in a log by accident: `Debug` shows only its public
/// prefix (`sar_…`). [`Secret::expose`] returns the value where it has to leave the program.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// The value. Only where it must leave the program (a header, a form, the saved sign-in).
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
    /// The public prefix (`slt_`, `sar_`, `sad_`), if the value has one.
    pub fn prefix(&self) -> Option<&str> {
        let end = self.0.find('_')?;
        (end <= 6).then(|| &self.0[..=end])
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.prefix() {
            Some(p) => write!(f, "Secret({p}…)"),
            None => f.write_str("Secret(…)"),
        }
    }
}

/// A Silicon's custodian, as Silicon Accounts names it in a token response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Custodian {
    pub uuid: String,
    pub id: String,
}

/// Who signed in: the account in Silicon Accounts' token response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedInAccount {
    /// The permanent uuid (short and case-sensitive, like `zQo`). Key on this.
    pub uuid: String,
    /// The current public id (`c:ada`, `si:scout`). It can change.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: MemberKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian (always there for a Silicon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<Custodian>,
}

/// Extend's tokens for one sign-in.
#[derive(Clone)]
pub struct Tokens {
    /// Silicon Accounts access token for Extend (`aud` = `extend`), for `Authorization: Bearer`.
    pub access_token: Secret,
    /// The rotating refresh token (`sar_…`).
    pub refresh_token: Option<Secret>,
    /// Seconds the access token lives (1800).
    pub expires_in: u64,
    /// When the access token expires, counted from when the answer arrived.
    pub expires_at: OffsetDateTime,
    /// When the sign-in itself ends, however often it is refreshed.
    pub refresh_expires_at: Option<OffsetDateTime>,
    /// Space-separated scopes Silicon Accounts granted.
    pub scope: Option<String>,
    pub account: Option<SignedInAccount>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("expires_at", &self.expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    fn from_response(r: TokenResponse) -> Self {
        let now = OffsetDateTime::now_utc();
        let account = r.account.map(|a| SignedInAccount {
            uuid: a.uuid,
            id: a.id,
            kind: match a.kind {
                AccountKind::Silicon => MemberKind::Silicon,
                AccountKind::Carbon => MemberKind::Carbon,
            },
            display_name: Some(a.display_name).filter(|n| !n.trim().is_empty()),
            pfp_url: Some(a.pfp_url).filter(|u| !u.trim().is_empty()),
            custodian: a.custodian.map(|c| Custodian { uuid: c.uuid, id: c.id }),
        });
        Self {
            expires_at: now + time::Duration::seconds(i64::try_from(r.expires_in).unwrap_or(1800)),
            access_token: Secret(r.access_token.into_inner()),
            refresh_token: r.refresh_token.map(|t| Secret(t.into_inner())),
            expires_in: r.expires_in,
            refresh_expires_at: r.refresh_token_expires_at,
            scope: r.scope,
            account,
        }
    }
}

/// A device sign-in in progress: show `user_code` and `verification_uri` to the Carbon.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    /// What the tool polls with; never shown.
    pub device_code: Secret,
    /// The code the Carbon checks on the approval page, like `MVHB-KQAW`.
    pub user_code: String,
    /// Where the Carbon approves.
    pub verification_uri: String,
    /// The same page with the code filled in.
    pub verification_uri_complete: Option<String>,
    /// Seconds until the code expires (600).
    pub expires_in: u64,
    /// When it expires.
    pub expires_at: OffsetDateTime,
    /// Seconds between polls (5).
    pub interval: u64,
}

/// One poll of a device sign-in.
#[derive(Debug)]
pub enum DevicePoll {
    /// Not decided yet: poll again after the interval.
    Pending,
    /// Polled too fast: add 5 seconds to the interval.
    SlowDown,
    /// The Carbon denied it.
    Denied,
    /// The code expired before anyone approved it.
    Expired,
    /// Approved.
    Tokens(Box<Tokens>),
}

/// What [`SignIn::wait_for_device`] reports while it waits.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceProgress {
    /// Still waiting for the Carbon; the next poll is in `interval`.
    Waiting { interval: Duration },
    /// Silicon Accounts asked to poll less often: the interval is now `interval`.
    SlowedDown { interval: Duration },
    /// A poll failed for a reason that usually passes (the network, a busy server); it tries again.
    Retrying { message: String, retry_in: Duration },
}

/// What [`SignIn::revoke`] answered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Revoked {
    /// False when Silicon Accounts didn't know the token as Extend's (already revoked, or not one).
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub message: Option<String>,
}

/// Why signing in, refreshing or revoking failed: a stable [`code`](Self::code), what happened and
/// why ([`message`](Self::message), Silicon Accounts' own words where it gave any) and what to do
/// next ([`hint`](Self::hint)). Never contains a token.
///
/// Codes: `slt_already_used`, `slt_expired`, `slt_wrong_app`, `slt_unknown`, `slt_wrong_kind`,
/// `slt_sign_in_ended`, `slt_refused` (other refusals), `not_an_slt` (refused before sending),
/// `sign_in_ended` (a refresh was refused: the sign-in is over), `device_denied`,
/// `device_expired`, `public_client_off`, `device_flow_off`, `unknown_app`, `rate_limited`,
/// `accounts_unreachable`, `accounts_error`, `unexpected_response`, `invalid_accounts_url`,
/// `invalid_input`, or Silicon Accounts' own error code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AuthError {
    pub code: String,
    pub message: String,
    pub hint: String,
    /// The HTTP status, when Silicon Accounts answered.
    pub status: Option<u16>,
    /// Silicon Accounts' request id, to quote in a bug report.
    pub request_id: Option<String>,
    /// A failure that usually passes (the network, a busy server): retrying may work.
    pub transient: bool,
}

impl AuthError {
    fn new(code: &str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: hint.into(),
            status: None,
            request_id: None,
            transient: false,
        }
    }

    fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn is(&self, code: &str) -> bool {
        self.code == code
    }

    /// A short-lived token was refused (any `slt_*` code or `not_an_slt`): mint a fresh one.
    pub fn is_slt_refusal(&self) -> bool {
        self.code.starts_with("slt_") || self.code == "not_an_slt"
    }

    /// The sign-in is over: sign in again. A refresh was refused, or the account is gone.
    pub fn sign_in_ended(&self) -> bool {
        self.code == "sign_in_ended"
    }
}

/// Plain http is allowed only for this machine, where nothing travels over a network.
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url.trim()) else {
        return false;
    };
    match u.host() {
        Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `url` without a trailing slash, if it is https, or http for this machine, with no query.
pub fn check_url(url: &str, what: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    let parsed = url::Url::parse(trimmed).map_err(|e| format!("{what} {trimmed:?} is not a URL ({e})"))?;
    match parsed.scheme() {
        "https" => {}
        "http" if is_loopback_url(trimmed) => {}
        "http" => {
            return Err(format!(
                "{what} {trimmed} uses plain http for a host that is not this machine, so tokens would travel unencrypted; use https (http is allowed only for localhost, 127.0.0.1 and [::1])"
            ));
        }
        other => return Err(format!("{what} {trimmed} uses the {other} scheme; use https")),
    }
    if parsed.host().is_none() || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!(
            "{what} {trimmed} must be an origin like https://example.com, without a query or fragment"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!("{what} {trimmed} must not contain a user name or password"));
    }
    Ok(trimmed.to_owned())
}

/// Signs in to Extend (or another app's own tool, with [`SignIn::for_app`]) at one Silicon Accounts.
#[derive(Debug, Clone)]
pub struct SignIn {
    accounts: AccountsClient,
    http: reqwest::Client,
    base: String,
    app_id: String,
}

impl SignIn {
    /// Extend's sign-in at the Silicon Accounts at `accounts_url` (https, or http for this machine).
    pub fn new(accounts_url: &str) -> Result<Self, AuthError> {
        Self::for_app(accounts_url, extend_protocol::APP_ID, None)
    }

    /// The sign-in of the app `app_id` (its `device_flow` or `public_client` must be on), with a
    /// product token for the User-Agent (`extend-cli/4.0.0`).
    pub fn for_app(accounts_url: &str, app_id: &str, user_agent: Option<&str>) -> Result<Self, AuthError> {
        let base = check_url(accounts_url, "The Silicon Accounts URL").map_err(|m| {
            AuthError::new(
                "invalid_accounts_url",
                m,
                format!("Set ACCOUNTS_URL to the Silicon Accounts to sign in at (production: {DEFAULT_ACCOUNTS_URL})."),
            )
        })?;
        let product = user_agent.map_or_else(
            || concat!("silicon-extend-client/", env!("CARGO_PKG_VERSION")).to_owned(),
            |ua| format!("{ua} silicon-extend-client/{}", env!("CARGO_PKG_VERSION")),
        );
        let accounts = AccountsClient::builder()
            .base_url(&base)
            .user_agent(product.clone())
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| {
                AuthError::new(
                    "invalid_accounts_url",
                    e.message(),
                    format!(
                        "Set ACCOUNTS_URL to the Silicon Accounts to sign in at (production: {DEFAULT_ACCOUNTS_URL})."
                    ),
                )
            })?;
        let http = reqwest::Client::builder()
            .user_agent(product)
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| {
                AuthError::new(
                    "invalid_input",
                    format!("Could not set up HTTP: {e}"),
                    "Check the system's TLS certificates.",
                )
            })?;
        Ok(Self {
            accounts,
            http,
            base,
            app_id: app_id.trim().to_owned(),
        })
    }

    pub fn accounts_url(&self) -> &str {
        &self.base
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// `POST /v1/device/authorize` with `client_id` alone: starts a Carbon's sign-in. `label` names
    /// this sign-in on the approval page and in the Carbon's list of sign-ins (≤ 100 characters).
    pub async fn start_device(&self, label: Option<&str>) -> Result<DeviceCode, AuthError> {
        let label = label.map(|l| l.chars().take(100).collect::<String>());
        let d = self
            .accounts
            .app_device_authorize(&self.app_id, None, label.as_deref())
            .await
            .map_err(|e| self.accounts_error(e, Step::DeviceStart))?;
        let expires_in = d.expires_in.max(1);
        Ok(DeviceCode {
            device_code: Secret(d.device_code.into_inner()),
            user_code: d.user_code,
            verification_uri: d.verification_uri,
            verification_uri_complete: d.verification_uri_complete,
            expires_in,
            expires_at: OffsetDateTime::now_utc() + time::Duration::seconds(i64::try_from(expires_in).unwrap_or(600)),
            interval: d.interval.max(1),
        })
    }

    /// Polls a device sign-in once.
    pub async fn poll_device(&self, code: &DeviceCode) -> Result<DevicePoll, AuthError> {
        match self
            .accounts
            .app_device_poll(&self.app_id, code.device_code.expose())
            .await
        {
            Ok(AccountsPoll::Tokens(t)) => Ok(DevicePoll::Tokens(Box::new(Tokens::from_response(*t)))),
            Ok(AccountsPoll::Pending) => Ok(DevicePoll::Pending),
            Ok(AccountsPoll::SlowDown) => Ok(DevicePoll::SlowDown),
            Ok(AccountsPoll::Denied) => Ok(DevicePoll::Denied),
            Ok(AccountsPoll::Expired) => Ok(DevicePoll::Expired),
            // A later silicon-accounts-client may add states; until this crate knows one, wait.
            Ok(_) => Ok(DevicePoll::Pending),
            Err(e) => Err(self.accounts_error(e, Step::DevicePoll)),
        }
    }

    /// Polls until the Carbon decides: every `interval` seconds, 5 more after each `slow_down`,
    /// until the code expires. `progress` hears about each wait. Fails with `device_denied` or
    /// `device_expired`; network failures and busy servers are retried until the code expires.
    pub async fn wait_for_device(
        &self,
        code: &DeviceCode,
        mut progress: impl FnMut(&DeviceProgress),
    ) -> Result<Tokens, AuthError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = Duration::from_secs(code.interval);
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(device_expired(code));
            }
            // The last poll comes just before the code expires, never after.
            tokio::time::sleep(interval.min(deadline - now)).await;
            match self.poll_device(code).await {
                Ok(DevicePoll::Tokens(t)) => return Ok(*t),
                Ok(DevicePoll::Pending) => progress(&DeviceProgress::Waiting { interval }),
                Ok(DevicePoll::SlowDown) => {
                    interval += Duration::from_secs(5);
                    progress(&DeviceProgress::SlowedDown { interval });
                }
                Ok(DevicePoll::Denied) => {
                    return Err(AuthError::new(
                        "device_denied",
                        format!(
                            "The sign-in with code {} was denied on the Silicon Accounts site.",
                            code.user_code
                        ),
                        "If that was a mistake, run `extend login` again and approve the new code.",
                    ));
                }
                Ok(DevicePoll::Expired) => return Err(device_expired(code)),
                Err(e) if e.transient => progress(&DeviceProgress::Retrying {
                    message: e.message.clone(),
                    retry_in: interval,
                }),
                Err(e) => return Err(e),
            }
        }
    }

    /// Exchanges a Silicon's short-lived token with `client_id` alone (`grant_type` …`:slt`). The
    /// token works once, whether the exchange succeeds or not. One that doesn't start with `slt_` is
    /// refused here (`not_an_slt`) and never sent.
    pub async fn exchange_slt(&self, slt: &str) -> Result<Tokens, AuthError> {
        let slt = slt.trim();
        if !slt.starts_with("slt_") || slt.len() < 8 || slt.chars().any(char::is_whitespace) {
            return Err(not_an_slt(slt));
        }
        let form = [
            ("grant_type", SLT_GRANT),
            ("slt", slt),
            ("client_id", self.app_id.as_str()),
        ];
        let answer = self.post_form("/v1/oauth/token", &form).await?;
        if answer.status < 300 {
            let r: TokenResponse = serde_json::from_slice(&answer.body)
                .map_err(|e| unexpected(&self.base, answer.status, &e.to_string()))?;
            return Ok(Tokens::from_response(r));
        }
        Err(self.oauth_failure(&answer, Step::Slt))
    }

    /// Rotates `refresh_token`. Store the new pair before using it: the old refresh token no longer
    /// works, and presenting it again revokes the sign-in. Fails with `sign_in_ended` when Silicon
    /// Accounts refused it (signed out, access removed, already used, expired).
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, AuthError> {
        self.accounts
            .refresh_app_public_client(&self.app_id, refresh_token)
            .await
            .map(Tokens::from_response)
            .map_err(|e| self.accounts_error(e, Step::Refresh))
    }

    /// Ends the sign-in behind a refresh (or access) token (`POST /v1/oauth/revoke` with
    /// `client_id` alone). Silicon Accounts answers 200 even for a token it doesn't know as
    /// Extend's; `revoked` says which.
    pub async fn revoke(&self, token: &str) -> Result<Revoked, AuthError> {
        let token = token.trim();
        let hint = if token.starts_with("sar_") {
            "refresh_token"
        } else {
            "access_token"
        };
        let form = [
            ("token", token),
            ("token_type_hint", hint),
            ("client_id", self.app_id.as_str()),
        ];
        let answer = self.post_form("/v1/oauth/revoke", &form).await?;
        if answer.status < 300 {
            return Ok(serde_json::from_slice(&answer.body).unwrap_or(Revoked {
                revoked: true,
                message: None,
            }));
        }
        Err(self.oauth_failure(&answer, Step::Revoke))
    }

    async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> Result<Answer, AuthError> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .post(&url)
            .form(form)
            .send()
            .await
            .map_err(|e| unreachable(&self.base, &e))?;
        let status = resp.status().as_u16();
        let request_id = resp
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = resp.bytes().await.map_err(|e| unreachable(&self.base, &e))?.to_vec();
        Ok(Answer {
            status,
            request_id,
            body,
        })
    }

    /// A refused `/v1/oauth/*` call, in Extend's words.
    fn oauth_failure(&self, a: &Answer, step: Step) -> AuthError {
        let v: serde_json::Value = serde_json::from_slice(&a.body).unwrap_or_default();
        let mut e = if let Some(error) = v.get("error").and_then(|e| e.as_str()) {
            let description = v
                .get("error_description")
                .and_then(|d| d.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Silicon Accounts refused it with `{error}`."));
            self.oauth_error(error, &description, a.status, step)
        } else if let Some(api) = v.get("error").filter(|e| e.is_object()) {
            let s = |k: &str| api.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
            let code = s("code");
            AuthError {
                transient: a.status >= 500 || a.status == 429,
                ..AuthError::new(
                    if code.is_empty() { "accounts_error" } else { &code },
                    s("message"),
                    Some(s("hint"))
                        .filter(|h| !h.is_empty())
                        .unwrap_or_else(|| "Retry in a moment.".into()),
                )
            }
        } else {
            unexpected(
                &self.base,
                a.status,
                &String::from_utf8_lossy(&a.body).chars().take(200).collect::<String>(),
            )
        };
        e.status = Some(a.status);
        e.request_id = a.request_id.clone();
        e
    }

    fn oauth_error(&self, error: &str, description: &str, status: u16, step: Step) -> AuthError {
        let app = &self.app_id;
        match (error, step) {
            ("invalid_grant", Step::Slt) => slt_refused(description),
            ("invalid_grant", Step::Refresh) => AuthError::new(
                "sign_in_ended",
                format!("Your sign-in to Extend has ended: {description}"),
                format!("Sign in again: `{app} login` (Carbons) or `{SILICON_SIGN_IN}` (Silicons)."),
            ),
            ("unauthorized_client", Step::Slt) => AuthError::new(
                "public_client_off",
                format!("Silicon Accounts doesn't let {app}'s CLI exchange short-lived tokens by itself: {description}"),
                format!("Extend's sign-in setup in Silicon Accounts needs `public_client` turned on; tell whoever runs {app}."),
            ),
            ("unauthorized_client", _) => AuthError::new(
                "device_flow_off",
                format!("Silicon Accounts doesn't let {app}'s CLI sign Carbons in with a code: {description}"),
                format!("Extend's sign-in setup in Silicon Accounts needs `device_flow` turned on; tell whoever runs {app}."),
            ),
            ("invalid_client", _) => AuthError::new(
                "unknown_app",
                format!("The Silicon Accounts at {} doesn't know the app `{app}`: {description}", self.base),
                "Check ACCOUNTS_URL: it must be the Silicon Accounts this Extend uses.",
            ),
            ("temporarily_unavailable" | "server_error", _) => AuthError {
                transient: true,
                ..AuthError::new("accounts_error", description, "Retry in a moment.")
            },
            _ => AuthError::new(error, description, "Retry; if it keeps failing, report it with the request id."),
        }
        .status(status)
    }

    /// An error from `silicon-accounts-client`, in Extend's words.
    fn accounts_error(&self, e: silicon_accounts_client::Error, step: Step) -> AuthError {
        use silicon_accounts_client::Error as E;
        match e {
            E::OAuth(o) => {
                let description = o.message();
                let mut out = self.oauth_error(&o.error, &description, o.status, step);
                out.request_id = o.request_id.clone();
                out
            }
            E::Api(a) => {
                let mut out = AuthError::new(
                    if a.status == 429 {
                        "rate_limited"
                    } else {
                        a.code.as_str()
                    },
                    a.message.clone(),
                    a.hint.clone().unwrap_or_else(|| "Retry in a moment.".into()),
                )
                .status(a.status);
                out.request_id = a.request_id.clone();
                out.transient = a.status >= 500;
                out
            }
            E::Http { message, .. } => AuthError {
                transient: true,
                ..AuthError::new(
                    "accounts_unreachable",
                    format!("Could not reach Silicon Accounts at {}: {message}", self.base),
                    "Check the network, or ACCOUNTS_URL if it is set.",
                )
            },
            E::Decode { message, .. } => unexpected(&self.base, 0, &message),
            other => AuthError::new("invalid_input", other.message(), "Check the value named above."),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    DeviceStart,
    DevicePoll,
    Slt,
    Refresh,
    Revoke,
}

struct Answer {
    status: u16,
    request_id: Option<String>,
    body: Vec<u8>,
}

fn device_expired(code: &DeviceCode) -> AuthError {
    AuthError::new(
        "device_expired",
        format!(
            "The code {} expired before anyone approved it (codes last {} minutes).",
            code.user_code,
            code.expires_in.div_ceil(60)
        ),
        "Run `extend login` again and approve the new code in time.",
    )
}

fn unreachable(base: &str, e: &reqwest::Error) -> AuthError {
    let mut source: &dyn std::error::Error = e;
    while let Some(next) = source.source() {
        source = next;
    }
    AuthError {
        transient: true,
        ..AuthError::new(
            "accounts_unreachable",
            format!("Could not reach Silicon Accounts at {base}: {source}"),
            "Check the network, or ACCOUNTS_URL if it is set.",
        )
    }
}

fn unexpected(base: &str, status: u16, detail: &str) -> AuthError {
    AuthError {
        status: (status > 0).then_some(status),
        ..AuthError::new(
            "unexpected_response",
            format!("Silicon Accounts at {base} answered something this client can't read: {detail}"),
            "Check ACCOUNTS_URL points at Silicon Accounts; if it does, report it.",
        )
    }
}

/// What a value that isn't a short-lived token looks like, without repeating it.
fn not_an_slt(value: &str) -> AuthError {
    let looks_like = match value {
        "" => "it is empty".to_owned(),
        v if v.starts_with("sar_") => "it is a refresh token".to_owned(),
        v if v.starts_with("stk-") => "it is an STK, which never leaves the Silicon".to_owned(),
        v if v.starts_with("eyJ") => "it is an access token".to_owned(),
        v if v.chars().any(char::is_whitespace) && v.starts_with("slt_") => {
            "it has spaces or line breaks inside".to_owned()
        }
        v => match v.find('_').filter(|i| *i <= 6) {
            Some(i) => format!("it starts with {}", &v[..=i]),
            None => format!("it is {} characters with no slt_ prefix", v.chars().count()),
        },
    };
    AuthError::new(
        "not_an_slt",
        format!(
            "That is not a Silicon Accounts short-lived token (they start with slt_): {looks_like}. Nothing was sent."
        ),
        format!("Mint one and pass it straight on: {SILICON_SIGN_IN}"),
    )
}

/// Silicon Accounts refused a short-lived token: its `error_description` says why.
fn slt_refused(description: &str) -> AuthError {
    let d = description.trim();
    let fresh = format!(
        "Short-lived tokens work once, for 2 minutes, for one app. Mint a fresh one right before you use it: {SILICON_SIGN_IN}"
    );
    let (code, hint) = if d.starts_with("The short-lived token was already used") {
        ("slt_already_used", fresh)
    } else if d.starts_with("The short-lived token expired") {
        ("slt_expired", fresh)
    } else if d.starts_with("The short-lived token was issued for the app") {
        (
            "slt_wrong_app",
            format!("Mint one for Extend (`--app extend`): {SILICON_SIGN_IN}"),
        )
    } else if d.starts_with("The short-lived token is not known") {
        (
            "slt_unknown",
            format!("Check it was copied whole, or mint a fresh one: {SILICON_SIGN_IN}"),
        )
    } else if d.starts_with("slt must be a short-lived token") {
        (
            "slt_wrong_kind",
            format!("Pass the slt_… value `{MINT_SLT}` prints: {SILICON_SIGN_IN}"),
        )
    } else if d.contains("rotated its STK") || d.contains("trusted outside token") {
        (
            "slt_sign_in_ended",
            format!(
                "The Silicon's own Silicon Accounts sign-in has ended. Sign in to Silicon Accounts again (`silicon-accounts login --silicon si:<id> --stk-stdin`), then: {SILICON_SIGN_IN}"
            ),
        )
    } else {
        ("slt_refused", fresh)
    };
    AuthError::new(
        code,
        format!("Silicon Accounts refused the short-lived token: {d}"),
        hint,
    )
}
