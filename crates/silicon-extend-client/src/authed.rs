//! The account API (API v2) as a signed-in Carbon or Silicon: [`Authed`].

use extend_protocol::DeviceId;
use extend_protocol::account::{AccountMe, AccountRef, SignOut, SiliconSummary};
use extend_protocol::envelope::Page;
use extend_protocol::model::*;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::{Client, Error, Result, decode, decode_envelope, env, expect_empty};

/// Filters for listing devices.
#[derive(Debug, Clone, Default)]
pub struct DeviceQuery {
    /// `mine` (a Carbon's default: every device they paired) or `accessible` (a Silicon's default:
    /// the devices it has access to). Devices are private: there is no shared list.
    pub scope: Option<String>,
    pub online: Option<bool>,
    pub os: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    /// Only this Silicon's actions (its `si:` id or uuid).
    pub silicon_id: Option<String>,
    pub session_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub device_id: Option<String>,
    pub session_id: Option<String>,
    pub state: Option<String>,
    pub kind: Option<String>,
    pub direction: Option<String>,
    /// A Carbon's view of a Silicon they look after (its custodian): its `si:` id or uuid.
    /// Sessions, files and requests take it.
    pub silicon: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
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

/// A path segment for an account named by id or uuid (`si:chef`, `zQo`): ids and uuids are made of
/// letters, digits, `-` and one `:`, which a path segment takes as they are; anything else is
/// percent-encoded.
fn seg(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b':' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// What [`Authed::stop`] stopped.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StopOutcome {
    /// The session ran through the caller's own pair of the device (envelope type `session`).
    Session(Box<Session>),
    /// The Silicon using the device was given access by another Carbon who paired it, so it isn't
    /// named (envelope type `device_stopped`).
    Other(DeviceStopped),
}

/// Calls made as a signed-in Carbon or Silicon (`Authorization: Bearer <access token>`), on
/// `/api/v2`. Build it with [`Client::authed`].
#[derive(Debug, Clone, Copy)]
pub struct Authed<'a> {
    c: &'a Client,
    token: &'a str,
}

impl<'a> Authed<'a> {
    pub(crate) fn new(c: &'a Client, token: &'a str) -> Self {
        Self { c, token }
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.c.req(method, path).bearer_auth(self.token)
    }
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        decode(self.c.send(self.req(Method::GET, path)).await?).await
    }
    async fn post<T: DeserializeOwned, B: Serialize>(&self, path: &str, kind: &str, body: B, idem: bool) -> Result<T> {
        let mut r = self.req(Method::POST, path).json(&env(kind, body));
        if idem {
            r = r.header("Idempotency-Key", Uuid::new_v4().to_string());
        }
        decode(self.c.send(r).await?).await
    }
    async fn delete(&self, path: &str) -> Result<()> {
        expect_empty(self.c.send(self.req(Method::DELETE, path)).await?).await
    }

    // ── Who, and signing out ──

    /// Who the access token belongs to (`GET /api/v2/me`): uuid, current id, kind, name, photo,
    /// and a Silicon's custodian.
    pub async fn me(&self) -> Result<AccountMe> {
        self.get("/api/v2/me").await
    }

    /// Signs this sign-in out of Extend (`POST /api/v2/auth/logout`). With `refresh_token` (the one
    /// the caller holds), Extend revokes it in Silicon Accounts; without one, the access token's
    /// sign-in. A Silicon's running sessions end; a Carbon's sign-out ends the sessions of the
    /// Silicons they gave access to, through their own pairs only. If Extend can't reach Silicon
    /// Accounts nothing changes (`service_unavailable`): revoke there directly with
    /// [`crate::auth::SignIn::revoke`].
    pub async fn sign_out(&self, refresh_token: Option<&str>) -> Result<()> {
        let r = self
            .req(Method::POST, "/api/v2/auth/logout")
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&env("logout", SignOut::new(refresh_token.map(str::to_owned))));
        expect_empty(self.c.send(r).await?).await
    }

    /// Who an id belongs to (`c:ada`, `si:scout`), through Silicon Accounts. Current ids only: an id
    /// nobody has is `422 invalid_input`.
    pub async fn lookup(&self, id: &str) -> Result<AccountRef> {
        self.get(&format!(
            "/api/v2/accounts/lookup{}",
            qs(&[("id", Some(id.to_owned()))])
        ))
        .await
    }

    // ── Devices ──

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

    /// Claims the pairing code a device shows: the device becomes the caller's pair of it.
    /// `claim.silicon_ids` gives those Silicons access at once (`si:` ids or uuids).
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
        decode(self.c.send(r).await?).await
    }

    /// Shows or hides the in-use banner on a device (`in_use_indicator`). One setting for the
    /// physical device, shared by every Carbon who paired it. Carbon only.
    pub async fn set_in_use_indicator(&self, id: &str, value: InUseIndicator) -> Result<Device> {
        let r = self.req(Method::PATCH, &format!("/api/v2/devices/{id}")).json(&env(
            "device",
            &DeviceSettingsPatch {
                in_use_indicator: Some(value),
                ..Default::default()
            },
        ));
        decode(self.c.send(r).await?).await
    }

    pub async fn remove_device(&self, id: &str, version: Option<i64>) -> Result<()> {
        let mut r = self.req(Method::DELETE, &format!("/api/v2/devices/{id}"));
        if let Some(v) = version {
            r = r.header("If-Match", format!("\"{v}\""));
        }
        expect_empty(self.c.send(r).await?).await
    }

    /// Stops the Silicon using a device (`POST /api/v2/devices/{device_id}/stop`), through any
    /// Carbon's pair of it: every Carbon who paired a device may stop it. On a computer it also
    /// stops devices it carries that the caller paired. When only a carried device the caller
    /// didn't pair is busy, it fails with `conflict` (stop it at the computer).
    pub async fn stop(&self, id: &str) -> Result<StopOutcome> {
        let r = self
            .req(Method::POST, &format!("/api/v2/devices/{id}/stop"))
            .json(&env("stop", serde_json::json!({})));
        let a = decode_envelope(self.c.send(r).await?).await?;
        if a.kind.as_deref() == Some("device_stopped") {
            Ok(StopOutcome::Other(a.into()?))
        } else {
            Ok(StopOutcome::Session(Box::new(a.into()?)))
        }
    }

    pub async fn setup(&self, id: &str) -> Result<Setup> {
        self.get(&format!("/api/v2/devices/{id}/setup")).await
    }

    /// Runs a device's failed setup step again now, or every failed step when `step` is `None`.
    /// Answers the keys of the steps the device was asked to rerun; the device then reports progress
    /// as usual ([`Authed::setup`]). Fails with `conflict` when nothing (or not that step) has
    /// failed, `device_offline` when the device or its computer isn't connected, `upgrade_required`
    /// when its app is older than 1.1, and `rate_limited` within 5 seconds of the last retry.
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

    // ── Access ──

    /// Every grant on the Carbon's pair of a device (owner only).
    pub async fn access(&self, id: &str) -> Result<Vec<AccessGrant>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<AccessGrant>,
        }
        Ok(self.get::<Items>(&format!("/api/v2/devices/{id}/access")).await?.items)
    }

    /// Gives a Silicon access to the Carbon's device: any active Silicon, by its current `si:` id or
    /// its Silicon Accounts uuid. It is the Carbon's decision, so the Silicon doesn't have to accept;
    /// the grant shows to that Silicon and its custodian. Repeating it returns the existing grant.
    pub async fn grant(&self, id: &str, silicon: &str) -> Result<AccessGrant> {
        decode(
            self.c
                .send(self.req(Method::PUT, &format!("/api/v2/devices/{id}/access/{}", seg(silicon))))
                .await?,
        )
        .await
    }

    /// Takes a Silicon's access to the device away, and ends its running session there.
    pub async fn revoke(&self, id: &str, silicon: &str) -> Result<()> {
        self.delete(&format!("/api/v2/devices/{id}/access/{}", seg(silicon)))
            .await
    }

    // ── The Silicons a Carbon looks after (custodian) or gave access to ──

    /// Carbons only: the Silicons they look after (they are its custodian in Silicon Accounts),
    /// then the ones they gave access to. Nobody else's Silicons are listed.
    pub async fn silicons(&self) -> Result<Vec<SiliconSummary>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<SiliconSummary>,
        }
        Ok(self.get::<Items>("/api/v2/silicons").await?.items)
    }

    /// One Silicon the caller looks after or gave access to, by `si:` id or uuid.
    pub async fn silicon(&self, silicon: &str) -> Result<SiliconSummary> {
        self.get(&format!("/api/v2/silicons/{}", seg(silicon))).await
    }

    /// Every device a Silicon the caller looks after can use, whoever gave the access (its
    /// custodian only).
    pub async fn silicon_grants(&self, silicon: &str) -> Result<Vec<AccessGrant>> {
        #[derive(serde::Deserialize)]
        struct Items {
            items: Vec<AccessGrant>,
        }
        Ok(self
            .get::<Items>(&format!("/api/v2/silicons/{}/grants", seg(silicon)))
            .await?
            .items)
    }

    /// Gives up a Silicon's access to a device, as its custodian or as the Silicon itself: its
    /// running session there ends and its open wake requests there are withdrawn.
    pub async fn renounce(&self, silicon: &str, device_id: &str) -> Result<()> {
        self.delete(&format!("/api/v2/silicons/{}/grants/{device_id}", seg(silicon)))
            .await
    }

    // ── Activity and requests ──

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
    /// Silicon was given access through the same pair and is in the caller's circle (the same
    /// custodian), the request goes to it (`to` names it). Otherwise it goes to the Carbon who gave
    /// that Silicon access: `to_hidden` is set and `to` reads [`crate::protocol::REQUEST_TO_HIDDEN`].
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

    /// The requests a Silicon sent or received (`direction`: `sent`, `received`, `all`), or, with
    /// `q.silicon`, a Silicon's the caller looks after.
    pub async fn requests(&self, q: ListQuery) -> Result<Page<RequestInfo>> {
        self.get(&format!(
            "/api/v2/requests{}",
            qs(&[
                ("direction", q.direction),
                ("device_id", q.device_id),
                ("silicon", q.silicon),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    // ── Waking a device ──

    /// Asks the Carbon who gave this Silicon access to wake a device that isn't awake, with a reason
    /// of 1–300 characters. Asking again after 5 minutes refreshes the open request (`asks` goes
    /// up); sooner fails with `rate_limited`. Fails with `conflict` when the device is already awake
    /// or its Carbon turned wake requests off, and `device_in_use` when another Silicon is using it.
    pub async fn wake(&self, device_id: &str, reason: &str) -> Result<WakeRequest> {
        self.post(
            &format!("/api/v2/devices/{device_id}/wake-requests"),
            "wake_request",
            WakeCreate::new(reason),
            true,
        )
        .await
    }

    /// Withdraws a wake request: the Silicon's own, or one of a Silicon the caller looks after.
    pub async fn cancel_wake(&self, device_id: &str, wake_id: Uuid) -> Result<()> {
        self.delete(&format!("/api/v2/devices/{device_id}/wake-requests/{wake_id}"))
            .await
    }

    /// Wake requests on a device: the Carbon who paired it sees all of them on their pair, a Silicon
    /// its own. `q.state` is `open` or `all` (the default); `limit` and `cursor` page.
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
    /// device: it ends every open request on it, whichever Carbon's. `declined` ends the requests on
    /// the Carbon's own pair (or the listed ones).
    pub async fn answer_wake(&self, device_id: &str, answer: &WakeAnswer) -> Result<WakeAnswered> {
        let r = self
            .req(
                Method::POST,
                &format!("/api/v2/devices/{device_id}/wake-requests/answer"),
            )
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&env("wake_answer", answer));
        let resp = self.c.send(r).await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(WakeAnswered::new(answer.answer, vec![]));
        }
        decode(resp).await
    }

    /// Turns wake requests off (or on again) for the Carbon's pair of a device, or for one Silicon
    /// on it (`settings.silicon_id`). Turning them off withdraws the open ones. (A `team` is refused.)
    pub async fn set_wake_settings(&self, device_id: &str, settings: &WakeSettings) -> Result<WakeSettingsView> {
        decode(
            self.c
                .send(
                    self.req(Method::PUT, &format!("/api/v2/devices/{device_id}/wake-settings"))
                        .json(&env("wake_settings", settings)),
                )
                .await?,
        )
        .await
    }

    // ── Ting ──

    /// Whether Extend's notifications reach the caller through Ting: `on`, `off` (they turned
    /// Extend off in Ting) or `pending`; Extend's types Ting reported missing; and
    /// `delivery_enabled`, false while this server sends no notifications through Ting.
    pub async fn ting_registration(&self) -> Result<TingRegistration> {
        self.get("/api/v2/ting-registration").await
    }

    /// "Turn on": enrols the caller with Ting again and sends the Tings that waited for it.
    /// `service_unavailable` while Ting is off on the server.
    pub async fn ting_turn_on(&self) -> Result<TingRegistration> {
        decode(
            self.c
                .send(
                    self.req(Method::PUT, "/api/v2/ting-registration")
                        .json(&env("ting_registration", serde_json::json!({}))),
                )
                .await?,
        )
        .await
    }

    // ── Sessions and commands ──

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

    /// A Silicon's own sessions; a Carbon's, through their pairs; or, with `q.silicon`, the
    /// sessions of a Silicon the caller looks after.
    pub async fn sessions(&self, q: ListQuery) -> Result<Page<Session>> {
        self.get(&format!(
            "/api/v2/sessions{}",
            qs(&[
                ("device_id", q.device_id),
                ("state", q.state),
                ("silicon", q.silicon),
                ("limit", q.limit.map(|l| l.to_string())),
                ("cursor", q.cursor)
            ])
        ))
        .await
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        self.get(&format!("/api/v2/sessions/{id}")).await
    }

    /// Ends a session: the Silicon's own, or, as the custodian of the Silicon using it (or a Carbon
    /// who paired its device), stops it.
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
        self.delete(&format!("/api/v2/sessions/{id}/takeover")).await
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

    // ── Files ──

    /// The files the caller may see: a Silicon's own; a Carbon's devices' and the Silicons they look
    /// after (narrowed with `q.silicon`).
    pub async fn files(&self, q: ListQuery) -> Result<Page<FileInfo>> {
        self.get(&format!(
            "/api/v2/files{}",
            qs(&[
                ("session_id", q.session_id),
                ("device_id", q.device_id),
                ("kind", q.kind),
                ("silicon", q.silicon),
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

    /// A file's bytes, read through Extend (`GET /api/v2/files/{file_id}/content`). Works for files
    /// stored in Briefcase, which Extend reads on the caller's behalf: for the Silicon that made the
    /// file, the Carbon who paired its device, and the Silicon's custodian.
    pub async fn file_content(&self, id: &str) -> Result<FileContent> {
        self.file_download(id, None).await?.content().await
    }

    /// Starts reading a file's bytes, optionally one range (`first` byte, and `last` inclusive or
    /// to the end), to take in chunks with [`FileDownload::chunk`] (large recordings) or at once
    /// with [`FileDownload::content`].
    pub async fn file_download(&self, id: &str, range: Option<(u64, Option<u64>)>) -> Result<FileDownload> {
        let path = format!("/api/v2/files/{id}/content");
        let mut r = self.req(Method::GET, &path);
        if let Some((first, last)) = range {
            let value = match last {
                Some(last) => format!("bytes={first}-{last}"),
                None => format!("bytes={first}-"),
            };
            r = r.header("range", value);
        }
        let resp = self.c.send(r).await?;
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
        Ok(FileDownload {
            content_type: header("content-type").unwrap_or_else(|| "application/octet-stream".into()),
            name: header("content-disposition").as_deref().and_then(disposition_name),
            length: header("content-length").and_then(|v| v.parse().ok()),
            range: header("content-range").as_deref().and_then(content_range),
            url: format!("{}{path}", self.c.base_url()),
            resp,
        })
    }

    // ── Operations ──

    pub async fn report(&self, input: &ReportInput) -> Result<ReportReceipt> {
        self.post("/api/v2/reports", "report", input, true).await
    }

    /// Sends one telemetry event (fields listed in api.yaml); errors are the caller's to ignore.
    pub async fn telemetry(&self, event: serde_json::Value) -> Result<()> {
        let r = self
            .req(Method::POST, "/api/v2/telemetry")
            .json(&env("telemetry", event));
        expect_empty(self.c.send(r).await?).await
    }
}

/// A file's bytes, read through Extend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    pub bytes: Vec<u8>,
    pub content_type: String,
    /// The file's name, from `Content-Disposition`.
    pub name: Option<String>,
    /// For a partial answer: (first byte, last byte, total size).
    pub range: Option<(u64, u64, u64)>,
}

/// A file being read through Extend, taken in chunks or all at once.
#[derive(Debug)]
pub struct FileDownload {
    resp: reqwest::Response,
    url: String,
    pub content_type: String,
    /// The file's name, from `Content-Disposition`.
    pub name: Option<String>,
    /// Bytes in this answer (the whole file, or the range asked for).
    pub length: Option<u64>,
    /// For a partial answer: (first byte, last byte, total size).
    pub range: Option<(u64, u64, u64)>,
}

impl FileDownload {
    /// The next chunk, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        self.resp
            .chunk()
            .await
            .map(|c| c.map(|b| b.to_vec()))
            .map_err(|e| Error::Transport {
                url: self.url.clone(),
                source: e,
            })
    }

    /// Everything that is left.
    pub async fn content(self) -> Result<FileContent> {
        let bytes = self.resp.bytes().await.map_err(|e| Error::Transport {
            url: self.url.clone(),
            source: e,
        })?;
        Ok(FileContent {
            bytes: bytes.to_vec(),
            content_type: self.content_type,
            name: self.name,
            range: self.range,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_segments_keep_ids_readable() {
        assert_eq!(seg("si:chef"), "si:chef");
        assert_eq!(seg("zQo"), "zQo");
        assert_eq!(seg("si:a b/c"), "si:a%20b%2Fc");
    }

    #[test]
    fn queries_are_encoded() {
        assert_eq!(qs(&[("a", None)]), "");
        assert_eq!(
            qs(&[("silicon", Some("si:chef".into())), ("x", Some("a&b".into()))]),
            "?silicon=si%3Achef&x=a%26b"
        );
    }

    #[test]
    fn content_disposition_names() {
        assert_eq!(
            disposition_name("attachment; filename=\"a.png\"; filename*=UTF-8''%C3%A9t%C3%A9.png").as_deref(),
            Some("été.png")
        );
        assert_eq!(
            disposition_name("attachment; filename=\"shot.png\"").as_deref(),
            Some("shot.png")
        );
        assert_eq!(content_range("bytes 0-3/10"), Some((0, 3, 10)));
    }
}
