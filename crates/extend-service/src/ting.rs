//! Extend's notifications through Ting: requests for a device in use, and wake requests.
//!
//! Every Ting is sent on a member's behalf with a reusable IAM OBO access token for `tings.send` (the
//! way Silicon Hook and DM reach Ting). Its `org_id` is one Team that both the sender (the actor)
//! and the recipient belong to. The actor is always a member who took part, or the recipient
//! itself (a notification to themselves, under their own consent); never a member who didn't take
//! part, such as the Silicon holding a device someone asks for.
//!
//! Ting resolves notification types per environment and app, independently of the delivery Team.
//! Extend's four types ([`extend_protocol::ting::ALL_TYPES`]) are registered once in the app owner's
//! catalog by a Ting manager with Honeycomb permission. Ting does not offer an OBO operation for
//! registering types; a missing type stays visible and pending until that manager registers it.
//!
//! Ting delivers an app's Tings only to recipients that registered the app
//! (`subscriptions.register`, with the *recipient's* own proof); a send to anyone else is refused
//! with `recipient_not_registered`. [`Notifier::register_recipient`] makes that registration while
//! Extend holds the member's login.
//!
//! Bodies are frozen: the first attempt's exact body is stored, and every retry resends it
//! unchanged ([`Notifier::send_frozen`]), because Ting answers `409 idempotency_conflict` when the
//! data under a key changes (after a rename, say). Such an answer means Ting already has the Ting,
//! so it counts as delivered.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use extend_protocol::ErrorCode;
use extend_protocol::ting::{self as types, TingType};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::iam::{DynIam, Principal, TestingSelection};

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

/// A request for a device in use, as a Ting carries it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceRequestTing<'a> {
    pub request_id: Uuid,
    /// The device, by the recipient's own id for it.
    pub device_id: &'a str,
    /// The recipient's own name for the device.
    pub device_name: &'a str,
    pub from: &'a str,
    /// The Silicon using the device (holder route), or the Carbon who gave it access (carbon route).
    pub to: &'a str,
    /// The holder's session: only on the holder route, where the recipient is that session's Silicon.
    pub session_id: Option<&'a str>,
    pub reason: &'a str,
    /// Which way the request went. `None` builds the exact 1.0.0 body (retries of rows 1.0.0 sent first).
    pub routed_to: Option<extend_protocol::model::RequestRoute>,
    /// The requester's Team: in holder-route bodies, and in carbon-route bodies only when the
    /// recipient is the requester's own Carbon.
    pub team: Option<&'a str>,
    /// Carbon route: the website link to the device's page.
    pub link: Option<String>,
    /// Whether the requesting Silicon is hidden from the recipient. A 1.1.0 service never sets it:
    /// the Carbon a request is routed to sees the asking Silicon and its reason (Carbon decision,
    /// 2026-09-27).
    pub from_hidden: bool,
}

/// The whole `tings.send` body for a request for a device in use.
pub fn request_body(app_id: &str, org_id: &str, t: &DeviceRequestTing<'_>) -> Value {
    let from = if t.from_hidden {
        extend_protocol::REQUEST_FROM_HIDDEN
    } else {
        t.from
    };
    let data = match t.routed_to {
        // Rows first sent by 1.0.0: its exact data shape, so a retry isn't refused as a changed body.
        None => json!({
            "request_id": t.request_id,
            "device_id": t.device_id,
            "device_name": t.device_name,
            "from": t.from,
            "session_id": t.session_id,
            "reason": t.reason,
            "summary": format!("{} asks to use {} ({}): {}", t.from, t.device_name, t.device_id, t.reason),
            "end_session": t.session_id.map(|s| format!("extend session end {s}")),
        }),
        Some(extend_protocol::model::RequestRoute::Carbon) => {
            // Never in the body: the Silicon using the device, its Team, or its session.
            let mut data = json!({
                "request_id": t.request_id,
                "device_id": t.device_id,
                "device_name": t.device_name,
                "from": from,
                "reason": t.reason,
                "routed_to": "carbon",
                "summary": format!(
                    "{} asks to use {} ({}), which one of your Silicons is using: {}. Stop it with: extend device stop {}",
                    if t.from_hidden { "A Silicon another Carbon gave access to" } else { t.from },
                    t.device_name,
                    t.device_id,
                    t.reason,
                    t.device_id
                ),
                "stop": format!("extend device stop {}", t.device_id),
                "link": t.link,
            });
            if let Some(team) = t.team {
                data["team"] = json!(team);
            }
            data
        }
        Some(_) => json!({
            "request_id": t.request_id,
            "device_id": t.device_id,
            "device_name": t.device_name,
            "from": t.from,
            "session_id": t.session_id,
            "reason": t.reason,
            "summary": format!("{} asks to use {} ({}): {}", t.from, t.device_name, t.device_id, t.reason),
            "end_session": t.session_id.map(|s| format!("extend session end {s}")),
            "team": t.team,
            "routed_to": "holder",
        }),
    };
    json!({
        "org_id": org_id,
        "type": format!("{app_id}.{DEVICE_REQUESTED}"),
        "for": t.to,
        "key": t.request_id.to_string(),
        "data": data,
        "metadata": {},
    })
}

/// A whole `tings.send` body of one of Extend's types.
pub fn body(app_id: &str, org_id: &str, ty: &str, recipient: &str, key: &str, data: Value) -> Value {
    json!({
        "org_id": org_id,
        "type": format!("{app_id}.{ty}"),
        "for": recipient,
        "key": key,
        "data": data,
        "metadata": {},
    })
}

/// The Team (`org_id`), full type name and recipient of a frozen body.
pub fn body_parts(body: &Value) -> (String, String, String) {
    let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    (s("org_id"), s("type"), s("for"))
}

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Sends one whole `tings.send` body as `actor`, exactly as given (the first attempt and every
    /// retry). `actor` must be a member of the body's `org_id`. Ting's `409 idempotency_conflict`
    /// (it already has a Ting under this key) counts as delivered.
    async fn send_frozen(&self, actor: &Principal, body: &Value, sel: Option<&TestingSelection>) -> AppResult<()>;

    /// Sends a request for a device in use as `from`, in `from`'s Team.
    async fn device_request(
        &self,
        from: &Principal,
        ting: &DeviceRequestTing<'_>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<()> {
        let body = request_body(self.app_id(), from.team.as_deref().unwrap_or_default(), ting);
        self.send_frozen(from, &body, sel).await
    }

    /// Lets Extend notify this member through Ting in `recipient`'s Team from now on. Needs the
    /// member's own login (the proof must be for the recipient). Registering again reactivates a
    /// grant the member turned off in Ting, so Carbons are registered only when Extend has no
    /// record for them in that Team, or when they ask ("Turn on", `force`). `force` also skips the
    /// cache of registered Silicons.
    async fn register_recipient(
        &self,
        _recipient: &Principal,
        _force: bool,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<()> {
        Ok(())
    }

    /// Forgets which Silicons were registered in a test environment (a clean removes Ting's test
    /// grants too), or in production for `None`.
    async fn clear_environment(&self, _environment_id: Option<Uuid>) {}

    /// Whether Ting refused a Ting a member sent to themselves. Remembered for the process; actor
    /// chains then skip such self-sends.
    fn self_send_refused(&self) -> bool {
        false
    }

    /// The app id Extend's types are named with.
    fn app_id(&self) -> &str {
        "extend"
    }
}

pub type DynNotifier = Arc<dyn Notifier>;

/// The full name of a type with the app id (`extend.device.woken`).
pub fn type_name(app_id: &str, ty: &str) -> String {
    format!("{app_id}.{ty}")
}

pub struct TingNotifier {
    http: reqwest::Client,
    base_url: String,
    iam: DynIam,
    /// (test environment, team, Silicon) already registered by this process. Carbons are never
    /// kept here: their registration is recorded in the database, per Team.
    registered: tokio::sync::Mutex<HashSet<(Option<Uuid>, String, String)>>,
    self_send_refused: AtomicBool,
}

impl TingNotifier {
    pub fn new(base_url: String, iam: DynIam) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            iam,
            registered: Default::default(),
            self_send_refused: AtomicBool::new(false),
        }
    }

    /// Sends a call with separately approved reusable authority; receiver authorization remains live.
    async fn call(
        &self,
        member: &Principal,
        endpoint_id: &str,
        path: &str,
        body: &Value,
        sel: Option<&TestingSelection>,
    ) -> AppResult<Value> {
        let bytes = serde_json::to_vec(body).map_err(AppError::internal)?;
        let proof = self
            .iam
            .obo_proof(member, "ting", endpoint_id, json!({}), "POST", &bytes, sel)
            .await?;
        if proof.org_id.as_deref() != body.get("org_id").and_then(Value::as_str)
            || (endpoint_id == "subscriptions.register" && proof.actor.as_deref() != Some(member.id()))
        {
            return Err(AppError::new(ErrorCode::ConfirmationRequired,"Ting approval must use the device organization; subscription approval must also use the recipient's account.")
                .hint("Open Extend Settings → Permissions and choose this account and device organization in IAM. Notification recipients are never silently moved to another organization."));
        }
        let mut req = self
            .http
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&proof.access_proof)
            .header("X-App-ID", self.iam.app_id())
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
        let json: Value = resp.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(json);
        }
        let code = json.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
        if endpoint_id == "tings.send" && status == 409 && code == "idempotency_conflict" {
            // Ting already has a Ting under this key (sent by an earlier attempt).
            return Ok(json);
        }
        let recipient = body.get("for").and_then(Value::as_str).unwrap_or("the recipient");
        let mut err = ting_error(
            status,
            &json,
            endpoint_id,
            recipient,
            self.iam.app_id(),
            body.get("type").and_then(Value::as_str),
            body.get("org_id").and_then(Value::as_str),
        );
        if endpoint_id == "tings.send" && recipient == member.id() && refuses_self_send(status, code) {
            self.self_send_refused.store(true, Ordering::Relaxed);
            err.0.details["self_send_refused"] = json!(true);
            tracing::warn!(
                status,
                code,
                "Ting refused a Ting a member sent to themselves; such sends are skipped from now on"
            );
        }
        Err(err)
    }
}

/// Only an explicit provider self-send restriction establishes that capability. Authentication,
/// authorization and recipient validation failures say nothing about other self-sends, and must
/// remain retryable with a current endpoint authority or after the affected member's permissions change.
fn refuses_self_send(status: u16, code: &str) -> bool {
    matches!(status, 400 | 403 | 422) && matches!(code, "self_send_not_allowed" | "self_send_unsupported")
}

/// Turns a Ting refusal into an error that says what happened, why, and what to do. `details`
/// carries `ting_status` and `ting_code`, and `missing_type` (the full type name) when Ting doesn't
/// know the app's type. The delivery Team is context for the failed send, not its registration owner.
pub fn ting_error(
    status: u16,
    body: &Value,
    endpoint_id: &str,
    recipient: &str,
    app_id: &str,
    ting_type: Option<&str>,
    team: Option<&str>,
) -> AppError {
    let code = body.pointer("/error/code").and_then(Value::as_str).unwrap_or("no code");
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("no details");
    let what = format!("Ting refused {endpoint_id} ({status} {code}: {message})");
    let type_name = ting_type
        .map(str::to_owned)
        .unwrap_or_else(|| type_name(app_id, DEVICE_REQUESTED));
    let mut details = json!({"ting_status": status, "ting_code": code});
    let err = match (status, code) {
        (403, "recipient_not_registered") => AppError::new(
            ErrorCode::NoAccess,
            format!("{what}: {recipient} has not registered to receive {app_id}'s notifications in Ting yet."),
        )
        .hint(format!(
            "Extend registers {recipient} while it holds {recipient}'s login; the Ting stays pending. A Carbon who \
             turned Extend's Tings off turns them on again in Extend's settings (`extend ting on`)."
        )),
        (404, _) if endpoint_id == "tings.send" => {
            details["missing_type"] = json!(type_name);
            let team = team.unwrap_or("the Team");
            let command = types::find(&type_name)
                .map(|t| types::register_command(types::OWNER_TEAM_PLACEHOLDER, app_id, t))
                .unwrap_or_else(|| {
                    format!(
                        "ting --org {} types register --type {type_name}",
                        types::OWNER_TEAM_PLACEHOLDER
                    )
                });
            AppError::new(
                ErrorCode::ServiceUnavailable,
                format!("{what}: Ting doesn't know the app type {type_name}; its notification to {team} stays pending."),
            )
            .hint(format!("A Ting manager in the Team that owns {app_id} registers it once for every delivery Team. Replace <owning-team> with that Team: {command}"))
        }
        (401, _) | (503, "proof_verification_uncertain") => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("{what}: Ting could not verify the single-use IAM proof Extend sent."),
        )
        .hint("It is retried with a new proof; if it keeps failing, report it with `extend report`."),
        (403, _) => AppError::new(
            ErrorCode::NoAccess,
            format!("{what}: Extend is not allowed to do this in Ting."),
        )
        .hint(format!(
            "A Team admin can check that {app_id} is approved for Ting's {endpoint_id} in Honeycomb."
        )),
        (429, _) => AppError::new(ErrorCode::RateLimited, format!("{what}: Ting is rate limiting."))
            .hint("It is retried shortly."),
        _ => AppError::unavailable("Ting", what),
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

/// Whether a send failed because the recipient hasn't registered Extend in Ting (or turned it off).
pub fn not_registered(e: &AppError) -> bool {
    e.0.details.get("ting_code").and_then(Value::as_str) == Some("recipient_not_registered")
}

/// Whether Ting refused a Ting a member sent to themselves.
pub fn self_send_refusal(e: &AppError) -> bool {
    e.0.details.get("self_send_refused").and_then(Value::as_bool) == Some(true)
}

#[async_trait]
impl Notifier for TingNotifier {
    async fn send_frozen(&self, actor: &Principal, body: &Value, sel: Option<&TestingSelection>) -> AppResult<()> {
        self.call(actor, "tings.send", "/v1/tings", body, sel).await.map(|_| ())
    }

    async fn register_recipient(
        &self,
        recipient: &Principal,
        force: bool,
        sel: Option<&TestingSelection>,
    ) -> AppResult<()> {
        let team = recipient.team()?.to_owned();
        let key = (sel.map(|s| s.environment_id), team.clone(), recipient.id().to_owned());
        let cacheable = recipient.is_silicon();
        if cacheable && !force && self.registered.lock().await.contains(&key) {
            return Ok(());
        }
        let body = json!({"org_id": team, "app_id": self.iam.app_id(), "for": recipient.id()});
        let grant = self
            .call(recipient, "subscriptions.register", "/v1/subscriptions", &body, sel)
            .await?;
        if grant.get("active").and_then(Value::as_bool) != Some(true) {
            return Err(AppError::unavailable(
                "Ting",
                format!(
                    "registering {} answered without an active subscription: {grant}",
                    recipient.id()
                ),
            ));
        }
        if cacheable {
            self.registered.lock().await.insert(key);
        }
        Ok(())
    }

    async fn clear_environment(&self, environment_id: Option<Uuid>) {
        self.registered
            .lock()
            .await
            .retain(|(env, _, _)| *env != environment_id);
    }

    fn self_send_refused(&self) -> bool {
        self.self_send_refused.load(Ordering::Relaxed)
    }

    fn app_id(&self) -> &str {
        self.iam.app_id()
    }
}

/// Records Tings without sending them anywhere (development and tests). Tests can make it answer
/// with an injected type refusal on a Team's send, an unregistered recipient or a self-send refusal.
#[derive(Default)]
pub struct LocalNotifier {
    /// Every Ting accepted: `{"actor", "org_id", "type", "for", "key", "data"}`.
    pub sent: tokio::sync::Mutex<Vec<Value>>,
    /// Recipients registered, as (test environment, team, member), once each.
    pub registered: tokio::sync::Mutex<Vec<(Option<Uuid>, String, String)>>,
    /// (team, full type name) that Ting answers 404 for.
    pub missing: std::sync::Mutex<HashSet<(String, String)>>,
    /// Answer `recipient_not_registered` for recipients never registered (in that Team).
    pub require_registration: AtomicBool,
    /// Refuse a Ting whose recipient is its sender.
    pub refuse_self_sends: AtomicBool,
    /// Answer every send with a 503, as an unreachable Ting.
    pub unavailable: AtomicBool,
    self_send_refused: AtomicBool,
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

    /// Makes Ting answer 404 for `event` in `team` (or answer again, with `false`).
    pub fn set_missing(&self, team: &str, event: &str, missing: bool) {
        let key = (team.to_owned(), type_name("extend", event));
        let mut m = self.missing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if missing {
            m.insert(key);
        } else {
            m.remove(&key);
        }
    }

    fn refusal(status: u16, code: &str, body: &Value, actor: &Principal) -> AppError {
        let (org, ty, recipient) = body_parts(body);
        let mut err = ting_error(
            status,
            &json!({"error": {"code": code, "message": "local Ting stand-in"}}),
            "tings.send",
            &recipient,
            "extend",
            Some(&ty),
            Some(&org),
        );
        if recipient == actor.id() && refuses_self_send(status, code) {
            err.0.details["self_send_refused"] = json!(true);
        }
        err
    }
}

#[async_trait]
impl Notifier for LocalNotifier {
    async fn send_frozen(&self, actor: &Principal, body: &Value, sel: Option<&TestingSelection>) -> AppResult<()> {
        let (org, ty, recipient) = body_parts(body);
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(AppError::unavailable(
                "Ting",
                "the local stand-in is set to be unreachable",
            ));
        }
        let missing = self
            .missing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(org.clone(), ty.clone()));
        if missing {
            return Err(Self::refusal(404, "not_found", body, actor));
        }
        if self.refuse_self_sends.load(Ordering::Relaxed) && recipient == actor.id() {
            self.self_send_refused.store(true, Ordering::Relaxed);
            return Err(Self::refusal(422, "self_send_not_allowed", body, actor));
        }
        if self.require_registration.load(Ordering::Relaxed) {
            let env = sel.map(|s| s.environment_id);
            let known = self
                .registered
                .lock()
                .await
                .iter()
                .any(|(e, t, m)| *e == env && *t == org && *m == recipient);
            if !known {
                return Err(Self::refusal(403, "recipient_not_registered", body, actor));
            }
        }
        tracing::info!(actor = actor.id(), to = recipient, org, ty, "local Ting: sent");
        self.sent.lock().await.push(json!({
            "actor": actor.id(),
            "org_id": org,
            "type": ty,
            "for": recipient,
            "key": body.get("key"),
            "data": body.get("data"),
        }));
        Ok(())
    }

    async fn register_recipient(
        &self,
        recipient: &Principal,
        _force: bool,
        sel: Option<&TestingSelection>,
    ) -> AppResult<()> {
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(AppError::unavailable(
                "Ting",
                "the local stand-in is set to be unreachable",
            ));
        }
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

    async fn clear_environment(&self, environment_id: Option<Uuid>) {
        self.registered
            .lock()
            .await
            .retain(|(env, _, _)| *env != environment_id);
    }

    fn self_send_refused(&self) -> bool {
        self.self_send_refused.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_say_what_why_and_what_to_do() {
        let body = json!({"error":{"code":"recipient_not_registered","message":"The app does not have permission to notify this recipient."}});
        let e = ting_error(403, &body, "tings.send", "si:chef", "extend", None, Some("acme"));
        assert_eq!(e.code(), ErrorCode::NoAccess);
        assert!(
            e.0.message.contains("si:chef has not registered") && e.0.message.contains("recipient_not_registered"),
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
            Some("labs"),
        );
        assert!(e.0.message.contains("extend.device.wake_requested") && e.0.message.contains("labs"));
        assert_eq!(
            e.0.hint.as_deref(),
            Some(
                "A Ting manager in the Team that owns extend registers it once for every delivery Team. Replace <owning-team> with that Team: ting --org '<owning-team>' types register --type \
                 extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'"
            )
        );
        assert_eq!(missing_type(&e).as_deref(), Some("extend.device.wake_requested"));
        assert_eq!(
            ting_error(502, &Value::Null, "tings.send", "si:chef", "extend", None, None).code(),
            ErrorCode::ServiceUnavailable
        );
    }

    #[test]
    fn request_bodies_carry_no_other_side() {
        let t = DeviceRequestTing {
            request_id: Uuid::nil(),
            device_id: "0d44e1f2",
            device_name: "Family TV",
            from: "si:chef",
            to: "c:bob",
            session_id: None,
            reason: "OTP",
            routed_to: Some(extend_protocol::model::RequestRoute::Carbon),
            team: None,
            link: Some("https://extend.teamofsilicons.com/devices/0d44e1f2".into()),
            from_hidden: false,
        };
        let b = request_body("extend", "acme", &t);
        assert_eq!(b["for"], "c:bob");
        assert_eq!(b["data"]["from"], "si:chef");
        assert_eq!(
            b["data"]["summary"],
            "si:chef asks to use Family TV (0d44e1f2), which one of your Silicons is using: OTP. Stop it with: extend device stop 0d44e1f2"
        );
        for gone in ["session_id", "end_session", "team"] {
            assert!(b["data"].get(gone).is_none(), "{gone}: {b}");
        }
        // The 1.0.0 shape, for rows 1.0.0 sent first.
        let old = DeviceRequestTing {
            routed_to: None,
            to: "si:sous",
            session_id: Some("a3f"),
            link: None,
            ..t
        };
        let b = request_body("extend", "acme", &old);
        assert_eq!(b["data"]["end_session"], "extend session end a3f");
        assert!(b["data"].get("routed_to").is_none());
    }
}
