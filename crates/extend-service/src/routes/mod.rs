//! HTTP and WebSocket routes (`understanding/api.yaml`; the 4.0 review copy is
//! `docs/migration/contracts/api.yaml`).
//!
//! - `/api/v1/device…` and `/api/v1/enrollments…`: the device wire installed Extend apps speak,
//!   unchanged from 1.x (device credentials).
//! - `/api/v2/…`: everything a Carbon or a Silicon does, signed in with Silicon Accounts
//!   (`Authorization: Bearer <access token>`). No Teams.
//! - Any other `/api/v1/…` route answers 410: it belonged to the Silicon IAM sign-in.
//! - `POST /webhooks/accounts`: Silicon Accounts' signed events.

mod accounts;
mod auth;
mod dev;
mod device_app;
mod devices;
mod display_files;
pub mod enroll;
mod files;
mod idempotency;
pub use idempotency::idempotent;
mod ops;
pub mod sessions;
mod silicons;
mod system;
pub mod wake;
mod webhook;

use std::sync::Arc;

use axum::extract::{FromRequest, Request};
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, patch, post, put};
use axum::{Json, Router};
use extend_protocol::ErrorCode;
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use uuid::Uuid;

use crate::error::{AppError, AppResult, REQUEST_ID};
use crate::state::Shared;
use crate::versions::Registry;

/// The exact hint every retired `/api/v1` account route gives.
pub const UPDATE_HINT: &str = "silicon-apps update extend";

/// Every route, behind the version layer. Each API major is mounted under its own `/api/v{n}`
/// prefix (see crate::versions).
pub fn router(state: Shared, versions: Arc<Registry>) -> Router {
    let api = Router::new()
        // ── The device wire (API v1), unchanged for installed apps ──
        .route("/api/v1/enrollments", post(enroll::create))
        .route(
            "/api/v1/enrollments/{enrollment_id}",
            get(enroll::get).delete(enroll::discard),
        )
        .route("/api/v1/enrollments/{enrollment_id}/connect", get(enroll::socket))
        .route(
            "/api/v1/device",
            get(device_app::me).patch(device_app::update).delete(device_app::revoke),
        )
        .route(
            "/api/v1/device/attachments/{device_id}",
            patch(device_app::update_attached),
        )
        .route("/api/v1/device/stop", post(device_app::stop))
        .route("/api/v1/device/enrollments", post(device_app::enrollments_create))
        .route("/api/v1/device/connect", get(device_app::socket))
        .route("/api/v1/device/artifacts/{upload_id}", put(device_app::upload))
        .route("/api/v1/contracts", get(system::contracts))
        .route("/api/v1/{*rest}", any(retired_v1))
        // ── The account API (v2): Silicon Accounts sign-in, no Teams ──
        .route("/api/v2/contracts", get(system::contracts))
        .route("/api/v2/accounts", get(system::accounts))
        .route("/api/v2/accounts/lookup", get(accounts::lookup))
        .route("/api/v2/me", get(auth::me))
        .route("/api/v2/auth/logout", post(auth::logout))
        .route("/api/v2/pairings", post(devices::claim))
        .route("/api/v2/devices", get(devices::list))
        .route(
            "/api/v2/devices/{device_id}",
            get(devices::get).patch(devices::update).delete(devices::remove),
        )
        .route("/api/v2/devices/{device_id}/stop", post(devices::stop))
        .route("/api/v2/devices/{device_id}/attachments", post(devices::attach))
        .route("/api/v2/devices/{device_id}/setup", get(devices::setup))
        .route("/api/v2/devices/{device_id}/setup/code", post(devices::setup_code))
        .route("/api/v2/devices/{device_id}/setup/retry", post(devices::setup_retry))
        .route(
            "/api/v2/devices/{device_id}/wake-requests",
            post(wake::create).get(wake::list),
        )
        .route("/api/v2/devices/{device_id}/wake-requests/answer", post(wake::answer))
        .route(
            "/api/v2/devices/{device_id}/wake-requests/{wake_id}",
            delete(wake::cancel),
        )
        .route("/api/v2/devices/{device_id}/wake-settings", put(wake::settings))
        .route("/api/v2/devices/{device_id}/access", get(devices::access_list))
        .route(
            "/api/v2/devices/{device_id}/access/{silicon}",
            put(devices::access_grant).delete(devices::access_revoke),
        )
        .route("/api/v2/devices/{device_id}/activity", get(devices::activity))
        .route(
            "/api/v2/devices/{device_id}/requests",
            get(devices::requests_for_device).post(devices::request_send),
        )
        .route("/api/v2/requests", get(devices::my_requests))
        .route("/api/v2/silicons", get(silicons::list))
        .route("/api/v2/silicons/{silicon}", get(silicons::show))
        .route("/api/v2/silicons/{silicon}/grants", get(silicons::grants))
        .route(
            "/api/v2/silicons/{silicon}/grants/{device_id}",
            delete(silicons::renounce),
        )
        .route("/api/v2/ting-registration", get(wake::ting_get).put(wake::ting_turn_on))
        .route("/api/v2/sessions", post(sessions::start).get(sessions::list))
        .route("/api/v2/sessions/{session_id}", get(sessions::get))
        .route("/api/v2/sessions/{session_id}/end", post(sessions::end))
        .route(
            "/api/v2/sessions/{session_id}/takeover",
            post(sessions::takeover_start)
                .get(sessions::takeover_get)
                .delete(sessions::takeover_release),
        )
        .route("/api/v2/sessions/{session_id}/commands", post(sessions::command))
        .route("/api/v2/files", get(files::list))
        .route("/api/v2/files/{file_id}", get(files::get))
        .route("/api/v2/files/{file_id}/keep", post(files::keep))
        .route("/api/v2/files/{file_id}/content", get(files::content))
        .route("/api/v2/reports", post(ops::report))
        .route("/api/v2/telemetry", post(ops::telemetry))
        // ── Silicon Accounts' events ──
        .route("/webhooks/accounts", post(webhook::receive))
        .route("/webhook/", post(webhook::retired))
        .route("/webhook", post(webhook::retired))
        // ── Development stand-ins (refused in production) ──
        .route("/dev/files/{file_id}", get(files::local_file))
        .route("/dev/accounts/token", post(dev::token))
        .route("/dev/accounts/.well-known/jwks.json", get(dev::jwks))
        .route("/dev/ting", get(dev::tings))
        .route("/dev/ting/missing", post(dev::ting_missing))
        .fallback(any(fallback))
        .layer(axum::middleware::from_fn(crate::state::no_test_environments))
        .layer(axum::middleware::from_fn_with_state(
            versions.clone(),
            crate::versions::layer,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            extend_protocol::MAX_ARTIFACT_BYTES as usize + 1024,
        ));

    let mut app = Router::new()
        .route("/live", get(system::live))
        .route("/ready", get(system::ready))
        .route("/api/version", get(system::negotiate))
        .merge(api);
    if let Some(dir) = state.cfg.web_dir.clone() {
        let index = dir.join("index.html");
        app = app.fallback_service(
            tower_http::services::ServeDir::new(dir).fallback(tower_http::services::ServeFile::new(index)),
        );
    }
    // Browsers reach the API through the website's server, which adds the access token; only the
    // origins in EXTEND_CORS_ORIGINS may call it from a page directly.
    let origins: Vec<HeaderValue> = state
        .cfg
        .cors_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    app.layer(axum::Extension(versions))
        .layer(axum::middleware::from_fn(request_id_layer))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods(AllowMethods::mirror_request())
                .allow_headers(AllowHeaders::mirror_request())
                .expose_headers([
                    http::header::ETAG,
                    http::header::CONTENT_DISPOSITION,
                    http::header::CONTENT_RANGE,
                    http::HeaderName::from_static("x-request-id"),
                    http::HeaderName::from_static("silicon-extend-api-version"),
                    http::HeaderName::from_static(crate::versions::DEPRECATION_HEADER),
                    http::HeaderName::from_static(crate::versions::SUNSET_HEADER),
                ])
                .max_age(std::time::Duration::from_secs(600)),
        )
        .with_state(state)
}

/// Every `/api/v1` route that isn't the device wire: it belonged to the Silicon IAM sign-in, which
/// 4.0 replaced with Silicon Accounts (`/api/v2`).
async fn retired_v1(method: Method, uri: Uri) -> Response {
    let mut resp = AppError::new(
        ErrorCode::ApiVersionSunset,
        format!(
            "{method} {} is retired: Silicon Extend 4 signs in with Silicon Accounts, and every account route moved to \
             /api/v2. This client is older than that. (Device apps keep using /api/v1/device and /api/v1/enrollments.)",
            uri.path()
        ),
    )
    .hint(UPDATE_HINT)
    .details(serde_json::json!({"retired": uri.path(), "use_api_version": extend_protocol::ACCOUNT_API_VERSION}))
    .into_response();
    *resp.status_mut() = StatusCode::GONE;
    resp
}

async fn fallback() -> AppError {
    AppError::new(ErrorCode::UnknownCommand, "No such endpoint in the Extend API.")
        .hint("The API is described at docs/migration/contracts/api.yaml (Extend 4) in the repository.")
}

/// Stamps a request id on every response and every error.
async fn request_id_layer(req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let mut resp = REQUEST_ID.scope(id.clone(), next.run(req)).await;
    if let Ok(v) = HeaderValue::from_str(&id) {
        resp.headers_mut().insert("x-request-id", v);
    }
    resp
}

/// A JSON envelope body `{"type": <kind>, "data": T}` with precise errors.
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> AppResult<Self> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| AppError::invalid(format!("Could not read the request body: {e}")))?;
        parse_envelope(&bytes).map(Body)
    }
}

pub fn parse_envelope<T: DeserializeOwned>(bytes: &[u8]) -> AppResult<T> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        AppError::invalid(format!("The body is not valid JSON: {e}")).hint(r#"Send {"type": "<kind>", "data": {...}}."#)
    })?;
    let data = v
        .get("data")
        .cloned()
        .ok_or_else(|| AppError::invalid(r#"The body must be an envelope {"type": "<kind>", "data": {...}}."#))?;
    serde_json::from_value(data).map_err(|e| AppError::invalid(format!("The body's data is not valid: {e}")))
}

pub fn ok<T: serde::Serialize>(kind: &str, data: T) -> Response {
    envelope(StatusCode::OK, kind, data)
}

pub fn created<T: serde::Serialize>(kind: &str, data: T) -> Response {
    envelope(StatusCode::CREATED, kind, data)
}

pub fn envelope<T: serde::Serialize>(status: StatusCode, kind: &str, data: T) -> Response {
    let mut resp = (status, Json(serde_json::json!({"type": kind, "data": data}))).into_response();
    resp.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    resp
}

pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub fn hash_json<T: serde::Serialize>(v: &T) -> String {
    extend_protocol::ids::hex_lower(&Sha256::digest(serde_json::to_vec(v).unwrap_or_default()))
}

/// Pagination cursors are opaque: base64 of the last item's sort key.
pub fn encode_cursor(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

pub fn decode_cursor(s: &str) -> AppResult<String> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| AppError::invalid("cursor is not one this API returned."))
}

pub fn limit(l: Option<i64>) -> AppResult<i64> {
    match l {
        None => Ok(50),
        Some(n) if (1..=100).contains(&n) => Ok(n),
        Some(n) => Err(AppError::invalid(format!("limit must be 1–100, got {n}."))),
    }
}
