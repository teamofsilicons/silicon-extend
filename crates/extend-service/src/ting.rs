//! Delivering a Silicon's request for a device to the Silicon using it, through Ting.
//!
//! Ting is reached on the requesting Silicon's behalf with an IAM OBO proof for `tings.send`
//! (the same way Silicon Hook and DM reach it). The reason travels exactly as the Silicon wrote it.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{AppError, AppResult};
use crate::iam::{DynIam, Principal, TestingSelection};

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceRequestTing<'a> {
    pub request_id: uuid::Uuid,
    pub device_id: &'a str,
    pub device_name: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub session_id: Option<&'a str>,
    pub reason: &'a str,
}

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn device_request(&self, from: &Principal, ting: &DeviceRequestTing<'_>, sel: Option<&TestingSelection>) -> AppResult<()>;
}

pub type DynNotifier = Arc<dyn Notifier>;

pub struct TingNotifier {
    http: reqwest::Client,
    base_url: String,
    iam: DynIam,
}

impl TingNotifier {
    pub fn new(base_url: String, iam: DynIam) -> Self {
        Self { http: reqwest::Client::new(), base_url: base_url.trim_end_matches('/').to_owned(), iam }
    }
}

#[async_trait]
impl Notifier for TingNotifier {
    async fn device_request(&self, from: &Principal, t: &DeviceRequestTing<'_>, sel: Option<&TestingSelection>) -> AppResult<()> {
        let body = serde_json::json!({
            "org_id": from.team.clone().unwrap_or_default(),
            "type": format!("{}.device.requested", self.iam.app_id()),
            "for": t.to,
            "key": t.request_id.to_string(),
            "data": {
                "request_id": t.request_id,
                "device_id": t.device_id,
                "device_name": t.device_name,
                "from": t.from,
                "session_id": t.session_id,
                "reason": t.reason,
                "summary": format!("{} asks to use {} ({}): {}", t.from, t.device_name, t.device_id, t.reason),
                "end_session": t.session_id.map(|s| format!("extend session end {s}")),
            },
            "metadata": {},
        });
        let bytes = serde_json::to_vec(&body).map_err(AppError::internal)?;
        let proof = self.iam.obo_proof(from, "ting", "tings.send", serde_json::json!({}), "POST", &bytes, sel).await?;
        let mut req = self
            .http
            .post(format!("{}/v1/tings", self.base_url))
            .bearer_auth(&proof.access_proof)
            .header("Content-Type", "application/json")
            .body(bytes);
        if let (Some(secret), Some(key)) = (&proof.testing_app_secret, &proof.testing_iam_key) {
            req = req.header("IAM_TEST_APP_SECRET", secret).header("X-Testing-Environment-Key", key);
        }
        let resp = req.send().await.map_err(|e| AppError::unavailable("Ting", e))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            Err(AppError::unavailable("Ting", format!("answered {status}: {}", text.chars().take(300).collect::<String>())))
        }
    }
}

/// Records requests without sending them anywhere (development and tests).
#[derive(Default)]
pub struct LocalNotifier {
    pub sent: tokio::sync::Mutex<Vec<serde_json::Value>>,
}

#[async_trait]
impl Notifier for LocalNotifier {
    async fn device_request(&self, _from: &Principal, t: &DeviceRequestTing<'_>, _sel: Option<&TestingSelection>) -> AppResult<()> {
        tracing::info!(to = t.to, from = t.from, device = t.device_id, "local Ting: device request");
        self.sent.lock().await.push(serde_json::to_value(t).unwrap_or_default());
        Ok(())
    }
}
