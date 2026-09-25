//! The official Rust client for Silicon Bridge.
//!
//! Stateless: it holds a base URL, an HTTP connection pool, the negotiated API version and
//! (optionally) a test-environment secret. Tokens are passed in per call through [`Authed`]; where
//! they're stored is the caller's business (the `bridge` CLI keeps them under `SILICON_HOME`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), silicon_bridge_client::Error> {
//! use silicon_bridge_client::Client;
//! let client = Client::connect("https://backend.bridge.teamofsilicons.com").await?;
//! let session = client.login("<short-lived token from Silicon IAM>").await?;
//! let me = client.authed(&session.access_token, Some("acme"));
//! for device in me.devices(Default::default()).await?.items {
//!     println!("{} {} online={}", device.device_id, device.name, device.online);
//! }
//! # Ok(()) }
//! ```

use std::time::Duration;

pub use bridge_protocol as protocol;
use bridge_protocol::envelope::Page;
use bridge_protocol::model::*;
use bridge_protocol::{API_VERSION, API_VERSION_HEADER, ApiError, DeviceId, ErrorCode, SUPPORTED_VERSIONS_HEADER, TEAM_HEADER, TESTING_SECRET_HEADER};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

/// Versions of the API this crate speaks.
pub const SUPPORTED_API_VERSIONS: &[u32] = &[API_VERSION];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bridge answered with an error envelope.
    #[error("{} ({}): {}", .error.message, .error.code.as_str(), .error.hint.clone().unwrap_or_default())]
    Api { status: u16, error: Box<ApiError> },
    /// The request never got a usable answer.
    #[error("could not reach Silicon Bridge at {url}: {source}")]
    Transport { url: String, source: reqwest::Error },
    /// Bridge answered with something this client can't read.
    #[error("unexpected response from Silicon Bridge ({status}): {detail}")]
    Decode { status: u16, detail: String },
    #[error("invalid input: {0}")]
    Invalid(String),
}

impl Error {
    /// The stable error code, when Bridge sent one.
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
}

impl ClientBuilder {
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
    /// Sends `X-Bridge-Telemetry: off` when false.
    pub fn telemetry(mut self, on: bool) -> Self {
        self.telemetry = on;
        self
    }

    /// Builds the client and negotiates the API version with the service.
    pub async fn connect(self) -> Result<Client> {
        let base = self.base_url.trim_end_matches('/').to_owned();
        if !(base.starts_with("https://") || base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost") || base.starts_with("http://10.0.2.2") || base.starts_with("http://[::1]")) {
            return Err(Error::Invalid(format!("{base} must be https (plain http is allowed only for local addresses)")));
        }
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent(self.user_agent.clone())
            .build()
            .map_err(|e| Error::Transport { url: base.clone(), source: e })?;
        let mut client = Client { http, base, testing_secret: self.testing_secret, api_version: API_VERSION, telemetry: self.telemetry };
        let supported: Vec<String> = SUPPORTED_API_VERSIONS.iter().map(u32::to_string).collect();
        let resp = client
            .http
            .get(format!("{}/api/version", client.base))
            .header(SUPPORTED_VERSIONS_HEADER, supported.join(", "))
            .send()
            .await
            .map_err(|e| Error::Transport { url: client.base.clone(), source: e })?;
        let info: VersionInfo = decode(resp).await?;
        client.api_version = info.api_version;
        Ok(client)
    }
}

/// A connection to Silicon Bridge.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    testing_secret: Option<String>,
    api_version: u32,
    telemetry: bool,
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| Error::Decode { status: status.as_u16(), detail: e.to_string() })?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Error::Decode {
        status: status.as_u16(),
        detail: String::from_utf8_lossy(&bytes).chars().take(300).collect(),
    })?;
    if !status.is_success() || v.get("type").and_then(|t| t.as_str()) == Some("error") {
        let error: ApiError = serde_json::from_value(v.get("data").cloned().unwrap_or_default())
            .map_err(|e| Error::Decode { status: status.as_u16(), detail: format!("error body: {e}") })?;
        return Err(Error::Api { status: status.as_u16(), error: Box::new(error) });
    }
    let data = v.get("data").cloned().unwrap_or(serde_json::Value::Null);
    serde_json::from_value(data).map_err(|e| Error::Decode { status: status.as_u16(), detail: e.to_string() })
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
            user_agent: concat!("silicon-bridge-client/", env!("CARGO_PKG_VERSION")).into(),
            telemetry: true,
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
        let mut r = self.http.request(method, format!("{}{path}", self.base)).header(API_VERSION_HEADER, self.api_version.to_string());
        if let Some(s) = &self.testing_secret {
            r = r.header(TESTING_SECRET_HEADER, s);
        }
        if !self.telemetry {
            r = r.header("X-Bridge-Telemetry", "off");
        }
        r
    }

    async fn send(&self, r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        r.send().await.map_err(|e| Error::Transport { url: self.base.clone(), source: e })
    }

    /// Scopes calls to a signed-in member and (optionally) one team.
    pub fn authed<'a>(&'a self, access_token: &'a str, team: Option<&'a str>) -> Authed<'a> {
        Authed { c: self, token: access_token, team }
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
        let r = self.req(Method::POST, "/api/v1/auth/login").header("Idempotency-Key", Uuid::new_v4().to_string()).json(&env("login", LoginInput { slt: slt.to_owned() }));
        decode(self.send(r).await?).await
    }

    pub async fn refresh(&self, refresh_token: &str, idempotency_key: &str) -> Result<AuthSession> {
        let r = self
            .req(Method::POST, "/api/v1/auth/refresh")
            .header("Idempotency-Key", idempotency_key)
            .json(&env("refresh", RefreshInput { refresh_token: refresh_token.to_owned() }));
        decode(self.send(r).await?).await
    }

    pub async fn logout(&self, token: &str, access_token: Option<&str>) -> Result<()> {
        let mut r = self.req(Method::POST, "/api/v1/auth/logout").header("Idempotency-Key", Uuid::new_v4().to_string()).json(&env("logout", LogoutInput { token: token.to_owned() }));
        if let Some(a) = access_token {
            r = r.bearer_auth(a);
        }
        expect_empty(self.send(r).await?).await
    }

    /// The test environment the client's secret selects.
    pub async fn testing_environment(&self) -> Result<TestingEnvironment> {
        decode(self.send(self.req(Method::GET, "/api/v1/testing-environment")).await?).await
    }

    // ── Device side (Bridge apps) ──

    pub async fn enroll(&self, input: &EnrollmentCreate) -> Result<EnrollmentCreated> {
        decode(self.send(self.req(Method::POST, "/api/v1/enrollments").json(&env("enrollment", input))).await?).await
    }

    pub async fn enrollment(&self, id: Uuid, secret: &str) -> Result<EnrollmentState> {
        let r = self.req(Method::GET, &format!("/api/v1/enrollments/{id}")).header("authorization", format!("Bridge-Enrollment {secret}"));
        decode(self.send(r).await?).await
    }

    pub async fn device_self(&self, credential: &str) -> Result<DeviceSelf> {
        let r = self.req(Method::GET, "/api/v1/device").header("authorization", format!("Bridge-Device {credential}"));
        decode(self.send(r).await?).await
    }

    pub async fn revoke_pair(&self, credential: &str) -> Result<()> {
        let r = self.req(Method::DELETE, "/api/v1/device").header("authorization", format!("Bridge-Device {credential}"));
        expect_empty(self.send(r).await?).await
    }

    pub async fn upload_artifact(&self, credential: &str, upload_id: Uuid, name: &str, content_type: &str, bytes: Vec<u8>) -> Result<()> {
        use sha2_lite::sha256_hex;
        let r = self
            .req(Method::PUT, &format!("/api/v1/device/artifacts/{upload_id}"))
            .header("authorization", format!("Bridge-Device {credential}"))
            .header("content-type", content_type)
            .header("x-file-name", name)
            .header("x-content-sha256", sha256_hex(&bytes))
            .body(bytes);
        expect_empty(self.send(r).await?).await
    }

    /// The WebSocket URL for a path (`/api/v1/device/connect`).
    pub fn ws_url(&self, path: &str) -> String {
        let base = self.base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        format!("{base}{path}")
    }

    /// Downloads a file by its URL (a Briefcase link, or a local development link).
    pub async fn download(&self, url: &str, access_token: &str) -> Result<Vec<u8>> {
        let resp = self.send(self.http.get(url).bearer_auth(access_token)).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Decode { status: status.as_u16(), detail: format!("downloading {url} failed") });
        }
        Ok(resp.bytes().await.map_err(|e| Error::Transport { url: url.to_owned(), source: e })?.to_vec())
    }
}

/// Filters for listing devices.
#[derive(Debug, Clone, Default)]
pub struct DeviceQuery {
    /// `mine`, `team` (Carbons) or `accessible` (Silicons). Default depends on the member.
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
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>())))
        .collect();
    if parts.is_empty() { String::new() } else { format!("?{}", parts.join("&")) }
}

/// Calls made as a signed-in member.
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
            qs(&[("scope", q.scope), ("online", q.online.map(|b| b.to_string())), ("os", q.os), ("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)])
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
        self.post(&format!("/api/v1/devices/{host_id}/attachments"), "attachment", input, true).await
    }

    pub async fn update_device(&self, id: &str, version: Option<i64>, patch: &DevicePatch) -> Result<Device> {
        let mut r = self.req(Method::PATCH, &format!("/api/v1/devices/{id}")).json(&env("device", patch));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        decode(self.c.send(r).await?).await
    }

    pub async fn remove_device(&self, id: &str, version: Option<i64>) -> Result<()> {
        let mut r = self.req(Method::DELETE, &format!("/api/v1/devices/{id}"));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        expect_empty(self.c.send(r).await?).await
    }

    pub async fn stop_device(&self, id: &str) -> Result<Session> {
        self.post(&format!("/api/v1/devices/{id}/stop"), "stop", serde_json::json!({}), false).await
    }

    pub async fn setup(&self, id: &str) -> Result<Setup> {
        self.get(&format!("/api/v1/devices/{id}/setup")).await
    }

    pub async fn setup_code(&self, id: &str, code: &str) -> Result<Setup> {
        self.post(&format!("/api/v1/devices/{id}/setup/code"), "setup_code", serde_json::json!({"code": code}), false).await
    }

    pub async fn access(&self, id: &str) -> Result<Vec<AccessGrant>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<AccessGrant>,
        }
        Ok(self.get::<Items>(&format!("/api/v1/devices/{id}/access")).await?.items)
    }

    pub async fn grant(&self, id: &str, silicon_id: &str) -> Result<AccessGrant> {
        decode(self.c.send(self.req(Method::PUT, &format!("/api/v1/devices/{id}/access/{silicon_id}"))).await?).await
    }

    pub async fn revoke(&self, id: &str, silicon_id: &str) -> Result<()> {
        expect_empty(self.c.send(self.req(Method::DELETE, &format!("/api/v1/devices/{id}/access/{silicon_id}"))).await?).await
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
        self.get(&format!("/api/v1/devices/{id}/requests{}", qs(&[("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)]))).await
    }

    pub async fn send_request(&self, device_id: &str, reason: &str) -> Result<RequestInfo> {
        self.post(&format!("/api/v1/devices/{device_id}/requests"), "request", RequestCreate { reason: reason.to_owned() }, true).await
    }

    pub async fn requests(&self, q: ListQuery) -> Result<Page<RequestInfo>> {
        self.get(&format!(
            "/api/v1/requests{}",
            qs(&[("direction", q.direction), ("device_id", q.device_id), ("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)])
        ))
        .await
    }

    pub async fn start_session(&self, device_id: &DeviceId) -> Result<Session> {
        self.post("/api/v1/sessions", "session", SessionCreate { device_id: device_id.clone() }, true).await
    }

    pub async fn sessions(&self, q: ListQuery) -> Result<Page<Session>> {
        self.get(&format!(
            "/api/v1/sessions{}",
            qs(&[("device_id", q.device_id), ("state", q.state), ("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)])
        ))
        .await
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        self.get(&format!("/api/v1/sessions/{id}")).await
    }

    pub async fn end_session(&self, id: &str) -> Result<Session> {
        self.post(&format!("/api/v1/sessions/{id}/end"), "end", serde_json::json!({}), false).await
    }

    pub async fn takeover(&self, id: &str, reason: &str) -> Result<Takeover> {
        self.post(&format!("/api/v1/sessions/{id}/takeover"), "takeover", TakeoverCreate { reason: reason.to_owned() }, false).await
    }

    pub async fn takeover_status(&self, id: &str) -> Result<Option<Takeover>> {
        self.get(&format!("/api/v1/sessions/{id}/takeover")).await
    }

    pub async fn release_takeover(&self, id: &str) -> Result<()> {
        expect_empty(self.c.send(self.req(Method::DELETE, &format!("/api/v1/sessions/{id}/takeover"))).await?).await
    }

    /// Runs one command in a session and waits for the device's answer.
    pub async fn run(&self, session_id: &str, cmd: &CommandRequest) -> Result<CommandResult> {
        self.post(&format!("/api/v1/sessions/{session_id}/commands"), "command", cmd, false).await
    }

    pub async fn files(&self, q: ListQuery) -> Result<Page<FileInfo>> {
        self.get(&format!(
            "/api/v1/files{}",
            qs(&[("session_id", q.session_id), ("device_id", q.device_id), ("kind", q.kind), ("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)])
        ))
        .await
    }

    pub async fn file(&self, id: &str) -> Result<FileInfo> {
        self.get(&format!("/api/v1/files/{id}")).await
    }

    pub async fn keep_file(&self, id: &str) -> Result<FileInfo> {
        self.post(&format!("/api/v1/files/{id}/keep"), "keep", serde_json::json!({}), false).await
    }

    pub async fn report(&self, input: &ReportInput) -> Result<ReportReceipt> {
        self.post("/api/v1/reports", "report", input, true).await
    }

    /// Sends one telemetry event (fields listed in api.yaml); errors are the caller's to ignore.
    pub async fn telemetry(&self, event: serde_json::Value) -> Result<()> {
        let r = self.req(Method::POST, "/api/v1/telemetry").json(&env("telemetry", event));
        expect_empty(self.c.send(r).await?).await
    }
}

/// True when an error means the access token needs refreshing.
pub fn needs_refresh(e: &Error) -> bool {
    matches!(e, Error::Api { status, error } if *status == StatusCode::UNAUTHORIZED.as_u16() && matches!(error.code, ErrorCode::TokenExpired | ErrorCode::NotSignedIn))
}

mod sha2_lite {
    use sha2::{Digest as _, Sha256};
    pub fn sha256_hex(bytes: &[u8]) -> String {
        bridge_protocol::ids::hex_lower(&Sha256::digest(bytes))
    }
}
