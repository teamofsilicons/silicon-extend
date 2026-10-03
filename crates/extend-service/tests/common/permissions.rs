//! Contract-test-only IAM decorator. Real service LocalIam never approves external features.
use async_trait::async_trait;
use axum::{Json, extract::State, routing::post};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use extend_protocol::model::{AuthSession, Member, TeamSilicon};
use extend_service::{
    error::AppResult,
    iam::{DynIam, Iam, IamEvent, Membership, OboProof, Principal, TestingSelection},
    obo::{GrantKey, GrantStore, PermissionInput},
};
use serde_json::{Value, json};
use silicon_iam_client::{Client, Credential};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;
#[derive(Default)]
struct Fixture {
    requests: HashMap<Uuid, Value>,
}
async fn start(State(state): State<Arc<Mutex<Fixture>>>, Json(body): Json<Value>) -> Json<Value> {
    let id = Uuid::new_v4();
    state.lock().await.requests.insert(id, body.clone());
    Json(
        json!({"id":id,"app_id":"extend","app_name":"Extend","actor":{"type":"carbon","public_id":"c:alice"},"org_id":"acme","status":"pending","version":1,"expires_at":"2099-01-01T00:00:00Z","endpoints":[],"authorization_url":"https://iam.example/obo/consent"}),
    )
}
async fn complete(State(state): State<Arc<Mutex<Fixture>>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["authorization_code"], "obc_sentinel-permission-code");
    let id: Uuid = serde_json::from_value(body["authorization_id"].clone()).unwrap();
    let state = state.lock().await;
    let request = &state.requests[&id];
    let items:Vec<_>=request["endpoints"].as_array().unwrap().iter().map(|ep|json!({"grant_id":Uuid::new_v4(),"access_token":"oba_contract_only","token_type":"Bearer","expires_in":300,"expires_at":"2099-01-01T00:00:00Z","audience":ep["audience"],"endpoint_id":ep["endpoint_id"],"org_id":"acme","actor":{"type":"carbon","public_id":"c:alice"},"scope":"fixture","refresh_token":"obr_contract_only"})).collect();
    Json(json!({"items":items}))
}
pub async fn decorate(inner: DynIam, pool: sqlx::PgPool) -> DynIam {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/api/v1/obo-access/authorizations", post(start))
        .route("/api/v1/obo-access/tokens", post(complete))
        .with_state(Arc::new(Mutex::new(Fixture::default())));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Client::builder(&base)
        .unwrap()
        .credential(Credential::application("extend", "fixture-only"))
        .telemetry(false)
        .build()
        .unwrap();
    let key = GrantKey::parse(&URL_SAFE_NO_PAD.encode([73u8; 32])).unwrap();
    Arc::new(PermissionIam {
        inner,
        store: GrantStore::new(pool, key),
        client,
    })
}
struct PermissionIam {
    inner: DynIam,
    store: GrantStore,
    client: Client,
}
#[async_trait]
impl Iam for PermissionIam {
    fn app_id(&self) -> &str {
        self.inner.app_id()
    }
    async fn login(&self, slt: &str, key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        self.inner.login(slt, key, sel).await
    }
    async fn refresh(&self, token: &str, key: &str, sel: Option<&TestingSelection>) -> AppResult<AuthSession> {
        self.inner.refresh(token, key, sel).await
    }
    async fn logout(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<()> {
        self.inner.logout(token, sel).await
    }
    async fn authorize(&self, token: &str, team: Option<&str>, sel: Option<&TestingSelection>) -> AppResult<Principal> {
        self.inner.authorize(token, team, sel).await
    }
    async fn member_active(
        &self,
        team: &str,
        id: &str,
        reader: Option<&Principal>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<bool> {
        self.inner.member_active(team, id, reader, sel).await
    }
    async fn membership(
        &self,
        team: &str,
        id: &str,
        reader: Option<&Principal>,
        sel: Option<&TestingSelection>,
    ) -> Membership {
        self.inner.membership(team, id, reader, sel).await
    }
    async fn team_silicons(&self, p: &Principal, sel: Option<&TestingSelection>) -> AppResult<Vec<TeamSilicon>> {
        self.inner.team_silicons(p, sel).await
    }
    async fn select_testing(&self, secret: &str) -> AppResult<(Uuid, String)> {
        self.inner.select_testing(secret).await
    }
    async fn obo_proof(
        &self,
        p: &Principal,
        aud: &str,
        ep: &str,
        metadata: Value,
        method: &str,
        body: &[u8],
        sel: Option<&TestingSelection>,
    ) -> AppResult<OboProof> {
        self.inner.obo_proof(p, aud, ep, metadata, method, body, sel).await
    }
    async fn verify_webhook(&self, headers: &http::HeaderMap, body: &[u8]) -> AppResult<IamEvent> {
        self.inner.verify_webhook(headers, body).await
    }
    async fn identify(&self, token: &str, sel: Option<&TestingSelection>) -> AppResult<Option<Member>> {
        self.inner.identify(token, sel).await
    }
    async fn test_webhook_digest(&self, id: Uuid) -> Option<String> {
        self.inner.test_webhook_digest(id).await
    }
    async fn remember_test_webhook_key(&self, digest: &str, id: Uuid) {
        self.inner.remember_test_webhook_key(digest, id).await
    }
    async fn permission_start(
        &self,
        p: &Principal,
        input: PermissionInput,
        key: Uuid,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Value> {
        self.store.start(&self.client, p, input, key, sel).await
    }
    async fn permission_complete(
        &self,
        p: &Principal,
        id: Uuid,
        input: &extend_service::obo::CompleteInput,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Value> {
        self.store.complete(&self.client, p, id, input, sel).await
    }
    async fn permissions(&self, p: &Principal, sel: Option<&TestingSelection>) -> AppResult<Value> {
        self.store.list(p, sel).await
    }
}
