//! The official Rust client for Silicon Extend.
//!
//! Stateless: it holds a base URL, an HTTP connection pool, the negotiated API version and
//! (optionally) a test-environment secret. Tokens are passed in per call through [`Authed`]; where
//! they're stored is the caller's business (the `extend` CLI keeps them under `SILICON_HOME`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), silicon_extend_client::Error> {
//! use silicon_extend_client::Client;
//! let client = Client::connect("https://backend.extend.teamofsilicons.com").await?;
//! let session = client.login("<short-lived token from Silicon IAM>").await?;
//! let me = client.authed(&session.access_token, Some("acme"));
//! for device in me.devices(Default::default()).await?.items {
//!     println!("{} {} online={}", device.device_id, device.name, device.online);
//! }
//! # Ok(()) }
//! ```

use std::time::Duration;

pub mod attachments;
/// Opt-in reference selection experiment; does not alter device commands or authorization.
pub mod ref_actions;
pub use extend_protocol as protocol;
use extend_protocol::envelope::Page;
use extend_protocol::model::*;
use extend_protocol::{
    API_VERSION, API_VERSION_HEADER, ApiError, DeviceId, ErrorCode, SUPPORTED_VERSIONS_HEADER, TEAM_HEADER,
    TESTING_SECRET_HEADER,
};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

/// Versions of the API this crate speaks.
pub const SUPPORTED_API_VERSIONS: &[u32] = &[API_VERSION];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Extend answered with an error envelope.
    #[error("{} ({}): {}", .error.message, .error.code.as_str(), .error.hint.clone().unwrap_or_default())]
    Api { status: u16, error: Box<ApiError> },
    /// The request never got a usable answer.
    #[error("could not reach Silicon Extend at {url}: {source}")]
    Transport { url: String, source: reqwest::Error },
    /// Extend answered with something this client can't read.
    #[error("unexpected response from Silicon Extend ({status}): {detail}")]
    Decode { status: u16, detail: String },
    #[error("invalid input: {0}")]
    Invalid(String),
}

impl Error {
    /// The stable error code, when Extend sent one.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Api { error, .. } => error.code,
            Self::Transport { .. } => ErrorCode::ServiceUnavailable,
            Self::Decode { .. } => ErrorCode::Internal,
            Self::Invalid(_) => ErrorCode::InvalidInput,
        }
    }
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Self::Api { error, .. } => Some(error),
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base_url: String,
    testing_secret: Option<String>,
    timeout: Duration,
    user_agent: String,
    telemetry: bool,
    isi: Option<String>,
}

impl ClientBuilder {
    /// The internal Silicon (ISI) acting, recorded with the Silicon's actions. Optional.
    pub fn isi(mut self, isi: Option<String>) -> Self {
        self.isi = isi.filter(|s| !s.trim().is_empty() && s.len() <= 128);
        self
    }
    /// Selects a test environment by its test application secret (`ask_…`).
    pub fn testing_secret(mut self, secret: impl Into<String>) -> Self {
        self.testing_secret = Some(secret.into());
        self
    }
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }
    /// Sends `X-Extend-Telemetry: off` when false.
    pub fn telemetry(mut self, on: bool) -> Self {
        self.telemetry = on;
        self
    }

    /// Builds the client and negotiates the API version with the service.
    pub async fn connect(self) -> Result<Client> {
        let base = self.base_url.trim_end_matches('/').to_owned();
        if !(base.starts_with("https://")
            || base.starts_with("http://127.0.0.1")
            || base.starts_with("http://localhost")
            || base.starts_with("http://10.0.2.2")
            || base.starts_with("http://[::1]"))
        {
            return Err(Error::Invalid(format!(
                "{base} must be https (plain http is allowed only for local addresses)"
            )));
        }
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent(self.user_agent.clone())
            .build()
            .map_err(|e| Error::Transport {
                url: base.clone(),
                source: e,
            })?;
        let mut client = Client {
            http,
            base,
            testing_secret: self.testing_secret,
            api_version: API_VERSION,
            telemetry: self.telemetry,
            isi: self.isi,
        };
        let supported: Vec<String> = SUPPORTED_API_VERSIONS.iter().map(u32::to_string).collect();
        let resp = client
            .http
            .get(format!("{}/api/version", client.base))
            .header(SUPPORTED_VERSIONS_HEADER, supported.join(", "))
            .send()
            .await
            .map_err(|e| Error::Transport {
                url: client.base.clone(),
                source: e,
            })?;
        let info: VersionInfo = decode(resp).await?;
        client.api_version = info.api_version;
        Ok(client)
    }
}

/// A connection to Silicon Extend.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    testing_secret: Option<String>,
    api_version: u32,
    telemetry: bool,
    isi: Option<String>,
}

/// A successful answer's envelope: the HTTP status, the envelope `type` and its `data`.
struct Answer {
    status: u16,
    kind: Option<String>,
    data: serde_json::Value,
}

impl Answer {
    fn into<T: DeserializeOwned>(self) -> Result<T> {
        serde_json::from_value(self.data).map_err(|e| Error::Decode {
            status: self.status,
            detail: e.to_string(),
        })
    }
}

async fn decode_envelope(resp: reqwest::Response) -> Result<Answer> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| Error::Decode {
        status: status.as_u16(),
        detail: e.to_string(),
    })?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Error::Decode {
        status: status.as_u16(),
        detail: String::from_utf8_lossy(&bytes).chars().take(300).collect(),
    })?;
    if !status.is_success() || v.get("type").and_then(|t| t.as_str()) == Some("error") {
        let error: ApiError =
            serde_json::from_value(v.get("data").cloned().unwrap_or_default()).map_err(|e| Error::Decode {
                status: status.as_u16(),
                detail: format!("error body: {e}"),
            })?;
        return Err(Error::Api {
            status: status.as_u16(),
            error: Box::new(error),
        });
    }
    Ok(Answer {
        status: status.as_u16(),
        kind: v.get("type").and_then(|t| t.as_str()).map(str::to_owned),
        data: v.get("data").cloned().unwrap_or(serde_json::Value::Null),
    })
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    decode_envelope(resp).await?.into()
}

async fn expect_empty(resp: reqwest::Response) -> Result<()> {
    if resp.status().is_success() {
        return Ok(());
    }
    decode::<serde_json::Value>(resp).await.map(|_| ())
}

fn env<T: Serialize>(kind: &str, data: T) -> serde_json::Value {
    serde_json::json!({"type": kind, "data": data})
}

impl Client {
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
            testing_secret: None,
            timeout: Duration::from_secs(330),
            user_agent: concat!("silicon-extend-client/", env!("CARGO_PKG_VERSION")).into(),
            telemetry: true,
            isi: None,
        }
    }

    /// Connects with default settings.
    pub async fn connect(base_url: impl Into<String>) -> Result<Self> {
        Self::builder(base_url).connect().await
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }
    pub fn api_version(&self) -> u32 {
        self.api_version
    }
    pub fn testing_secret(&self) -> Option<&str> {
        self.testing_secret.as_deref()
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(API_VERSION_HEADER, self.api_version.to_string());
        if let Some(s) = &self.testing_secret {
            r = r.header(TESTING_SECRET_HEADER, s);
        }
        if !self.telemetry {
            r = r.header("X-Extend-Telemetry", "off");
        }
        if let Some(isi) = &self.isi {
            r = r.header("X-Silicon-ISI", isi);
        }
        r
    }

    async fn send(&self, r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        r.send().await.map_err(|e| Error::Transport {
            url: self.base.clone(),
            source: e,
        })
    }

    /// Scopes calls to a signed-in member and (optionally) one team.
    pub fn authed<'a>(&'a self, access_token: &'a str, team: Option<&'a str>) -> Authed<'a> {
        Authed {
            c: self,
            token: access_token,
            team,
        }
    }

    pub async fn iam(&self) -> Result<IamInfo> {
        decode(self.send(self.req(Method::GET, "/api/v1/iam")).await?).await
    }

    pub async fn contracts(&self) -> Result<serde_json::Value> {
        decode(self.send(self.req(Method::GET, "/api/v1/contracts")).await?).await
    }

    /// Exchanges a short-lived token from Silicon IAM. In a test environment a test member id
    /// (`c:alice`, `si:chef`) also works.
    pub async fn login(&self, slt: &str) -> Result<AuthSession> {
        let r = self
            .req(Method::POST, "/api/v1/auth/login")
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&env("login", LoginInput { slt: slt.to_owned() }));
        decode(self.send(r).await?).await
    }

    pub async fn refresh(&self, refresh_token: &str, idempotency_key: &str) -> Result<AuthSession> {
        let r = self
            .req(Method::POST, "/api/v1/auth/refresh")
            .header("Idempotency-Key", idempotency_key)
            .json(&env(
                "refresh",
                RefreshInput {
                    refresh_token: refresh_token.to_owned(),
                },
            ));
        decode(self.send(r).await?).await
    }

    pub async fn logout(&self, token: &str, access_token: Option<&str>) -> Result<()> {
        let mut r = self
            .req(Method::POST, "/api/v1/auth/logout")
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&env(
                "logout",
                LogoutInput {
                    token: token.to_owned(),
                },
            ));
        if let Some(a) = access_token {
            r = r.bearer_auth(a);
        }
        expect_empty(self.send(r).await?).await
    }

    /// The test environment the client's secret selects.
    pub async fn testing_environment(&self) -> Result<TestingEnvironment> {
        decode(self.send(self.req(Method::GET, "/api/v1/testing-environment")).await?).await
    }

    // ── Device side (Extend apps) ──

    pub async fn enroll(&self, input: &EnrollmentCreate) -> Result<EnrollmentCreated> {
        decode(
            self.send(
                self.req(Method::POST, "/api/v1/enrollments")
                    .json(&env("enrollment", input)),
            )
            .await?,
        )
        .await
    }

    pub async fn enrollment(&self, id: Uuid, secret: &str) -> Result<EnrollmentState> {
        let r = self
            .req(Method::GET, &format!("/api/v1/enrollments/{id}"))
            .header("authorization", format!("Extend-Enrollment {secret}"));
        decode(self.send(r).await?).await
    }

    /// "Pair with another Carbon" (`POST /api/v1/device/enrollments`, 1.1): an app that is already
    /// paired starts an enrollment with the credential of any of its live pairs, so another Carbon
    /// can pair the same device. The answer is shaped like [`Client::enroll`]'s; follow it with
    /// [`Client::enrollment`] and the enrollment socket as for a first pairing.
    pub async fn pair_enrollment(&self, credential: &str) -> Result<EnrollmentCreated> {
        let r = self
            .req(Method::POST, "/api/v1/device/enrollments")
            .header("authorization", format!("Extend-Device {credential}"))
            .json(&env("enrollment", serde_json::json!({})));
        decode(self.send(r).await?).await
    }

    pub async fn device_self(&self, credential: &str) -> Result<DeviceSelf> {
        let r = self
            .req(Method::GET, "/api/v1/device")
            .header("authorization", format!("Extend-Device {credential}"));
        decode(self.send(r).await?).await
    }

    /// The device's own Extend app changes the device's settings (`PATCH /api/v1/device`, 1.1),
    /// with any of its pair credentials: today whether it shows the in-use banner, which every pair
    /// of the device shares. Answers the device as [`Client::device_self`] reads it.
    pub async fn update_device_self(&self, credential: &str, patch: &DeviceSelfPatch) -> Result<DeviceSelf> {
        let r = self
            .req(Method::PATCH, "/api/v1/device")
            .header("authorization", format!("Extend-Device {credential}"))
            .json(&env("device_self", patch));
        decode(self.send(r).await?).await
    }

    pub async fn revoke_pair(&self, credential: &str) -> Result<()> {
        let r = self
            .req(Method::DELETE, "/api/v1/device")
            .header("authorization", format!("Extend-Device {credential}"));
        expect_empty(self.send(r).await?).await
    }

    /// The Carbon tapped Stop on the device (`POST /api/v1/device/stop`): ends the session running
    /// on it, and on a host, on every device it carries. The fallback when the socket's `stop`
    /// frame can't be sent.
    pub async fn device_stop(&self, credential: &str) -> Result<()> {
        let r = self
            .req(Method::POST, "/api/v1/device/stop")
            .header("authorization", format!("Extend-Device {credential}"));
        expect_empty(self.send(r).await?).await
    }

    pub async fn upload_artifact(
        &self,
        credential: &str,
        upload_id: Uuid,
        name: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        use sha2_lite::sha256_hex;
        let r = self
            .req(Method::PUT, &format!("/api/v1/device/artifacts/{upload_id}"))
            .header("authorization", format!("Extend-Device {credential}"))
            .header("content-type", content_type)
            .header("x-file-name", name)
            .header("x-content-sha256", sha256_hex(&bytes))
            .body(bytes);
        expect_empty(self.send(r).await?).await
    }

    /// The WebSocket URL for a path (`/api/v1/device/connect`).
    pub fn ws_url(&self, path: &str) -> String {
        let base = self
            .base
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        format!("{base}{path}")
    }

    /// Downloads a file by its URL (a Briefcase link, or a local development link).
    pub async fn download(&self, url: &str, access_token: &str) -> Result<Vec<u8>> {
        let resp = self.send(self.http.get(url).bearer_auth(access_token)).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Decode {
                status: status.as_u16(),
                detail: format!("downloading {url} failed"),
            });
        }
        Ok(resp
            .bytes()
            .await
            .map_err(|e| Error::Transport {
                url: url.to_owned(),
                source: e,
            })?
            .to_vec())
    }
}

/// Filters for listing devices.
#[derive(Debug, Clone, Default)]
pub struct DeviceQuery {
    /// `mine` (Carbons: every device they paired, whatever the Team) or `accessible` (Silicons:
    /// the devices they were given access to in their Team). Default depends on the member. `team`
    /// is deprecated: from 1.1 a device is only visible to the Carbons who paired it, so it always
    /// lists nothing.
    pub scope: Option<String>,
    pub online: Option<bool>,
    pub os: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    pub silicon_id: Option<String>,
    pub session_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub device_id: Option<String>,
    pub session_id: Option<String>,
    pub state: Option<String>,
    pub kind: Option<String>,
    pub direction: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

fn qs(pairs: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| {
            v.as_ref().map(|v| {
                format!(
                    "{k}={}",
                    url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// What [`Authed::stop`] stopped.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StopOutcome {
    /// The session ran through the caller's own pair of the device (envelope type `session`, the
    /// 1.0 answer).
    Session(Box<Session>),
    /// The Silicon using the device was given access by another Carbon who paired it, so it isn't
    /// named (envelope type `device_stopped`).
    Other(DeviceStopped),
}

/// Calls made as a signed-in member.
///
/// The Team (`X-Org-ID`) is the Silicon's Team for everything a Silicon does. A Carbon's calls on
/// their own devices work in every Team, so the Team may be left out; it is still the Team a
/// [`Authed::grant`] gives access in.
#[derive(Debug, Clone, Copy)]
pub struct Authed<'a> {
    c: &'a Client,
    token: &'a str,
    team: Option<&'a str>,
}

impl Authed<'_> {
    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.c.req(method, path).bearer_auth(self.token);
        if let Some(t) = self.team {
            r = r.header(TEAM_HEADER, t);
        }
        r
    }
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        decode(self.c.send(self.req(Method::GET, path)).await?).await
    }
    async fn post<T: DeserializeOwned, B: Serialize>(&self, path: &str, kind: &str, body: B, idem: bool) -> Result<T> {
        let mut r = self.req(Method::POST, path).json(&env(kind, body));
        if idem {
            r = r.header("Idempotency-Key", Uuid::new_v4().to_string());
        }
        decode(self.c.send(r).await?).await
    }

    pub async fn me(&self) -> Result<Me> {
        self.get("/api/v1/auth/me").await
    }

    pub async fn devices(&self, q: DeviceQuery) -> Result<Page<Device>> {
        self.get(&format!(
            "/api/v1/devices{}",
            qs(&[
                ("scope", q.scope),
                ("online", q.online.map(|b| b.to_string())),
                ("os", q.os),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    /// Like [`Authed::devices`], but also lists the Carbon's removed devices, each with
    /// `removed_at` and `removed_reason` set (`include_removed=true`; `scope=mine` only). A removed
    /// device's record and activity log stay readable with [`Authed::device`] and
    /// [`Authed::activity`].
    pub async fn devices_including_removed(&self, q: DeviceQuery) -> Result<Page<Device>> {
        self.get(&format!(
            "/api/v1/devices{}",
            qs(&[
                ("scope", q.scope),
                ("online", q.online.map(|b| b.to_string())),
                ("os", q.os),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor),
                ("include_removed", Some("true".to_owned()))
            ])
        ))
        .await
    }

    pub async fn device(&self, id: &str) -> Result<Device> {
        self.get(&format!("/api/v1/devices/{id}")).await
    }

    pub async fn pair(&self, claim: &PairingClaim) -> Result<Device> {
        self.post("/api/v1/pairings", "pairing", claim, true).await
    }

    pub async fn attach(&self, host_id: &str, input: &AttachmentCreate) -> Result<Device> {
        self.post(
            &format!("/api/v1/devices/{host_id}/attachments"),
            "attachment",
            input,
            true,
        )
        .await
    }

    pub async fn update_device(&self, id: &str, version: Option<i64>, patch: &DevicePatch) -> Result<Device> {
        let mut r = self
            .req(Method::PATCH, &format!("/api/v1/devices/{id}"))
            .json(&env("device", patch));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        decode(self.c.send(r).await?).await
    }

    /// Shows or hides the in-use banner on a device (`PATCH /api/v1/devices/{id}` with
    /// `in_use_indicator`, 1.1). One setting for the physical device, shared by every Carbon who
    /// paired it. Carbon only: a Silicon gets `carbon_only`.
    pub async fn set_in_use_indicator(&self, id: &str, value: InUseIndicator) -> Result<Device> {
        let r = self.req(Method::PATCH, &format!("/api/v1/devices/{id}")).json(&env(
            "device",
            &DeviceSettingsPatch {
                in_use_indicator: Some(value),
                ..Default::default()
            },
        ));
        decode(self.c.send(r).await?).await
    }

    pub async fn remove_device(&self, id: &str, version: Option<i64>) -> Result<()> {
        let mut r = self.req(Method::DELETE, &format!("/api/v1/devices/{id}"));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        expect_empty(self.c.send(r).await?).await
    }

    /// Stops the Silicon using a device, answering the stopped session. A 1.1 service answers
    /// `device_stopped` instead when that Silicon was given access by another Carbon who paired the
    /// device; this call then fails to decode after the stop succeeded. [`Authed::stop`] reads both.
    pub async fn stop_device(&self, id: &str) -> Result<Session> {
        self.post(
            &format!("/api/v1/devices/{id}/stop"),
            "stop",
            serde_json::json!({}),
            false,
        )
        .await
    }

    /// Stops the Silicon using a device (`POST /api/v1/devices/{device_id}/stop`), through any
    /// Carbon's pair of it: every Carbon who paired a device may stop it. On a computer it also
    /// stops devices it carries that the caller paired. When only a carried device the caller
    /// didn't pair is busy, it fails with `conflict` (stop it at the computer).
    pub async fn stop(&self, id: &str) -> Result<StopOutcome> {
        let r = self
            .req(Method::POST, &format!("/api/v1/devices/{id}/stop"))
            .json(&env("stop", serde_json::json!({})));
        let a = decode_envelope(self.c.send(r).await?).await?;
        if a.kind.as_deref() == Some("device_stopped") {
            Ok(StopOutcome::Other(a.into()?))
        } else {
            Ok(StopOutcome::Session(Box::new(a.into()?)))
        }
    }

    /// Silicons in the team, for choosing who gets access.
    pub async fn team_silicons(&self) -> Result<Vec<TeamSilicon>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<TeamSilicon>,
        }
        Ok(self.get::<Items>("/api/v1/team/silicons").await?.items)
    }

    /// The Silicons of every Team the Carbon's Extend login reaches, each tagged with its Team
    /// (`GET /api/v1/team/silicons?team=any`, 1.1), and how each Team's directory read went: a
    /// Team that couldn't be read is in `teams` with its error, and the others still answer.
    pub async fn team_silicons_all(&self) -> Result<TeamSilicons> {
        self.get("/api/v1/team/silicons?team=any").await
    }

    pub async fn setup(&self, id: &str) -> Result<Setup> {
        self.get(&format!("/api/v1/devices/{id}/setup")).await
    }

    /// Runs a device's failed setup step again now (`POST /api/v1/devices/{device_id}/setup/retry`,
    /// 1.1), or every failed step when `step` is `None`. Answers the keys of the steps the device
    /// was asked to rerun; the device then reports progress as usual ([`Authed::setup`]). Fails
    /// with `conflict` when nothing (or not that step) has failed, `device_offline` when the device
    /// or its computer isn't connected, `upgrade_required` when its app is older than 1.1, and
    /// `rate_limited` within 5 seconds of the last retry.
    pub async fn retry_setup(&self, id: &str, step: Option<&str>) -> Result<RetryResult> {
        let input = match step {
            Some(k) => SetupRetryInput::step(k),
            None => SetupRetryInput::all(),
        };
        self.post(
            &format!("/api/v1/devices/{id}/setup/retry"),
            "setup_retry",
            input,
            false,
        )
        .await
    }

    pub async fn setup_code(&self, id: &str, code: &str) -> Result<Setup> {
        self.post(
            &format!("/api/v1/devices/{id}/setup/code"),
            "setup_code",
            serde_json::json!({"code": code}),
            false,
        )
        .await
    }

    pub async fn access(&self, id: &str) -> Result<Vec<AccessGrant>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<AccessGrant>,
        }
        Ok(self.get::<Items>(&format!("/api/v1/devices/{id}/access")).await?.items)
    }

    /// Gives a Silicon access to the Carbon's device, in this [`Authed`]'s Team (the Silicon's
    /// Team). The Carbon's login must reach that Team (`not_a_team_member` otherwise).
    pub async fn grant(&self, id: &str, silicon_id: &str) -> Result<AccessGrant> {
        decode(
            self.c
                .send(self.req(Method::PUT, &format!("/api/v1/devices/{id}/access/{silicon_id}")))
                .await?,
        )
        .await
    }

    /// Takes a Silicon's access to the device away, in every Team it was given in, and ends its
    /// running session there.
    pub async fn revoke(&self, id: &str, silicon_id: &str) -> Result<()> {
        expect_empty(
            self.c
                .send(self.req(Method::DELETE, &format!("/api/v1/devices/{id}/access/{silicon_id}")))
                .await?,
        )
        .await
    }

    /// Takes a Silicon's access to the device away in one Team only (1.1). It works on ownership
    /// alone, even for a Team the Carbon's login no longer reaches.
    pub async fn revoke_in_team(&self, id: &str, silicon_id: &str, team: &str) -> Result<()> {
        let path = format!(
            "/api/v1/devices/{id}/access/{silicon_id}{}",
            qs(&[("team", Some(team.to_owned()))])
        );
        expect_empty(self.c.send(self.req(Method::DELETE, &path)).await?).await
    }

    pub async fn activity(&self, id: &str, q: ActivityQuery) -> Result<Page<ActivityEntry>> {
        self.get(&format!(
            "/api/v1/devices/{id}/activity{}",
            qs(&[
                ("silicon_id", q.silicon_id),
                ("session_id", q.session_id),
                ("since", q.since),
                ("until", q.until),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn device_requests(&self, id: &str, q: ListQuery) -> Result<Page<RequestInfo>> {
        self.get(&format!(
            "/api/v1/devices/{id}/requests{}",
            qs(&[("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)])
        ))
        .await
    }

    /// Asks for a device another Silicon is using, with a reason of 1–300 characters. When that
    /// Silicon is in the caller's Team and was given access by the same Carbon, the request goes to
    /// it (`to` names it). Otherwise it goes to the Carbon who gave that Silicon access: `to_hidden`
    /// is set and `to` reads [`protocol::REQUEST_TO_HIDDEN`].
    pub async fn send_request(&self, device_id: &str, reason: &str) -> Result<RequestInfo> {
        self.post(
            &format!("/api/v1/devices/{device_id}/requests"),
            "request",
            RequestCreate {
                reason: reason.to_owned(),
            },
            true,
        )
        .await
    }

    // ── Waking a device (1.1) ──

    /// Asks the Carbon who gave this Silicon access to wake a device that isn't awake
    /// (`POST /api/v1/devices/{device_id}/wake-requests`), with a reason of 1–300 characters. The
    /// device shows it where it can, and the Carbon gets it through Ting. Asking again after 5
    /// minutes refreshes the open request (`asks` goes up); sooner fails with `rate_limited`. Fails
    /// with `conflict` when the device is already awake or its Carbon turned wake requests off, and
    /// `device_in_use` when another Silicon is using it.
    pub async fn wake(&self, device_id: &str, reason: &str) -> Result<WakeRequest> {
        self.post(
            &format!("/api/v1/devices/{device_id}/wake-requests"),
            "wake_request",
            WakeCreate::new(reason),
            true,
        )
        .await
    }

    /// Withdraws the Silicon's own open wake request.
    pub async fn cancel_wake(&self, device_id: &str, wake_id: Uuid) -> Result<()> {
        expect_empty(
            self.c
                .send(self.req(
                    Method::DELETE,
                    &format!("/api/v1/devices/{device_id}/wake-requests/{wake_id}"),
                ))
                .await?,
        )
        .await
    }

    /// Wake requests on a device: the Carbon who paired it sees every Team's, a Silicon its own.
    /// `q.state` is `open` or `all` (the default); `limit` and `cursor` page.
    pub async fn wake_requests(&self, device_id: &str, q: ListQuery) -> Result<Page<WakeRequest>> {
        self.get(&format!(
            "/api/v1/devices/{device_id}/wake-requests{}",
            qs(&[
                ("state", q.state),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    /// The Carbon answers wake requests on their device. `woken` ("It's awake") is about the
    /// device: it ends every open request on it, whichever Carbon's and whichever Team's. `declined`
    /// ends the requests on the Carbon's own pair (or the listed ones).
    pub async fn answer_wake(&self, device_id: &str, answer: &WakeAnswer) -> Result<WakeAnswered> {
        let r = self
            .req(
                Method::POST,
                &format!("/api/v1/devices/{device_id}/wake-requests/answer"),
            )
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&env("wake_answer", answer));
        let resp = self.c.send(r).await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(WakeAnswered::new(answer.answer, vec![]));
        }
        decode(resp).await
    }

    /// Turns wake requests off (or on again) for the Carbon's pair of a device, or for one Silicon
    /// on it, in every Team or only `settings.team`. Turning them off withdraws the open ones.
    pub async fn set_wake_settings(&self, device_id: &str, settings: &WakeSettings) -> Result<WakeSettingsView> {
        decode(
            self.c
                .send(
                    self.req(Method::PUT, &format!("/api/v1/devices/{device_id}/wake-settings"))
                        .json(&env("wake_settings", settings)),
                )
                .await?,
        )
        .await
    }

    // ── Ting (1.1) ──

    /// Whether Extend's Tings reach the caller in `team`, and which of Extend's Ting types Ting
    /// doesn't know there (`GET /api/v1/ting-registration?team=`).
    pub async fn ting_registration(&self, team: &str) -> Result<TingRegistration> {
        self.get(&format!(
            "/api/v1/ting-registration{}",
            qs(&[("team", Some(team.to_owned()))])
        ))
        .await
    }

    /// [`Authed::ting_registration`] for every Team a Carbon's login reaches, plus the Teams of their
    /// grants (`?team=any`).
    pub async fn ting_registrations(&self) -> Result<Vec<TingRegistration>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<TingRegistration>,
        }
        Ok(self.get::<Items>("/api/v1/ting-registration?team=any").await?.items)
    }

    /// "Turn on": registers the caller with Ting in `team` again, with their own login for it, and
    /// sends the Tings waiting for them there (`PUT /api/v1/ting-registration?team=`).
    pub async fn ting_turn_on(&self, team: &str) -> Result<TingRegistration> {
        decode(
            self.c
                .send(
                    self.req(
                        Method::PUT,
                        &format!("/api/v1/ting-registration{}", qs(&[("team", Some(team.to_owned()))])),
                    )
                    .json(&env("ting_registration", serde_json::json!({}))),
                )
                .await?,
        )
        .await
    }

    pub async fn requests(&self, q: ListQuery) -> Result<Page<RequestInfo>> {
        self.get(&format!(
            "/api/v1/requests{}",
            qs(&[
                ("direction", q.direction),
                ("device_id", q.device_id),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn start_session(&self, device_id: &DeviceId) -> Result<Session> {
        self.post(
            "/api/v1/sessions",
            "session",
            SessionCreate {
                device_id: device_id.clone(),
            },
            true,
        )
        .await
    }

    pub async fn sessions(&self, q: ListQuery) -> Result<Page<Session>> {
        self.get(&format!(
            "/api/v1/sessions{}",
            qs(&[
                ("device_id", q.device_id),
                ("state", q.state),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        self.get(&format!("/api/v1/sessions/{id}")).await
    }

    pub async fn end_session(&self, id: &str) -> Result<Session> {
        self.post(
            &format!("/api/v1/sessions/{id}/end"),
            "end",
            serde_json::json!({}),
            false,
        )
        .await
    }

    pub async fn takeover(&self, id: &str, reason: &str) -> Result<Takeover> {
        self.post(
            &format!("/api/v1/sessions/{id}/takeover"),
            "takeover",
            TakeoverCreate {
                reason: reason.to_owned(),
            },
            false,
        )
        .await
    }

    pub async fn takeover_status(&self, id: &str) -> Result<Option<Takeover>> {
        self.get(&format!("/api/v1/sessions/{id}/takeover")).await
    }

    pub async fn release_takeover(&self, id: &str) -> Result<()> {
        expect_empty(
            self.c
                .send(self.req(Method::DELETE, &format!("/api/v1/sessions/{id}/takeover")))
                .await?,
        )
        .await
    }

    /// Runs one command in a session and waits for the device's answer.
    pub async fn run(&self, session_id: &str, cmd: &CommandRequest) -> Result<CommandResult> {
        self.post(
            &format!("/api/v1/sessions/{session_id}/commands"),
            "command",
            cmd,
            false,
        )
        .await
    }

    pub async fn files(&self, q: ListQuery) -> Result<Page<FileInfo>> {
        self.get(&format!(
            "/api/v1/files{}",
            qs(&[
                ("session_id", q.session_id),
                ("device_id", q.device_id),
                ("kind", q.kind),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn file(&self, id: &str) -> Result<FileInfo> {
        self.get(&format!("/api/v1/files/{id}")).await
    }

    pub async fn keep_file(&self, id: &str) -> Result<FileInfo> {
        self.post(
            &format!("/api/v1/files/{id}/keep"),
            "keep",
            serde_json::json!({}),
            false,
        )
        .await
    }

    /// A file's bytes, read through Extend (`GET /api/v1/files/{file_id}/content`). Works for
    /// files stored in Briefcase, which Extend reads on the caller's behalf; for the Silicon that
    /// made the file and the Carbon who owns its device.
    pub async fn file_content(&self, id: &str) -> Result<FileContent> {
        self.file_download(id, None).await?.content().await
    }

    /// Starts reading a file's bytes, optionally one range (`first` byte, and `last` inclusive or
    /// to the end), to take in chunks with [`FileDownload::chunk`] (large recordings) or at once
    /// with [`FileDownload::content`].
    pub async fn file_download(&self, id: &str, range: Option<(u64, Option<u64>)>) -> Result<FileDownload> {
        let path = format!("/api/v1/files/{id}/content");
        let mut r = self.req(Method::GET, &path);
        if let Some((first, last)) = range {
            let value = match last {
                Some(last) => format!("bytes={first}-{last}"),
                None => format!("bytes={first}-"),
            };
            r = r.header("range", value);
        }
        let resp = self.c.send(r).await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            decode::<serde_json::Value>(resp).await?;
            return Err(Error::Decode {
                status,
                detail: format!("reading file {id} failed"),
            });
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        Ok(FileDownload {
            content_type: header("content-type").unwrap_or_else(|| "application/octet-stream".into()),
            name: header("content-disposition").as_deref().and_then(disposition_name),
            length: header("content-length").and_then(|v| v.parse().ok()),
            range: header("content-range").as_deref().and_then(content_range),
            url: format!("{}{path}", self.c.base),
            resp,
        })
    }

    pub async fn report(&self, input: &ReportInput) -> Result<ReportReceipt> {
        self.post("/api/v1/reports", "report", input, true).await
    }

    /// Sends one telemetry event (fields listed in api.yaml); errors are the caller's to ignore.
    pub async fn telemetry(&self, event: serde_json::Value) -> Result<()> {
        let r = self
            .req(Method::POST, "/api/v1/telemetry")
            .json(&env("telemetry", event));
        expect_empty(self.c.send(r).await?).await
    }
}

/// A file's bytes, read through Extend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    pub bytes: Vec<u8>,
    pub content_type: String,
    /// The file's name, from `Content-Disposition`.
    pub name: Option<String>,
    /// For a partial answer: (first byte, last byte, total size).
    pub range: Option<(u64, u64, u64)>,
}

/// A file being read through Extend, taken in chunks or all at once.
#[derive(Debug)]
pub struct FileDownload {
    resp: reqwest::Response,
    url: String,
    pub content_type: String,
    /// The file's name, from `Content-Disposition`.
    pub name: Option<String>,
    /// Bytes in this answer (the whole file, or the range asked for).
    pub length: Option<u64>,
    /// For a partial answer: (first byte, last byte, total size).
    pub range: Option<(u64, u64, u64)>,
}

impl FileDownload {
    /// The next chunk, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        self.resp
            .chunk()
            .await
            .map(|c| c.map(|b| b.to_vec()))
            .map_err(|e| Error::Transport {
                url: self.url.clone(),
                source: e,
            })
    }

    /// Everything that is left.
    pub async fn content(self) -> Result<FileContent> {
        let bytes = self.resp.bytes().await.map_err(|e| Error::Transport {
            url: self.url.clone(),
            source: e,
        })?;
        Ok(FileContent {
            bytes: bytes.to_vec(),
            content_type: self.content_type,
            name: self.name,
            range: self.range,
        })
    }
}

/// The file name in a `Content-Disposition` header: the RFC 8187 `filename*` when present,
/// else `filename`.
fn disposition_name(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        if let Some(v) = part.strip_prefix("filename*=") {
            let encoded = v.split_once("''").map_or(v, |(_, e)| e);
            let mut out = Vec::new();
            let bytes = encoded.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%'
                    && let Some(b) = encoded.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            if let Ok(s) = String::from_utf8(out) {
                return Some(s);
            }
        } else if let Some(v) = part.strip_prefix("filename=") {
            plain = Some(v.trim_matches('"').to_owned());
        }
    }
    plain
}

/// `bytes first-last/total` → (first, last, total).
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (span, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    Some((first.parse().ok()?, last.parse().ok()?, total.parse().ok()?))
}

/// True when an error means the access token needs refreshing.
pub fn needs_refresh(e: &Error) -> bool {
    matches!(e, Error::Api { status, error } if *status == StatusCode::UNAUTHORIZED.as_u16() && matches!(error.code, ErrorCode::TokenExpired | ErrorCode::NotSignedIn))
}

mod sha2_lite {
    use sha2::{Digest as _, Sha256};
    pub fn sha256_hex(bytes: &[u8]) -> String {
        extend_protocol::ids::hex_lower(&Sha256::digest(bytes))
    }
}
