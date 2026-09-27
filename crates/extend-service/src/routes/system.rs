//! Health, version negotiation, contract discovery and public IAM details.

use std::sync::Arc;

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use extend_protocol::model::{IamInfo, TestingEnvironment, VersionInfo};
use extend_protocol::{API_VERSION_HEADER, ErrorCode, SUPPORTED_VERSIONS_HEADER, TESTING_SECRET_HEADER};

use super::ok;
use crate::error::{AppError, AppResult};
use crate::state::Shared;
use crate::versions::{Lifecycle, Registry, path_major};

pub async fn live() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn ready(State(state): State<Shared>) -> Response {
    match sqlx::query("SELECT 1 FROM extend_global.schema_versions LIMIT 1")
        .execute(&state.pool)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("database unavailable: {e}")).into_response(),
    }
}

pub async fn negotiate(Extension(versions): Extension<Arc<Registry>>, headers: HeaderMap) -> AppResult<Response> {
    // A client too old to send the header predates every major after 1.
    let raw = headers
        .get(SUPPORTED_VERSIONS_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("1");
    let theirs: Vec<u32> = raw.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let ours = versions.negotiable();
    let Some(agreed) = theirs.iter().copied().filter(|v| ours.contains(v)).max() else {
        // A client that only speaks retired majors is told why and what to do.
        if let Some(m) = theirs
            .iter()
            .filter_map(|v| versions.majors().iter().find(|m| m.api_version == *v).cloned())
            .filter(|m| m.state == Lifecycle::Sunset)
            .max_by_key(|m| m.api_version)
        {
            let theirs_list = theirs.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
            let mut resp = versions
                .sunset_error(
                    &m,
                    &format!("Extend no longer offers it. This client speaks only {theirs_list}."),
                )
                .into_response();
            resp.headers_mut()
                .insert("vary", HeaderValue::from_static(SUPPORTED_VERSIONS_HEADER));
            return Ok(resp);
        }
        return Err(AppError::new(
            ErrorCode::ApiVersionUnsupported,
            format!("No API version in common: the client supports {theirs:?}, Extend supports {ours:?}."),
        )
        .hint("Update the CLI with `honeycomb install 'extend'`.")
        .details(serde_json::json!({"client": theirs, "service": ours})));
    };
    let mut resp = ok(
        "version",
        VersionInfo {
            api_version: agreed,
            supported: ours,
            service_version: env!("CARGO_PKG_VERSION").into(),
            deprecated: versions.deprecated(),
        },
    );
    resp.headers_mut().insert(API_VERSION_HEADER, HeaderValue::from(agreed));
    resp.headers_mut()
        .insert("vary", HeaderValue::from_static(SUPPORTED_VERSIONS_HEADER));
    if let Some(m) = versions.major(agreed) {
        for (name, value) in versions.deprecation_headers(&m) {
            resp.headers_mut().insert(name, value);
        }
    }
    Ok(resp)
}

/// The compatibility matrix, built from the lifecycle state and configuration (see
/// `crate::versions`): every major with its state, and the client, CLI and app versions that work.
pub async fn contracts(
    State(state): State<Shared>,
    Extension(versions): Extension<Arc<Registry>>,
    uri: Uri,
) -> Response {
    ok(
        "contracts",
        versions.matrix(&state.cfg.device_app_min_version, path_major(uri.path())),
    )
}

pub async fn iam(State(state): State<Shared>, headers: HeaderMap) -> AppResult<Response> {
    let secret = headers.get(TESTING_SECRET_HEADER).and_then(|v| v.to_str().ok());
    let (_, sel) = state.select_world(secret).await?;
    Ok(ok(
        "iam",
        IamInfo {
            app_id: state.iam.app_id().to_owned(),
            iam_base_url: state.cfg.iam_public_url.clone(),
            iam_login_url: Some(state.cfg.iam_login_url.clone()),
            api_base_url: state.cfg.public_url.clone(),
            website_url: state.cfg.website_url.clone(),
            docs_url: state.cfg.docs_url.clone(),
            repository_url: state.cfg.repository_url.clone(),
            testing_environment: sel.map(|s| TestingEnvironment {
                environment_id: s.environment_id,
                name: s.name,
                state: "ready".into(),
                paired_devices: 0,
                device_limit: extend_protocol::TEST_DEVICE_LIMIT,
            }),
        },
    ))
}
