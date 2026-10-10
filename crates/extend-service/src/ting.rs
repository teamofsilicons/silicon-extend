//! Extend's notifications through Ting: requests for a device in use, and wake requests.
//!
//! Ting is outside Extend's move to Silicon Accounts and still runs on its own sign-in, so Extend
//! talks to it through this adapter, whose Silicon Accounts contract is:
//! - a recipient is enrolled with a User verification proof for receiving app `ting`, scope
//!   `tings.subscribe`, issued while the recipient uses Extend ([`Notifier::register_recipient`]);
//! - Extend sends with an App verification proof for `ting`, scope `tings.send`
//!   ([`Notifier::send_frozen`]): Extend notifies as itself, never as one of the people involved;
//! - recipients are addressed by Silicon Accounts uuid, with their current public id.
//!
//! Delivery is off unless `EXTEND_TING_URL` is set ([`OffNotifier`]). Extend works the same
//! without it: requests and wake requests are on the website, in the CLI and on the device, and
//! the start-up log, `GET /api/v2/accounts` and `GET /api/v2/ting-registration` say so.
//!
//! Bodies are frozen: the first attempt's exact body is stored, and every retry resends it
//! unchanged, because Ting answers `409 idempotency_conflict` when the data under a key changes.
//! Such an answer means Ting already has the Ting, so it counts as delivered.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use extend_protocol::ErrorCode;
use extend_protocol::ting::{self as types, TingType};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::accounts::Principal;
use crate::error::{AppError, AppResult};
use crate::proofs::{ProofStore, TING, TING_SEND_SCOPES, TING_SUBSCRIBE_SCOPES};

/// `{app}.device.requested`: a Silicon asks to use a device another Silicon is using.
pub const DEVICE_REQUESTED: &str = types::DEVICE_REQUESTED.event;
/// `{app}.device.wake_requested`: a Silicon asks its Carbon to wake a device.
pub const WAKE_REQUESTED: &str = types::WAKE_REQUESTED.event;
/// `{app}.device.woken`: a device a Silicon asked to wake is awake.
pub const WOKEN: &str = types::WOKEN.event;
/// `{app}.device.wake_declined`: a Carbon turned down a request to wake a device.
pub const WAKE_DECLINED: &str = types::WAKE_DECLINED.event;
/// Every type Extend sends, with the description it is registered with.
pub const ALL_TYPES: [TingType; 4] = types::ALL_TYPES;

/// Why nothing was sent while Ting is off.
pub const OFF: &str = "Not sent: notifications through Ting are off on this Extend server (EXTEND_TING_URL is unset). \
                       The recipient sees it on the website, in the CLI and on the device.";

/// A request for a device in use, as a Ting carries it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceRequestTing<'a> {
    pub request_id: Uuid,
    /// The device, by the recipient's own id for it.
    pub device_id: &'a str,
    /// The recipient's own name for the device.
    pub device_name: &'a str,
    /// The asking Silicon's current public id.
    pub from: &'a str,
    /// The recipient: the Silicon using the device (holder route) or the Carbon who gave it access
    /// (carbon route), by uuid and current public id.
    pub to: &'a str,
    pub to_id: &'a str,
    /// The holder's session: only on the holder route, where the recipient is that session's Silicon.
    pub session_id: Option<&'a str>,
    pub reason: &'a str,
    pub routed_to: extend_protocol::model::RequestRoute,
    /// Carbon route: the website link to the device's page.
    pub link: Option<String>,
}

/// The whole `tings.send` body for a request for a device in use.
pub fn request_body(app_id: &str, t: &DeviceRequestTing<'_>) -> Value {
    let data = match t.routed_to {
        extend_protocol::model::RequestRoute::Carbon => json!({
            "request_id": t.request_id,
            "device_id": t.device_id,
            "device_name": t.device_name,
            "from": t.from,
            "reason": t.reason,
            "routed_to": "carbon",
            "summary": format!(
                "{} asks to use {} ({}), which one of your Silicons is using: {}. Stop it with: extend device stop {}",
                t.from, t.device_name, t.device_id, t.reason, t.device_id
            ),
            "stop": format!("extend device stop {}", t.device_id),
            "link": t.link,
        }),
        _ => json!({
            "request_id": t.request_id,
            "device_id": t.device_id,
            "device_name": t.device_name,
            "from": t.from,
            "session_id": t.session_id,
            "reason": t.reason,
            "summary": format!("{} asks to use {} ({}): {}", t.from, t.device_name, t.device_id, t.reason),
            "end_session": t.session_id.map(|s| format!("extend session end {s}")),
            "routed_to": "holder",
        }),
    };
    body(app_id, DEVICE_REQUESTED, t.to, t.to_id, &t.request_id.to_string(), data)
}

/// A whole `tings.send` body of one of Extend's types, for one recipient (uuid and current id).
pub fn body(app_id: &str, ty: &str, recipient: &str, recipient_id: &str, key: &str, data: Value) -> Value {
    json!({
        "type": format!("{app_id}.{ty}"),
        "for": recipient,
        "for_id": recipient_id,
        "key": key,
        "data": data,
        "metadata": {},
    })
}

/// The full type name and the recipient (uuid) of a frozen body.
pub fn body_parts(body: &Value) -> (String, String) {
    let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    (s("type"), s("for"))
}

/// The full name of a type with the app id (`extend.device.woken`).
pub fn type_name(app_id: &str, ty: &str) -> String {
    format!("{app_id}.{ty}")
}

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Whether Extend delivers through Ting at all.
    fn enabled(&self) -> bool {
        true
    }
    /// Sends one whole body exactly as given (the first attempt and every retry), as Extend.
    async fn send_frozen(&self, body: &Value) -> AppResult<()>;
    /// Lets Ting deliver Extend's notifications to `recipient` from now on, with the recipient's
    /// own agreement (a proof issued from their live sign-in). `force` re-enrols a recipient who
    /// turned Extend off in Ting ("Turn on").
    async fn register_recipient(&self, recipient: &Principal, force: bool) -> AppResult<()>;
    /// The app id Extend's types are named with.
    fn app_id(&self) -> &str;
}

pub type DynNotifier = Arc<dyn Notifier>;

/// Ting is not configured: nothing is sent, and every attempt says why.
pub struct OffNotifier {
    pub app_id: String,
}

#[async_trait]
impl Notifier for OffNotifier {
    fn enabled(&self) -> bool {
        false
    }
    async fn send_frozen(&self, _body: &Value) -> AppResult<()> {
        Err(AppError::new(ErrorCode::ServiceUnavailable, OFF))
    }
    async fn register_recipient(&self, _recipient: &Principal, _force: bool) -> AppResult<()> {
        Ok(())
    }
    fn app_id(&self) -> &str {
        &self.app_id
    }
}

pub struct TingNotifier {
    http: reqwest::Client,
    base_url: String,
    app_id: String,
    proofs: Arc<ProofStore>,
    /// Silicons enrolled by this process (Carbons are recorded in the database).
    registered: tokio::sync::Mutex<HashSet<String>>,
}

impl TingNotifier {
    pub fn new(base_url: String, app_id: String, proofs: Arc<ProofStore>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .user_agent(concat!("silicon-extend/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            app_id,
            proofs,
            registered: Default::default(),
        }
    }

    async fn call(&self, proof: &str, what: &str, path: &str, body: &Value) -> AppResult<Value> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base_url))
            .header("authorization", format!("Proof {proof}"))
            .json(body)
            .send()
            .await
            .map_err(|e| AppError::unavailable("Ting", format!("{what} could not be sent: {e}")))?;
        let status = resp.status().as_u16();
        let json: Value = resp.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(json);
        }
        let code = json.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
        if what == "tings.send" && status == 409 && code == "idempotency_conflict" {
            return Ok(json);
        }
        let recipient = body.get("for_id").and_then(Value::as_str).unwrap_or("the recipient");
        Err(ting_error(
            status,
            &json,
            what,
            recipient,
            &self.app_id,
            body.get("type").and_then(Value::as_str),
        ))
    }
}

/// Turns a Ting refusal into an error that says what happened, why, and what to do. `details`
/// carries `ting_status` and `ting_code`, and `missing_type` when Ting doesn't know the type.
pub fn ting_error(
    status: u16,
    body: &Value,
    what: &str,
    recipient: &str,
    app_id: &str,
    ting_type: Option<&str>,
) -> AppError {
    let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or("no code");
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("no details");
    let head = format!("Ting refused {what} ({status} {code}: {message})");
    let type_name = ting_type
        .map(str::to_owned)
        .unwrap_or_else(|| type_name(app_id, DEVICE_REQUESTED));
    let mut details = json!({"ting_status": status, "ting_code": code});
    let err = match (status, code) {
        (403, "recipient_not_registered") => AppError::new(
            ErrorCode::NoAccess,
            format!("{head}: {recipient} has not enrolled to receive {app_id}'s notifications in Ting yet."),
        )
        .hint(format!(
            "Extend enrols {recipient} at their next use of Extend; the notification waits. A Carbon who turned \
             Extend's notifications off turns them on again in Extend's settings (`extend ting on`)."
        )),
        (404, _) if what == "tings.send" => {
            details["missing_type"] = json!(type_name);
            AppError::new(
                ErrorCode::ServiceUnavailable,
                format!("{head}: Ting doesn't know the app type {type_name}; the notification waits."),
            )
            .hint("Ting registers an app's types once; ask a Ting Carbon to register Extend's.")
        }
        (401, _) => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("{head}: Ting did not accept the Silicon Accounts proof Extend sent."),
        )
        .hint("It is retried with a fresh proof; if it keeps failing, report it with `extend report`."),
        (403, _) => AppError::new(
            ErrorCode::NoAccess,
            format!("{head}: Extend is not allowed to do this in Ting."),
        )
        .hint("Ask a Ting Carbon to accept Extend's proofs for this."),
        (429, _) => AppError::new(ErrorCode::RateLimited, format!("{head}: Ting is rate limiting."))
            .hint("It is retried shortly."),
        _ => AppError::unavailable("Ting", head),
    };
    err.details(details)
}

/// The full type name Ting doesn't know, when that is why a send failed.
pub fn missing_type(e: &AppError) -> Option<String> {
    e.0.details
        .get("missing_type")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Whether a send failed because the recipient hasn't enrolled Extend in Ting (or turned it off).
pub fn not_registered(e: &AppError) -> bool {
    e.0.details.get("ting_code").and_then(Value::as_str) == Some("recipient_not_registered")
}

#[async_trait]
impl Notifier for TingNotifier {
    async fn send_frozen(&self, body: &Value) -> AppResult<()> {
        let proof = self.proofs.for_app(TING, TING_SEND_SCOPES).await?;
        self.call(&proof, "tings.send", "/v1/tings", body).await.map(|_| ())
    }

    async fn register_recipient(&self, recipient: &Principal, force: bool) -> AppResult<()> {
        if recipient.is_silicon() && !force && self.registered.lock().await.contains(&recipient.uuid) {
            return Ok(());
        }
        let proof = self.proofs.for_user(recipient, TING, TING_SUBSCRIBE_SCOPES).await?;
        let body = json!({"app_id": self.app_id, "for": recipient.uuid, "for_id": recipient.id});
        let grant = self.call(&proof, "tings.subscribe", "/v1/subscriptions", &body).await?;
        if grant.get("active").and_then(Value::as_bool) == Some(false) {
            return Err(AppError::unavailable(
                "Ting",
                format!("enrolling {} answered without an active subscription", recipient.id),
            ));
        }
        if recipient.is_silicon() {
            self.registered.lock().await.insert(recipient.uuid.clone());
        }
        Ok(())
    }

    fn app_id(&self) -> &str {
        &self.app_id
    }
}

/// Records Tings without sending them anywhere (development and tests). Tests can make it answer
/// with an injected missing type, an unenrolled recipient, or as unreachable.
#[derive(Default)]
pub struct LocalNotifier {
    /// Every Ting accepted: `{"type", "for", "for_id", "key", "data"}`.
    pub sent: tokio::sync::Mutex<Vec<Value>>,
    /// Recipients enrolled (uuids), once each.
    pub registered: tokio::sync::Mutex<Vec<String>>,
    /// Full type names Ting answers 404 for.
    pub missing: std::sync::Mutex<HashSet<String>>,
    /// Answer `recipient_not_registered` for recipients never enrolled.
    pub require_registration: AtomicBool,
    /// Answer every send with a 503, as an unreachable Ting.
    pub unavailable: AtomicBool,
}

impl LocalNotifier {
    /// The recorded Tings of one type (by its event, `device.woken`).
    pub async fn sent_of(&self, event: &str) -> Vec<Value> {
        self.sent
            .lock()
            .await
            .iter()
            .filter(|t| t["type"].as_str().is_some_and(|ty| ty.ends_with(event)))
            .cloned()
            .collect()
    }

    /// Makes Ting answer 404 for `event` (or answer again, with `false`).
    pub fn set_missing(&self, event: &str, missing: bool) {
        let key = type_name("extend", event);
        let mut m = self.missing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if missing {
            m.insert(key);
        } else {
            m.remove(&key);
        }
    }

    fn refusal(status: u16, code: &str, body: &Value) -> AppError {
        let (ty, _) = body_parts(body);
        let recipient = body.get("for_id").and_then(Value::as_str).unwrap_or_default();
        ting_error(
            status,
            &json!({"error": {"code": code, "message": "local Ting stand-in"}}),
            "tings.send",
            recipient,
            "extend",
            Some(&ty),
        )
    }
}

#[async_trait]
impl Notifier for LocalNotifier {
    async fn send_frozen(&self, body: &Value) -> AppResult<()> {
        let (ty, recipient) = body_parts(body);
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(AppError::unavailable(
                "Ting",
                "the local stand-in is set to be unreachable",
            ));
        }
        if self
            .missing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&ty)
        {
            return Err(Self::refusal(404, "not_found", body));
        }
        if self.require_registration.load(Ordering::Relaxed) && !self.registered.lock().await.contains(&recipient) {
            return Err(Self::refusal(403, "recipient_not_registered", body));
        }
        tracing::info!(to = recipient, ty, "local Ting: sent");
        self.sent.lock().await.push(json!({
            "type": ty,
            "for": recipient,
            "for_id": body.get("for_id"),
            "key": body.get("key"),
            "data": body.get("data"),
        }));
        Ok(())
    }

    async fn register_recipient(&self, recipient: &Principal, _force: bool) -> AppResult<()> {
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(AppError::unavailable(
                "Ting",
                "the local stand-in is set to be unreachable",
            ));
        }
        let mut registered = self.registered.lock().await;
        if !registered.contains(&recipient.uuid) {
            tracing::info!(recipient = recipient.id, "local Ting: recipient enrolled");
            registered.push(recipient.uuid.clone());
        }
        Ok(())
    }

    fn app_id(&self) -> &str {
        "extend"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_say_what_why_and_what_to_do() {
        let body = json!({"error":{"code":"recipient_not_registered","message":"The app does not have permission to notify this recipient."}});
        let e = ting_error(403, &body, "tings.send", "si:chef", "extend", None);
        assert_eq!(e.code(), ErrorCode::NoAccess);
        assert!(
            e.0.message.contains("si:chef has not enrolled") && e.0.message.contains("recipient_not_registered"),
            "{}",
            e.0.message
        );
        assert!(not_registered(&e));
        let e = ting_error(
            404,
            &json!({"error":{"code":"not_found","message":"Not found."}}),
            "tings.send",
            "c:alice",
            "extend",
            Some("extend.device.wake_requested"),
        );
        assert!(e.0.message.contains("extend.device.wake_requested"));
        assert_eq!(missing_type(&e).as_deref(), Some("extend.device.wake_requested"));
        assert_eq!(
            ting_error(502, &Value::Null, "tings.send", "si:chef", "extend", None).code(),
            ErrorCode::ServiceUnavailable
        );
    }

    #[test]
    fn request_bodies_address_recipients_by_uuid_and_carry_no_other_side() {
        let t = DeviceRequestTing {
            request_id: Uuid::nil(),
            device_id: "0d44e1f2",
            device_name: "Family TV",
            from: "si:chef",
            to: "B0bUuid1",
            to_id: "c:bob",
            session_id: None,
            reason: "OTP",
            routed_to: extend_protocol::model::RequestRoute::Carbon,
            link: Some("https://extend.teamofsilicons.com/devices/0d44e1f2".into()),
        };
        let b = request_body("extend", &t);
        assert_eq!(b["for"], "B0bUuid1");
        assert_eq!(b["for_id"], "c:bob");
        assert_eq!(b["data"]["from"], "si:chef");
        assert!(b.get("org_id").is_none());
        assert_eq!(
            b["data"]["summary"],
            "si:chef asks to use Family TV (0d44e1f2), which one of your Silicons is using: OTP. Stop it with: extend device stop 0d44e1f2"
        );
        for gone in ["session_id", "end_session", "team"] {
            assert!(b["data"].get(gone).is_none(), "{gone}: {b}");
        }
    }
}
