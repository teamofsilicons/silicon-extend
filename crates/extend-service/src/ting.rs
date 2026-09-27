//! Delivering a Silicon's request for a device to the Silicon using it, through Ting.
//!
//! Ting is reached on the requesting Silicon's behalf with an IAM OBO proof for `tings.send`
//! (the same way Silicon Hook and DM reach it). The reason travels exactly as the Silicon wrote it.
//!
//! Ting delivers an app's tings only to recipients that registered that app (`subscriptions.register`,
//! with the *recipient's* own proof); a send to anyone else is refused with
//! `recipient_not_registered`. [`Notifier::register_recipient`] makes that registration while
//! Extend holds the Silicon's login: when the Silicon starts a session (the Silicon using a device
//! is the one requests go to), and again before each retry of a pending request. Extend's type
//! (`extend.device.requested`) must be registered in Ting's catalog for the `extend` app (through
//! Honeycomb).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use extend_protocol::ErrorCode;

use crate::error::{AppError, AppResult};
use crate::iam::{DynIam, Principal, TestingSelection};

/// The Ting type Extend sends: `{app}.device.requested`.
pub const DEVICE_REQUESTED: &str = "device.requested";

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
    async fn device_request(
        &self,
        from: &Principal,
        ting: &DeviceRequestTing<'_>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<()>;
    /// Lets Extend notify this Silicon through Ting from now on. Needs the Silicon's own login (the
    /// proof must be for the recipient), so it belongs where a Silicon signs in or starts a session.
    /// Registering again is harmless.
    async fn register_recipient(&self, _recipient: &Principal, _sel: Option<&TestingSelection>) -> AppResult<()> {
        Ok(())
    }
}

pub type DynNotifier = Arc<dyn Notifier>;

pub struct TingNotifier {
    http: reqwest::Client,
    base_url: String,
    iam: DynIam,
    /// (test environment, team, member) already registered by this process.
    registered: tokio::sync::Mutex<HashSet<(Option<uuid::Uuid>, String, String)>>,
}

impl TingNotifier {
    pub fn new(base_url: String, iam: DynIam) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            iam,
            registered: Default::default(),
        }
    }

    /// Sends one proof-bound call to Ting as `member`. A proof is single-use; no retries here.
    async fn call(
        &self,
        member: &Principal,
        endpoint_id: &str,
        path: &str,
        body: &serde_json::Value,
        sel: Option<&TestingSelection>,
    ) -> AppResult<serde_json::Value> {
        let bytes = serde_json::to_vec(body).map_err(AppError::internal)?;
        let proof = self
            .iam
            .obo_proof(member, "ting", endpoint_id, serde_json::json!({}), "POST", &bytes, sel)
            .await?;
        let mut req = self
            .http
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&proof.access_proof)
            .header("Content-Type", "application/json")
            .body(bytes);
        if let (Some(secret), Some(key)) = (&proof.testing_app_secret, &proof.testing_iam_key) {
            req = req
                .header("IAM_TEST_APP_SECRET", secret)
                .header("X-Testing-Environment-Key", key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::unavailable("Ting", format!("{endpoint_id} could not be sent: {e}")))?;
        let status = resp.status().as_u16();
        let json: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if (200..300).contains(&status) {
            Ok(json)
        } else {
            Err(ting_error(
                status,
                &json,
                endpoint_id,
                body.get("for").and_then(|v| v.as_str()).unwrap_or("the recipient"),
                self.iam.app_id(),
            ))
        }
    }
}

/// Turns a Ting refusal into an error that says what happened, why, and what to do.
pub fn ting_error(status: u16, body: &serde_json::Value, endpoint_id: &str, recipient: &str, app_id: &str) -> AppError {
    let code = body
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .unwrap_or("no code");
    let message = body
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .unwrap_or("no details");
    let what = format!("Ting refused {endpoint_id} ({status} {code}: {message})");
    match (status, code) {
        (403, "recipient_not_registered") => AppError::new(
            ErrorCode::NoAccess,
            format!("{what}: {recipient} has not registered to receive {app_id}'s notifications in Ting yet."),
        )
        .hint(format!("Extend registers {recipient} when it starts a session (and while Extend holds its login); the request stays pending and is retried.")),
        (404, _) if endpoint_id == "tings.send" => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("{what}: Ting does not know the type {app_id}.{DEVICE_REQUESTED}."),
        )
        .hint(format!("Register the type {app_id}.{DEVICE_REQUESTED} for {app_id} in Ting (through Honeycomb).")),
        (401, _) | (503, "proof_verification_uncertain") => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("{what}: Ting could not verify the single-use IAM proof Extend sent."),
        )
        .hint("It is retried with a new proof; if it keeps failing, report it with `extend report`."),
        (403, _) => AppError::new(ErrorCode::NoAccess, format!("{what}: Extend is not allowed to do this in Ting."))
            .hint(format!("A Team admin can check that {app_id} is approved for Ting's {endpoint_id} in Honeycomb.")),
        (429, _) => AppError::new(ErrorCode::RateLimited, format!("{what}: Ting is rate limiting.")).hint("It is retried shortly."),
        _ => AppError::unavailable("Ting", what),
    }
}

#[async_trait]
impl Notifier for TingNotifier {
    async fn device_request(
        &self,
        from: &Principal,
        t: &DeviceRequestTing<'_>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<()> {
        let body = serde_json::json!({
            "org_id": from.team.clone().unwrap_or_default(),
            "type": format!("{}.{DEVICE_REQUESTED}", self.iam.app_id()),
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
        self.call(from, "tings.send", "/v1/tings", &body, sel).await.map(|_| ())
    }

    async fn register_recipient(&self, recipient: &Principal, sel: Option<&TestingSelection>) -> AppResult<()> {
        let team = recipient.team()?.to_owned();
        let key = (sel.map(|s| s.environment_id), team.clone(), recipient.id().to_owned());
        if self.registered.lock().await.contains(&key) {
            return Ok(());
        }
        let body = serde_json::json!({"org_id": team, "app_id": self.iam.app_id(), "for": recipient.id()});
        let grant = self
            .call(recipient, "subscriptions.register", "/v1/subscriptions", &body, sel)
            .await?;
        if grant.get("active").and_then(|v| v.as_bool()) != Some(true) {
            return Err(AppError::unavailable(
                "Ting",
                format!(
                    "registering {} answered without an active subscription: {grant}",
                    recipient.id()
                ),
            ));
        }
        self.registered.lock().await.insert(key);
        Ok(())
    }
}

/// Records requests without sending them anywhere (development and tests).
#[derive(Default)]
pub struct LocalNotifier {
    pub sent: tokio::sync::Mutex<Vec<serde_json::Value>>,
    /// Recipients registered, as (test environment, team, member), once each.
    pub registered: tokio::sync::Mutex<Vec<(Option<uuid::Uuid>, String, String)>>,
}

#[async_trait]
impl Notifier for LocalNotifier {
    async fn device_request(
        &self,
        _from: &Principal,
        t: &DeviceRequestTing<'_>,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<()> {
        tracing::info!(
            to = t.to,
            from = t.from,
            device = t.device_id,
            "local Ting: device request"
        );
        self.sent.lock().await.push(serde_json::to_value(t).unwrap_or_default());
        Ok(())
    }

    async fn register_recipient(&self, recipient: &Principal, sel: Option<&TestingSelection>) -> AppResult<()> {
        let key = (
            sel.map(|s| s.environment_id),
            recipient.team()?.to_owned(),
            recipient.id().to_owned(),
        );
        let mut registered = self.registered.lock().await;
        if !registered.contains(&key) {
            tracing::info!(
                recipient = recipient.id(),
                team = key.1,
                "local Ting: recipient registered"
            );
            registered.push(key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_say_what_why_and_what_to_do() {
        let body = serde_json::json!({"error":{"code":"recipient_not_registered","message":"The app does not have permission to notify this recipient."}});
        let e = ting_error(403, &body, "tings.send", "si:chef", "extend");
        assert_eq!(e.code(), ErrorCode::NoAccess);
        assert!(
            e.0.message.contains("si:chef has not registered") && e.0.message.contains("recipient_not_registered"),
            "{}",
            e.0.message
        );
        assert!(e.0.hint.as_deref().unwrap_or_default().contains("retried"));
        let e = ting_error(
            404,
            &serde_json::json!({"error":{"code":"not_found","message":"Not found."}}),
            "tings.send",
            "si:chef",
            "extend",
        );
        assert!(e.0.message.contains("extend.device.requested"));
        assert_eq!(
            ting_error(502, &serde_json::Value::Null, "tings.send", "si:chef", "extend").code(),
            ErrorCode::ServiceUnavailable
        );
    }
}
