//! Account API v2 calls for the service tests: the 3.x client crate's account calls, on
//! `/api/v2` with a Silicon Accounts bearer token and no Team. (The device-wire calls still use
//! the 3.x client: the device wire is unchanged.) Generated from silicon-extend-client 3.1's
//! `Authed`; the client stage rewrites the crate itself.

#![allow(dead_code)]

use extend_protocol::envelope::Page;
use extend_protocol::model::*;
use extend_protocol::{ApiError, DeviceId};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use silicon_extend_client::{ActivityQuery, DeviceQuery, Error, FileContent, ListQuery, StopOutcome};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Error>;

/// Calls made as a signed-in Carbon or Silicon.
#[derive(Debug, Clone)]
pub struct V2<'a> {
    pub http: reqwest::Client,
    pub base: &'a str,
    pub token: &'a str,
}

impl<'a> V2<'a> {
    pub fn new(base: &'a str, token: &'a str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            token,
        }
    }
    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(self.token)
    }
    async fn send(&self, r: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        r.send().await.map_err(|e| Error::Transport {
            url: self.base.to_owned(),
            source: e,
        })
    }
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        decode(self.send(self.req(Method::GET, path)).await?).await
    }
    async fn post<T: DeserializeOwned, B: Serialize>(&self, path: &str, kind: &str, body: B, idem: bool) -> Result<T> {
        let mut r = self.req(Method::POST, path).json(&env(kind, body));
        if idem {
            r = r.header("Idempotency-Key", Uuid::new_v4().to_string());
        }
        decode(self.send(r).await?).await
    }

    pub async fn me(&self) -> Result<serde_json::Value> {
        self.get("/api/v2/me").await
    }

    pub async fn ting_registration(&self) -> Result<TingRegistration> {
        self.get("/api/v2/ting-registration").await
    }

    pub async fn ting_turn_on(&self) -> Result<TingRegistration> {
        let r = self.req(Method::PUT, "/api/v2/ting-registration");
        decode(self.send(r).await?).await
    }

    pub async fn file_content(&self, id: &str) -> Result<FileContent> {
        self.file_range(id, None).await
    }

    /// A file's bytes, optionally one range (`first`, and `last` inclusive or to the end).
    pub async fn file_range(&self, id: &str, range: Option<(u64, Option<u64>)>) -> Result<FileContent> {
        let path = format!("/api/v2/files/{id}/content");
        let mut r = self.req(Method::GET, &path);
        if let Some((first, last)) = range {
            let value = match last {
                Some(last) => format!("bytes={first}-{last}"),
                None => format!("bytes={first}-"),
            };
            r = r.header("range", value);
        }
        let resp = self.send(r).await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            decode::<serde_json::Value>(resp).await?;
            return Err(Error::Decode {
                status,
                detail: format!("reading file {id} failed"),
            });
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let content_type = header("content-type").unwrap_or_else(|| "application/octet-stream".into());
        let name = header("content-disposition").as_deref().and_then(disposition_name);
        let range = header("content-range").as_deref().and_then(content_range);
        let bytes = resp.bytes().await.map_err(|e| Error::Transport {
            url: path.clone(),
            source: e,
        })?;
        Ok(FileContent {
            bytes: bytes.to_vec(),
            content_type,
            name,
            range,
        })
    }
}

/// The whole envelope of a successful answer (errors as [`Error::Api`]).
async fn decode_raw(resp: reqwest::Response) -> Result<serde_json::Value> {
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|e| Error::Decode {
        status: status.as_u16(),
        detail: e.to_string(),
    })?;
    if !status.is_success() || v.get("type").and_then(|t| t.as_str()) == Some("error") {
        let error: ApiError =
            serde_json::from_value(v.get("data").cloned().unwrap_or_default()).map_err(|e| Error::Decode {
                status: status.as_u16(),
                detail: format!("error body: {e}"),
            })?;
        return Err(Error::Api {
            status: status.as_u16(),
            error: Box::new(error),
        });
    }
    Ok(v)
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| Error::Decode {
        status: status.as_u16(),
        detail: e.to_string(),
    })?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Error::Decode {
        status: status.as_u16(),
        detail: String::from_utf8_lossy(&bytes).chars().take(300).collect(),
    })?;
    if !status.is_success() || v.get("type").and_then(|t| t.as_str()) == Some("error") {
        let error: ApiError =
            serde_json::from_value(v.get("data").cloned().unwrap_or_default()).map_err(|e| Error::Decode {
                status: status.as_u16(),
                detail: format!("error body: {e}"),
            })?;
        return Err(Error::Api {
            status: status.as_u16(),
            error: Box::new(error),
        });
    }
    serde_json::from_value(v.get("data").cloned().unwrap_or(serde_json::Value::Null)).map_err(|e| Error::Decode {
        status: status.as_u16(),
        detail: e.to_string(),
    })
}

async fn expect_empty(resp: reqwest::Response) -> Result<()> {
    if resp.status().is_success() {
        return Ok(());
    }
    decode::<serde_json::Value>(resp).await.map(|_| ())
}

fn env<T: Serialize>(kind: &str, data: T) -> serde_json::Value {
    serde_json::json!({"type": kind, "data": data})
}

fn qs(pairs: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| {
            v.as_ref().map(|v| {
                format!(
                    "{k}={}",
                    url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

impl V2<'_> {
    pub async fn devices(&self, q: DeviceQuery) -> Result<Page<Device>> {
        self.get(&format!(
            "/api/v2/devices{}",
            qs(&[
                ("scope", q.scope),
                ("online", q.online.map(|b| b.to_string())),
                ("os", q.os),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    /// Like [`Authed::devices`], but also lists the Carbon's removed devices, each with
    /// `removed_at` and `removed_reason` set (`include_removed=true`; `scope=mine` only). A removed
    /// device's record and activity log stay readable with [`Authed::device`] and
    /// [`Authed::activity`].
    pub async fn devices_including_removed(&self, q: DeviceQuery) -> Result<Page<Device>> {
        self.get(&format!(
            "/api/v2/devices{}",
            qs(&[
                ("scope", q.scope),
                ("online", q.online.map(|b| b.to_string())),
                ("os", q.os),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor),
                ("include_removed", Some("true".to_owned()))
            ])
        ))
        .await
    }

    pub async fn device(&self, id: &str) -> Result<Device> {
        self.get(&format!("/api/v2/devices/{id}")).await
    }

    pub async fn pair(&self, claim: &PairingClaim) -> Result<Device> {
        self.post("/api/v2/pairings", "pairing", claim, true).await
    }

    pub async fn attach(&self, host_id: &str, input: &AttachmentCreate) -> Result<Device> {
        self.post(
            &format!("/api/v2/devices/{host_id}/attachments"),
            "attachment",
            input,
            true,
        )
        .await
    }

    pub async fn update_device(&self, id: &str, version: Option<i64>, patch: &DevicePatch) -> Result<Device> {
        let mut r = self
            .req(Method::PATCH, &format!("/api/v2/devices/{id}"))
            .json(&env("device", patch));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        decode(self.send(r).await?).await
    }

    /// Shows or hides the in-use banner on a device (`PATCH /api/v2/devices/{id}` with
    /// `in_use_indicator`, 1.1). One setting for the physical device, shared by every Carbon who
    /// paired it. Carbon only: a Silicon gets `carbon_only`.
    pub async fn set_in_use_indicator(&self, id: &str, value: InUseIndicator) -> Result<Device> {
        let r = self.req(Method::PATCH, &format!("/api/v2/devices/{id}")).json(&env(
            "device",
            &DeviceSettingsPatch {
                in_use_indicator: Some(value),
                ..Default::default()
            },
        ));
        decode(self.send(r).await?).await
    }

    pub async fn remove_device(&self, id: &str, version: Option<i64>) -> Result<()> {
        let mut r = self.req(Method::DELETE, &format!("/api/v2/devices/{id}"));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        expect_empty(self.send(r).await?).await
    }

    /// Stops the Silicon using a device, answering the stopped session. A 1.1 service answers
    /// `device_stopped` instead when that Silicon was given access by another Carbon who paired the
    /// device; this call then fails to decode after the stop succeeded. [`Authed::stop`] reads both.
    pub async fn stop_device(&self, id: &str) -> Result<Session> {
        self.post(
            &format!("/api/v2/devices/{id}/stop"),
            "stop",
            serde_json::json!({}),
            false,
        )
        .await
    }

    /// Stops the Silicon using a device (`POST /api/v2/devices/{device_id}/stop`), through any
    /// Carbon's pair of it: every Carbon who paired a device may stop it. On a computer it also
    /// stops devices it carries that the caller paired. When only a carried device the caller
    /// didn't pair is busy, it fails with `conflict` (stop it at the computer).
    pub async fn stop(&self, id: &str) -> Result<StopOutcome> {
        let r = self
            .req(Method::POST, &format!("/api/v2/devices/{id}/stop"))
            .json(&env("stop", serde_json::json!({})));
        let resp = self.send(r).await?;
        let v: serde_json::Value = decode_raw(resp).await?;
        let kind = v.get("type").and_then(|t| t.as_str()).map(str::to_owned);
        let data = v.get("data").cloned().unwrap_or_default();
        let parse = |d: serde_json::Value| -> Result<_> { Ok(d) };
        let data = parse(data)?;
        if kind.as_deref() == Some("device_stopped") {
            Ok(StopOutcome::Other(serde_json::from_value(data).map_err(|e| {
                Error::Decode {
                    status: 200,
                    detail: e.to_string(),
                }
            })?))
        } else {
            Ok(StopOutcome::Session(Box::new(serde_json::from_value(data).map_err(
                |e| Error::Decode {
                    status: 200,
                    detail: e.to_string(),
                },
            )?)))
        }
    }

    pub async fn setup(&self, id: &str) -> Result<Setup> {
        self.get(&format!("/api/v2/devices/{id}/setup")).await
    }

    /// Runs a device's failed setup step again now (`POST /api/v2/devices/{device_id}/setup/retry`,
    /// 1.1), or every failed step when `step` is `None`. Answers the keys of the steps the device
    /// was asked to rerun; the device then reports progress as usual ([`Authed::setup`]). Fails
    /// with `conflict` when nothing (or not that step) has failed, `device_offline` when the device
    /// or its computer isn't connected, `upgrade_required` when its app is older than 1.1, and
    /// `rate_limited` within 5 seconds of the last retry.
    pub async fn retry_setup(&self, id: &str, step: Option<&str>) -> Result<RetryResult> {
        let input = match step {
            Some(k) => SetupRetryInput::step(k),
            None => SetupRetryInput::all(),
        };
        self.post(
            &format!("/api/v2/devices/{id}/setup/retry"),
            "setup_retry",
            input,
            false,
        )
        .await
    }

    pub async fn setup_code(&self, id: &str, code: &str) -> Result<Setup> {
        self.post(
            &format!("/api/v2/devices/{id}/setup/code"),
            "setup_code",
            serde_json::json!({"code": code}),
            false,
        )
        .await
    }

    pub async fn access(&self, id: &str) -> Result<Vec<AccessGrant>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<AccessGrant>,
        }
        Ok(self.get::<Items>(&format!("/api/v2/devices/{id}/access")).await?.items)
    }

    /// Gives a Silicon access to the Carbon's device, in this [`Authed`]'s Team (the Silicon's
    /// Team). The Carbon's login must reach that Team (`not_a_team_member` otherwise).
    pub async fn grant(&self, id: &str, silicon_id: &str) -> Result<AccessGrant> {
        decode(
            self.send(self.req(Method::PUT, &format!("/api/v2/devices/{id}/access/{silicon_id}")))
                .await?,
        )
        .await
    }

    /// Takes a Silicon's access to the device away, in every Team it was given in, and ends its
    /// running session there.
    pub async fn revoke(&self, id: &str, silicon_id: &str) -> Result<()> {
        expect_empty(
            self.send(self.req(Method::DELETE, &format!("/api/v2/devices/{id}/access/{silicon_id}")))
                .await?,
        )
        .await
    }

    pub async fn activity(&self, id: &str, q: ActivityQuery) -> Result<Page<ActivityEntry>> {
        self.get(&format!(
            "/api/v2/devices/{id}/activity{}",
            qs(&[
                ("silicon_id", q.silicon_id),
                ("session_id", q.session_id),
                ("since", q.since),
                ("until", q.until),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn device_requests(&self, id: &str, q: ListQuery) -> Result<Page<RequestInfo>> {
        self.get(&format!(
            "/api/v2/devices/{id}/requests{}",
            qs(&[("limit", q.limit.map(|l| l.to_string())), ("cursor", q.cursor)])
        ))
        .await
    }

    /// Asks for a device another Silicon is using, with a reason of 1–300 characters. When that
    /// Silicon is in the caller's Team and was given access by the same Carbon, the request goes to
    /// it (`to` names it). Otherwise it goes to the Carbon who gave that Silicon access: `to_hidden`
    /// is set and `to` reads [`protocol::REQUEST_TO_HIDDEN`].
    pub async fn send_request(&self, device_id: &str, reason: &str) -> Result<RequestInfo> {
        self.post(
            &format!("/api/v2/devices/{device_id}/requests"),
            "request",
            RequestCreate {
                reason: reason.to_owned(),
            },
            true,
        )
        .await
    }

    // ── Waking a device (1.1) ──

    /// Asks the Carbon who gave this Silicon access to wake a device that isn't awake
    /// (`POST /api/v2/devices/{device_id}/wake-requests`), with a reason of 1–300 characters. The
    /// device shows it where it can, and the Carbon gets it through Ting. Asking again after 5
    /// minutes refreshes the open request (`asks` goes up); sooner fails with `rate_limited`. Fails
    /// with `conflict` when the device is already awake or its Carbon turned wake requests off, and
    /// `device_in_use` when another Silicon is using it.
    pub async fn wake(&self, device_id: &str, reason: &str) -> Result<WakeRequest> {
        self.post(
            &format!("/api/v2/devices/{device_id}/wake-requests"),
            "wake_request",
            WakeCreate::new(reason),
            true,
        )
        .await
    }

    /// Withdraws the Silicon's own open wake request.
    pub async fn cancel_wake(&self, device_id: &str, wake_id: Uuid) -> Result<()> {
        expect_empty(
            self.send(self.req(
                Method::DELETE,
                &format!("/api/v2/devices/{device_id}/wake-requests/{wake_id}"),
            ))
            .await?,
        )
        .await
    }

    /// Wake requests on a device: the Carbon who paired it sees every Team's, a Silicon its own.
    /// `q.state` is `open` or `all` (the default); `limit` and `cursor` page.
    pub async fn wake_requests(&self, device_id: &str, q: ListQuery) -> Result<Page<WakeRequest>> {
        self.get(&format!(
            "/api/v2/devices/{device_id}/wake-requests{}",
            qs(&[
                ("state", q.state),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    /// The Carbon answers wake requests on their device. `woken` ("It's awake") is about the
    /// device: it ends every open request on it, whichever Carbon's and whichever Team's. `declined`
    /// ends the requests on the Carbon's own pair (or the listed ones).
    pub async fn answer_wake(&self, device_id: &str, answer: &WakeAnswer) -> Result<WakeAnswered> {
        let r = self
            .req(
                Method::POST,
                &format!("/api/v2/devices/{device_id}/wake-requests/answer"),
            )
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&env("wake_answer", answer));
        let resp = self.send(r).await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(WakeAnswered::new(answer.answer, vec![]));
        }
        decode(resp).await
    }

    /// Turns wake requests off (or on again) for the Carbon's pair of a device, or for one Silicon
    /// on it, in every Team or only `settings.team`. Turning them off withdraws the open ones.
    pub async fn set_wake_settings(&self, device_id: &str, settings: &WakeSettings) -> Result<WakeSettingsView> {
        decode(
            self.send(
                self.req(Method::PUT, &format!("/api/v2/devices/{device_id}/wake-settings"))
                    .json(&env("wake_settings", settings)),
            )
            .await?,
        )
        .await
    }

    // ── Ting (1.1) ──

    pub async fn requests(&self, q: ListQuery) -> Result<Page<RequestInfo>> {
        self.get(&format!(
            "/api/v2/requests{}",
            qs(&[
                ("direction", q.direction),
                ("device_id", q.device_id),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn start_session(&self, device_id: &DeviceId) -> Result<Session> {
        self.post(
            "/api/v2/sessions",
            "session",
            SessionCreate {
                device_id: device_id.clone(),
            },
            true,
        )
        .await
    }

    pub async fn sessions(&self, q: ListQuery) -> Result<Page<Session>> {
        self.get(&format!(
            "/api/v2/sessions{}",
            qs(&[
                ("device_id", q.device_id),
                ("state", q.state),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        self.get(&format!("/api/v2/sessions/{id}")).await
    }

    pub async fn end_session(&self, id: &str) -> Result<Session> {
        self.post(
            &format!("/api/v2/sessions/{id}/end"),
            "end",
            serde_json::json!({}),
            false,
        )
        .await
    }

    pub async fn takeover(&self, id: &str, reason: &str) -> Result<Takeover> {
        self.post(
            &format!("/api/v2/sessions/{id}/takeover"),
            "takeover",
            TakeoverCreate {
                reason: reason.to_owned(),
            },
            false,
        )
        .await
    }

    pub async fn takeover_status(&self, id: &str) -> Result<Option<Takeover>> {
        self.get(&format!("/api/v2/sessions/{id}/takeover")).await
    }

    pub async fn release_takeover(&self, id: &str) -> Result<()> {
        expect_empty(
            self.send(self.req(Method::DELETE, &format!("/api/v2/sessions/{id}/takeover")))
                .await?,
        )
        .await
    }

    /// Runs one command in a session and waits for the device's answer.
    pub async fn run(&self, session_id: &str, cmd: &CommandRequest) -> Result<CommandResult> {
        self.post(
            &format!("/api/v2/sessions/{session_id}/commands"),
            "command",
            cmd,
            false,
        )
        .await
    }

    pub async fn files(&self, q: ListQuery) -> Result<Page<FileInfo>> {
        self.get(&format!(
            "/api/v2/files{}",
            qs(&[
                ("session_id", q.session_id),
                ("device_id", q.device_id),
                ("kind", q.kind),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn file(&self, id: &str) -> Result<FileInfo> {
        self.get(&format!("/api/v2/files/{id}")).await
    }

    pub async fn keep_file(&self, id: &str) -> Result<FileInfo> {
        self.post(
            &format!("/api/v2/files/{id}/keep"),
            "keep",
            serde_json::json!({}),
            false,
        )
        .await
    }

    pub async fn report(&self, input: &ReportInput) -> Result<ReportReceipt> {
        self.post("/api/v2/reports", "report", input, true).await
    }

    /// Sends one telemetry event (fields listed in api.yaml); errors are the caller's to ignore.
    pub async fn telemetry(&self, event: serde_json::Value) -> Result<()> {
        let r = self
            .req(Method::POST, "/api/v2/telemetry")
            .json(&env("telemetry", event));
        expect_empty(self.send(r).await?).await
    }
}

/// The file name in a `Content-Disposition` header: the RFC 8187 `filename*` when present,
/// else `filename`.
fn disposition_name(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        if let Some(v) = part.strip_prefix("filename*=") {
            let encoded = v.split_once("''").map_or(v, |(_, e)| e);
            let mut out = Vec::new();
            let bytes = encoded.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%'
                    && let Some(b) = encoded.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            if let Ok(s) = String::from_utf8(out) {
                return Some(s);
            }
        } else if let Some(v) = part.strip_prefix("filename=") {
            plain = Some(v.trim_matches('"').to_owned());
        }
    }
    plain
}

/// `bytes first-last/total` → (first, last, total).
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (span, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    Some((first.parse().ok()?, last.parse().ok()?, total.parse().ok()?))
}
