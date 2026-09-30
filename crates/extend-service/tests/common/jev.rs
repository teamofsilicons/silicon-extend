//! A deterministic loopback Jev server. Every answer is formed from the offered choices.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use extend_service::config::JevConfig;
use serde_json::{Value, json};

#[derive(Clone)]
struct MockState {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    status: Arc<AtomicU16>,
}

pub struct JevMock {
    pub endpoint: String,
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
    pub status: Arc<AtomicU16>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for JevMock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn choose(State(state): State<MockState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let credential = headers.get("authorization").unwrap().to_str().unwrap().to_owned();
    state.calls.lock().unwrap().push((credential, body.clone()));
    let status = StatusCode::from_u16(state.status.load(Ordering::Relaxed)).unwrap();
    if status != StatusCode::OK {
        return (status, "secret-provider-error-body").into_response();
    }
    let answers = body["questions"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, question)| {
            let criteria = question["criteria"].as_object().unwrap();
            let choice = if name == "operation" { "click" } else { "@e1" };
            let choice = if criteria.contains_key(choice) {
                choice
            } else {
                "BLOCKED"
            };
            let probabilities: serde_json::Map<String, Value> = criteria
                .keys()
                .map(|key| {
                    (
                        key.clone(),
                        json!(if key == choice {
                            0.9
                        } else {
                            0.1 / (criteria.len() - 1) as f64
                        }),
                    )
                })
                .collect();
            (
                name.clone(),
                json!({"choice":choice,"confidence":0.9,"probabilities":probabilities}),
            )
        })
        .collect::<serde_json::Map<String, Value>>();
    Json(json!({"answers":answers,"model":"jev-1.13.0","usage":{"input_tokens":50}})).into_response()
}

impl JevMock {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/select", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(AtomicU16::new(200));
        let app = Router::new().route("/select", post(choose)).with_state(MockState {
            calls: calls.clone(),
            status: status.clone(),
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            calls,
            status,
            task,
        }
    }

    pub fn config(&self) -> JevConfig {
        JevConfig {
            api_key: Some("managed-prod-fixture".into()),
            test_api_key: Some("managed-test-fixture".into()),
            endpoint: self.endpoint.clone(),
            ..Default::default()
        }
    }
}
