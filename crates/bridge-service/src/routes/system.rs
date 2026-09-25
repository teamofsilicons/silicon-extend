//! Health, version negotiation, contract discovery and public IAM details.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bridge_protocol::model::{IamInfo, TestingEnvironment, VersionInfo};
use bridge_protocol::{API_VERSION, API_VERSION_HEADER, ErrorCode, SUPPORTED_VERSIONS_HEADER, TESTING_SECRET_HEADER};

use super::ok;
use crate::error::{AppError, AppResult};
use crate::state::Shared;

pub async fn live() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn ready(State(state): State<Shared>) -> Response {
    match sqlx::query("SELECT 1 FROM bridge_global.schema_versions LIMIT 1").execute(&state.pool).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("database unavailable: {e}")).into_response(),
    }
}

pub async fn negotiate(headers: HeaderMap) -> AppResult<Response> {
    let raw = headers.get(SUPPORTED_VERSIONS_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("1");
    let theirs: Vec<u32> = raw.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let ours = [API_VERSION];
    let agreed = theirs.iter().copied().filter(|v| ours.contains(v)).max().ok_or_else(|| {
        AppError::new(ErrorCode::ApiVersionUnsupported, format!("No API version in common: the client supports {theirs:?}, Bridge supports {ours:?}."))
            .hint("Update the CLI with `honeycomb install 'bridge'`.")
            .details(serde_json::json!({"client": theirs, "service": ours}))
    })?;
    let mut resp = ok(
        "version",
        VersionInfo { api_version: agreed, supported: ours.to_vec(), service_version: env!("CARGO_PKG_VERSION").into(), deprecated: vec![] },
    );
    resp.headers_mut().insert(API_VERSION_HEADER, HeaderValue::from(agreed));
    resp.headers_mut().insert("vary", HeaderValue::from_static(SUPPORTED_VERSIONS_HEADER));
    Ok(resp)
}

pub async fn contracts() -> Response {
    ok(
        "contracts",
        serde_json::json!({
            "versions": [{
                "api_version": API_VERSION,
                "state": "current",
                "deprecated_at": null,
                "sunset_rule": "Sunset after 7 consecutive days with zero requests",
                "compatible": {"client_crate": ">=1.0.0, <2.0.0", "cli": ">=1.0.0, <2.0.0", "device_app_min": "1.0.0"}
            }]
        }),
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
            api_base_url: state.cfg.public_url.clone(),
            website_url: state.cfg.website_url.clone(),
            docs_url: state.cfg.docs_url.clone(),
            repository_url: state.cfg.repository_url.clone(),
            testing_environment: sel.map(|s| TestingEnvironment {
                environment_id: s.environment_id,
                name: s.name,
                state: "ready".into(),
                paired_devices: 0,
                device_limit: bridge_protocol::TEST_DEVICE_LIMIT,
            }),
        },
    ))
}
