//! The official Rust client for Silicon Extend.
//!
//! Stateless: a [`Client`] holds a base URL, an HTTP connection pool and the negotiated API version.
//! Tokens are passed in per call through [`Authed`]; where they're stored is the caller's business
//! (the `extend` CLI keeps them under `SILICON_HOME`).
//!
//! - Every Carbon and Silicon signs in with **Silicon Accounts**. The [`auth`] module does it the
//!   way Extend's own tools do: the device flow for Carbons, a short-lived token for Silicons, and
//!   rotating refresh tokens. The access token goes to [`Client::authed`].
//! - The account API is **API v2** (`/api/v2/…`): devices, access, sessions, commands, requests,
//!   waking, files, and the custodian views of the Silicons a Carbon looks after. A device belongs
//!   to the Carbon who paired it, and a Silicon uses what it was given access to.
//! - The device wire (`/api/v1/device…`, `/api/v1/enrollments…`) is unchanged for the Extend apps on
//!   devices; [`Client::enroll`] and the other device-side calls speak it.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use silicon_extend_client::{Client, DeviceQuery, auth::SignIn};
//! let sign_in = SignIn::new("https://accounts.teamofsilicons.com")?;
//! let tokens = sign_in.exchange_slt("slt_…").await?; // from `silicon-accounts login --app extend -q`
//! let client = Client::connect("https://backend.extend.teamofsilicons.com").await?;
//! let me = client.authed(tokens.access_token.expose());
//! for device in me.devices(DeviceQuery::default()).await?.items {
//!     println!("{} {} online={}", device.device_id, device.name, device.online);
//! }
//! # Ok(()) }
//! ```

use std::time::Duration;

pub mod attachments;
pub mod auth;
mod authed;

pub use authed::{ActivityQuery, Authed, DeviceQuery, FileContent, FileDownload, ListQuery, StopOutcome};
pub use extend_protocol as protocol;
use extend_protocol::account::AccountsInfo;
use extend_protocol::model::*;
use extend_protocol::{
    ACCOUNT_API_VERSION, API_VERSION, API_VERSION_HEADER, ApiError, ErrorCode, SUPPORTED_VERSIONS_HEADER,
};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

/// API majors this crate agrees on for the account API: 2 (Silicon Accounts sign-in).
pub const SUPPORTED_API_VERSIONS: &[u32] = &[ACCOUNT_API_VERSION];
/// The device wire's major, pinned on every `/api/v1/…` call (the Extend apps' API).
pub const DEVICE_API_VERSION: u32 = API_VERSION;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Extend answered with an error envelope (`{"type":"error","data":{code,message,hint,…}}`).
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
    /// The error envelope Extend sent, with its message, hint, request id and details.
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Self::Api { error, .. } => Some(error),
            _ => None,
        }
    }
    /// The HTTP status, when Extend answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } | Self::Decode { status, .. } => Some(*status),
            _ => None,
        }
    }
    pub fn is_code(&self, code: ErrorCode) -> bool {
        self.code() == code
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base_url: String,
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

    /// Builds the client and negotiates the API version with the service (`GET /api/version`).
    /// A service that doesn't speak API v2 (Extend 3 and older) answers `api_version_unsupported`.
    pub async fn connect(self) -> Result<Client> {
        let base = auth::check_url(&self.base_url, "The Extend service URL").map_err(Error::Invalid)?;
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
            api_version: ACCOUNT_API_VERSION,
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
    api_version: u32,
    telemetry: bool,
    isi: Option<String>,
}

/// A successful answer's envelope: the HTTP status, the envelope `type` and its `data`.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) kind: Option<String>,
    pub(crate) data: serde_json::Value,
}

impl Answer {
    pub(crate) fn into<T: DeserializeOwned>(self) -> Result<T> {
        serde_json::from_value(self.data).map_err(|e| Error::Decode {
            status: self.status,
            detail: e.to_string(),
        })
    }
}

pub(crate) async fn decode_envelope(resp: reqwest::Response) -> Result<Answer> {
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

pub(crate) async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    decode_envelope(resp).await?.into()
}

pub(crate) async fn expect_empty(resp: reqwest::Response) -> Result<()> {
    if resp.status().is_success() {
        return Ok(());
    }
    decode::<serde_json::Value>(resp).await.map(|_| ())
}

pub(crate) fn env<T: Serialize>(kind: &str, data: T) -> serde_json::Value {
    serde_json::json!({"type": kind, "data": data})
}

impl Client {
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
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

    /// The account API major agreed with the service (2).
    pub fn api_version(&self) -> u32 {
        self.api_version
    }

    /// A request to `path`, pinned to the major its path names: the device wire (`/api/v1/…`)
    /// stays on 1, the account API on the agreed version.
    pub(crate) fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let pin = if path.starts_with("/api/v1/") {
            DEVICE_API_VERSION
        } else {
            self.api_version
        };
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(API_VERSION_HEADER, pin.to_string());
        if !self.telemetry {
            r = r.header("X-Extend-Telemetry", "off");
        }
        if let Some(isi) = &self.isi {
            r = r.header("X-Silicon-ISI", isi);
        }
        r
    }

    pub(crate) async fn send(&self, r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        r.send().await.map_err(|e| Error::Transport {
            url: self.base.clone(),
            source: e,
        })
    }

    /// Calls made as a signed-in Carbon or Silicon, with its Silicon Accounts access token for
    /// Extend (`aud` = `extend`; see [`auth`]).
    pub fn authed<'a>(&'a self, access_token: &'a str) -> Authed<'a> {
        Authed::new(self, access_token)
    }

    /// Where Extend's accounts come from (no sign-in, `GET /api/v2/accounts`): its app id in
    /// Silicon Accounts, the Silicon Accounts it trusts (`accounts_url`, the `iss` of every token it
    /// accepts), its links, and whether it delivers notifications through Ting.
    pub async fn accounts(&self) -> Result<AccountsInfo> {
        decode(self.send(self.req(Method::GET, "/api/v2/accounts")).await?).await
    }

    /// The compatibility matrix (`GET /api/v2/contracts`): every API major's state and the client,
    /// CLI and device app versions that work with it.
    pub async fn contracts(&self) -> Result<serde_json::Value> {
        decode(self.send(self.req(Method::GET, "/api/v2/contracts")).await?).await
    }

    // ── Device side (the Extend apps on devices; API v1, unchanged) ──

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

/// True when an error means the access token needs refreshing: Extend refused it as expired
/// (`401 token_expired`). Refresh once with [`auth::SignIn::refresh`] and retry; if the refresh is
/// refused too, the sign-in is over.
pub fn needs_refresh(e: &Error) -> bool {
    matches!(e, Error::Api { status, error } if *status == StatusCode::UNAUTHORIZED.as_u16() && error.code == ErrorCode::TokenExpired)
}

mod sha2_lite {
    use sha2::{Digest as _, Sha256};
    pub fn sha256_hex(bytes: &[u8]) -> String {
        extend_protocol::ids::hex_lower(&Sha256::digest(bytes))
    }
}
