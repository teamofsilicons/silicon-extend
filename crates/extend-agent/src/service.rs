//! HTTP calls to the Extend service (`docs/device-protocol.md` sections 1-3).

use std::path::Path;
use std::time::Duration;

use extend_protocol::model::{DeviceSelf, EnrollmentCreate, EnrollmentCreated, EnrollmentState};
use extend_protocol::{API_VERSION, API_VERSION_HEADER, Envelope};
use rand::Rng as _;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;
use url::Url;
use uuid::Uuid;

use crate::config::APP_VERSION;

/// A failed call. `status` is the HTTP status when the service answered.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ServiceError {
    pub status: Option<u16>,
    pub code: Option<String>,
    pub message: String,
}

impl ServiceError {
    fn network(e: impl std::fmt::Display) -> Self {
        Self {
            status: None,
            code: None,
            message: format!("couldn't reach Extend: {e}"),
        }
    }
    /// The credential or enrollment is no longer valid.
    pub fn is_auth(&self) -> bool {
        matches!(self.status, Some(401) | Some(403))
    }
    pub fn is_gone(&self) -> bool {
        matches!(self.status, Some(404) | Some(410))
    }
    pub fn is_upgrade_required(&self) -> bool {
        self.status == Some(426)
    }
    /// Worth retrying: network trouble, rate limits and server errors.
    pub fn is_transient(&self) -> bool {
        match self.status {
            None => true,
            Some(s) => s == 408 || s == 429 || s >= 500,
        }
    }
}

pub type ServiceResult<T> = Result<T, ServiceError>;

#[derive(Clone)]
pub struct ServiceClient {
    http: reqwest::Client,
    base: Url,
}

pub fn user_agent() -> String {
    format!("SiliconExtend/{APP_VERSION} ({})", crate::sysinfo::device_os().as_str())
}

impl ServiceClient {
    pub fn new(base: Url) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("HTTP client");
        Self { http, base }
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub fn url(&self, path: &str) -> Url {
        self.base.join(path.trim_start_matches('/')).expect("valid path")
    }

    /// The WebSocket URL for `path` (`http` becomes `ws`, `https` becomes `wss`).
    pub fn ws_url(&self, path: &str) -> Url {
        let mut u = self.url(path);
        let scheme = if u.scheme() == "https" { "wss" } else { "ws" };
        u.set_scheme(scheme).expect("ws scheme");
        u
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header(API_VERSION_HEADER, API_VERSION.to_string())
            .timeout(Duration::from_secs(30))
    }

    pub async fn create_enrollment(&self, body: &EnrollmentCreate) -> ServiceResult<EnrollmentCreated> {
        let resp = self
            .request(reqwest::Method::POST, "api/v1/enrollments")
            .json(&Envelope::new("enrollment", body))
            .send()
            .await
            .map_err(ServiceError::network)?;
        read_envelope(resp).await
    }

    pub async fn get_enrollment(&self, id: Uuid, secret: &str) -> ServiceResult<EnrollmentState> {
        let resp = self
            .request(reqwest::Method::GET, &format!("api/v1/enrollments/{id}"))
            .header("Authorization", format!("Extend-Enrollment {secret}"))
            .send()
            .await
            .map_err(ServiceError::network)?;
        read_envelope(resp).await
    }

    pub async fn discard_enrollment(&self, id: Uuid, secret: &str) -> ServiceResult<()> {
        let resp = self
            .request(reqwest::Method::DELETE, &format!("api/v1/enrollments/{id}"))
            .header("Authorization", format!("Extend-Enrollment {secret}"))
            .send()
            .await
            .map_err(ServiceError::network)?;
        read_empty(resp).await
    }

    pub async fn device_self(&self, credential: &str) -> ServiceResult<DeviceSelf> {
        let resp = self
            .request(reqwest::Method::GET, "api/v1/device")
            .header("Authorization", device_auth(credential))
            .send()
            .await
            .map_err(ServiceError::network)?;
        read_envelope(resp).await
    }

    /// Revoke pair. The caller confirms with the Carbon first.
    pub async fn revoke_pair(&self, credential: &str) -> ServiceResult<()> {
        let resp = self
            .request(reqwest::Method::DELETE, "api/v1/device")
            .header("Authorization", device_auth(credential))
            .send()
            .await
            .map_err(ServiceError::network)?;
        read_empty(resp).await
    }

    /// Stop, for when the socket is down.
    pub async fn stop(&self, credential: &str) -> ServiceResult<()> {
        let resp = self
            .request(reqwest::Method::POST, "api/v1/device/stop")
            .header("Authorization", device_auth(credential))
            .send()
            .await
            .map_err(ServiceError::network)?;
        read_empty(resp).await
    }

    /// Uploads one produced file under `upload_id`. Retries network trouble and server errors
    /// (the upload id stays valid until the command's deadline). Returns the digest and size.
    pub async fn upload_artifact(
        &self,
        credential: &str,
        upload_id: Uuid,
        path: &Path,
        name: &str,
        content_type: &str,
    ) -> ServiceResult<(String, u64)> {
        let (digest, size) = file_digest(path).await.map_err(|e| ServiceError {
            status: None,
            code: Some("file_unreadable".into()),
            message: format!("couldn't read {}: {e}", path.display()),
        })?;
        if size > extend_protocol::MAX_ARTIFACT_BYTES {
            return Err(ServiceError {
                status: Some(413),
                code: Some("payload_too_large".into()),
                message: format!("{name} is {size} bytes; Extend accepts files up to 1 GiB"),
            });
        }
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let file = tokio::fs::File::open(path).await.map_err(ServiceError::network)?;
            let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
            let result = self
                .http
                .put(self.url(&format!("api/v1/device/artifacts/{upload_id}")))
                .header(API_VERSION_HEADER, API_VERSION.to_string())
                .header("Authorization", device_auth(credential))
                .header("Content-Type", content_type)
                .header("Content-Length", size)
                .header("X-Content-SHA256", &digest)
                .header("X-File-Name", header_safe_name(name))
                .timeout(Duration::from_secs(600))
                .body(body)
                .send()
                .await;
            let err = match result {
                Ok(resp) => match read_empty(resp).await {
                    Ok(()) => return Ok((digest, size)),
                    Err(e) => e,
                },
                Err(e) => ServiceError::network(e),
            };
            if !err.is_transient() || attempt >= 3 {
                return Err(err);
            }
            let wait = 500 * u64::from(attempt) + rand::rng().random_range(0..250);
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }
}

pub fn device_auth(credential: &str) -> String {
    format!("Extend-Device {credential}")
}

/// `X-File-Name` must be a header-safe value: printable ASCII only.
pub fn header_safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' })
        .collect();
    s.chars().take(255).collect()
}

/// SHA-256 (lowercase hex) and size of a file, read in chunks.
pub async fn file_digest(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((extend_protocol::ids::hex_lower(&hasher.finalize()), size))
}

async fn read_envelope<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> ServiceResult<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(ServiceError::network)?;
    if !status.is_success() {
        return Err(error_from(status.as_u16(), &bytes));
    }
    let env: Envelope<T> = serde_json::from_slice(&bytes).map_err(|e| ServiceError {
        status: Some(status.as_u16()),
        code: Some("bad_response".into()),
        message: format!("Extend answered with something this app doesn't understand: {e}"),
    })?;
    Ok(env.data)
}

async fn read_empty(resp: reqwest::Response) -> ServiceResult<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let bytes = resp.bytes().await.unwrap_or_default();
    Err(error_from(status.as_u16(), &bytes))
}

fn error_from(status: u16, body: &[u8]) -> ServiceError {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let data = parsed.as_ref().and_then(|v| v.get("data"));
    let code = data
        .and_then(|d| d.get("code"))
        .and_then(|c| c.as_str())
        .map(str::to_owned);
    let message = data
        .and_then(|d| d.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Extend answered HTTP {status}"));
    ServiceError {
        status: Some(status),
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_urls_follow_the_scheme() {
        let c = ServiceClient::new(Url::parse("https://backend.extend.teamofsilicons.com/").unwrap());
        assert_eq!(
            c.ws_url("/api/v1/device/connect").as_str(),
            "wss://backend.extend.teamofsilicons.com/api/v1/device/connect"
        );
        let c = ServiceClient::new(Url::parse("http://127.0.0.1:8480/").unwrap());
        assert_eq!(
            c.ws_url("api/v1/device/connect").as_str(),
            "ws://127.0.0.1:8480/api/v1/device/connect"
        );
    }

    #[test]
    fn errors_read_the_envelope() {
        let e = error_from(
            401,
            br#"{"type":"error","data":{"code":"unauthorized","message":"no"}}"#,
        );
        assert!(e.is_auth());
        assert_eq!(e.code.as_deref(), Some("unauthorized"));
        assert_eq!(e.message, "no");
        let e = error_from(502, b"<html>");
        assert!(e.is_transient());
        assert!(!error_from(404, b"").is_transient());
    }

    #[test]
    fn file_names_are_header_safe() {
        assert_eq!(header_safe_name("écran 1.png"), "_cran 1.png");
        assert_eq!(header_safe_name("a\nb"), "a_b");
    }

    #[tokio::test]
    async fn digests_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x");
        std::fs::write(&p, b"abc").unwrap();
        let (d, n) = file_digest(&p).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
