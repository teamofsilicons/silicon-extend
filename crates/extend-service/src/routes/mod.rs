//! HTTP and WebSocket routes (`understanding/api.yaml`).

mod auth;
mod dev;
mod device_app;
mod devices;
pub mod enroll;
mod files;
mod ops;
pub mod sessions;
mod system;
mod testing;
mod webhook;

use axum::extract::{FromRequest, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use extend_protocol::{API_VERSION, API_VERSION_HEADER, ErrorCode};
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use uuid::Uuid;

use crate::db::World;
use crate::error::{AppError, AppResult, REQUEST_ID};
use crate::state::Shared;

pub fn router(state: Shared) -> Router {
    let api = Router::new()
        .route("/api/v1/contracts", get(system::contracts))
        .route("/api/v1/iam", get(system::iam))
        .route("/api/v1/auth/login", post(auth::login))
        .route("/api/v1/auth/refresh", post(auth::refresh))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/enrollments", post(enroll::create))
        .route("/api/v1/enrollments/{enrollment_id}", get(enroll::get).delete(enroll::discard))
        .route("/api/v1/enrollments/{enrollment_id}/connect", get(enroll::socket))
        .route("/api/v1/pairings", post(devices::claim))
        .route("/api/v1/devices", get(devices::list))
        .route("/api/v1/devices/{device_id}", get(devices::get).patch(devices::update).delete(devices::remove))
        .route("/api/v1/devices/{device_id}/stop", post(devices::stop))
        .route("/api/v1/devices/{device_id}/attachments", post(devices::attach))
        .route("/api/v1/devices/{device_id}/setup", get(devices::setup))
        .route("/api/v1/devices/{device_id}/setup/code", post(devices::setup_code))
        .route("/api/v1/devices/{device_id}/access", get(devices::access_list))
        .route("/api/v1/devices/{device_id}/access/{silicon_id}", put(devices::access_grant).delete(devices::access_revoke))
        .route("/api/v1/devices/{device_id}/activity", get(devices::activity))
        .route("/api/v1/devices/{device_id}/requests", get(devices::requests_for_device).post(devices::request_send))
        .route("/api/v1/requests", get(devices::my_requests))
        .route("/api/v1/team/silicons", get(devices::team_silicons))
        .route("/api/v1/sessions", post(sessions::start).get(sessions::list))
        .route("/api/v1/sessions/{session_id}", get(sessions::get))
        .route("/api/v1/sessions/{session_id}/end", post(sessions::end))
        .route("/api/v1/sessions/{session_id}/takeover", post(sessions::takeover_start).get(sessions::takeover_get).delete(sessions::takeover_release))
        .route("/api/v1/sessions/{session_id}/commands", post(sessions::command))
        .route("/api/v1/files", get(files::list))
        .route("/api/v1/files/{file_id}", get(files::get))
        .route("/api/v1/files/{file_id}/keep", post(files::keep))
        .route("/api/v1/device", get(device_app::me).delete(device_app::revoke))
        .route("/api/v1/device/stop", post(device_app::stop))
        .route("/api/v1/device/connect", get(device_app::socket))
        .route("/api/v1/device/artifacts/{upload_id}", put(device_app::upload))
        .route("/api/v1/testing-environment", get(testing::current))
        .route(
            "/internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}",
            put(testing::apply).get(testing::receipt),
        )
        .route("/webhook/", post(webhook::receive))
        .route("/webhook", post(webhook::receive))
        .route("/api/v1/reports", post(ops::report))
        .route("/api/v1/telemetry", post(ops::telemetry))
        .route("/dev/files/{file_id}", get(files::local_file))
        .route("/dev/iam/members", post(dev::member))
        .route("/dev/iam/test-apps", post(dev::test_app))
        .route("/dev/iam/authorize", get(dev::authorize_page).post(dev::authorize_submit))
        .route("/dev/iam/login", get(dev::authorize_page).post(dev::authorize_submit))
        .route("/dev/ting", get(dev::tings))
        .fallback(any(fallback))
        .layer(axum::middleware::from_fn_with_state(state.clone(), version_layer))
        .layer(axum::extract::DefaultBodyLimit::max(extend_protocol::MAX_ARTIFACT_BYTES as usize + 1024));

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
    app.layer(axum::middleware::from_fn(request_id_layer))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::mirror_request())
                .allow_methods(AllowMethods::mirror_request())
                .allow_headers(AllowHeaders::mirror_request())
                .expose_headers([
                    http::header::ETAG,
                    http::HeaderName::from_static("x-request-id"),
                    http::HeaderName::from_static("silicon-extend-api-version"),
                ])
                .max_age(std::time::Duration::from_secs(600)),
        )
        .with_state(state)
}

async fn fallback() -> AppError {
    AppError::new(ErrorCode::UnknownCommand, "No such endpoint in the Extend API.")
        .hint("The API is described at understanding/api.yaml in the repository.")
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

/// Checks a client's version pin, advertises the version, and counts usage for sunset decisions.
async fn version_layer(State(state): State<Shared>, req: Request, next: Next) -> Response {
    if let Some(pin) = req.headers().get(API_VERSION_HEADER).and_then(|v| v.to_str().ok())
        && pin.trim() != API_VERSION.to_string()
        && req.uri().path().starts_with("/api/v1/")
    {
        return AppError::new(
            ErrorCode::ApiVersionMismatch,
            format!("The client pinned API version {pin}, but this path is version {API_VERSION}."),
        )
        .hint("Negotiate with GET /api/version and use the matching path.")
        .into_response();
    }
    if req.uri().path().starts_with("/api/v1/") {
        let pool = state.pool.clone();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO extend_global.api_version_usage (api_version, day, requests) VALUES (1, current_date, 1)
                 ON CONFLICT (api_version, day) DO UPDATE SET requests = extend_global.api_version_usage.requests + 1",
            )
            .execute(&pool)
            .await;
        });
    }
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(API_VERSION_HEADER, HeaderValue::from(API_VERSION));
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

/// Replays a stored response when the same Idempotency-Key and body come back; refuses a changed body.
pub async fn idempotent<F, Fut>(
    state: &Shared,
    world: &World,
    principal: &str,
    route: &str,
    headers: &http::HeaderMap,
    body_hash: &str,
    run: F,
) -> AppResult<Response>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = AppResult<(StatusCode, &'static str, serde_json::Value)>>,
{
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(k) = &key {
        if k.len() < 8 || k.len() > 255 || !k.bytes().all(|b| (b'!'..=b'~').contains(&b)) {
            return Err(AppError::invalid("Idempotency-Key must be 8–255 printable characters."));
        }
        let found: Option<(String, i32, serde_json::Value)> = sqlx::query_as(sql!(
            "SELECT request_hash, status, response FROM {} WHERE principal = $1 AND route = $2 AND key = $3",
            world.t("idempotency")
        ))
        .bind(principal)
        .bind(route)
        .bind(k)
        .fetch_optional(&state.pool)
        .await?;
        if let Some((hash, status, response)) = found {
            if hash != body_hash {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "This Idempotency-Key was already used with a different body.",
                )
                .hint("Use a new key for a new request; reuse a key only to retry the identical request."));
            }
            let mut resp = (
                StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK),
                Json(response),
            )
                .into_response();
            resp.headers_mut()
                .insert("idempotency-replayed", HeaderValue::from_static("true"));
            return Ok(resp);
        }
    }
    let (status, kind, data) = run().await?;
    let body = serde_json::json!({"type": kind, "data": data});
    if let Some(k) = key {
        let _ = sqlx::query(sql!(
            "INSERT INTO {} (principal, route, key, request_hash, status, response) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            world.t("idempotency")
        ))
        .bind(principal)
        .bind(route)
        .bind(k)
        .bind(body_hash)
        .bind(i32::from(status.as_u16()))
        .bind(&body)
        .execute(&state.pool)
        .await;
    }
    let mut resp = (status, Json(body)).into_response();
    resp.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(resp)
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
