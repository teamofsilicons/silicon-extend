//! The agent: pairs the computer, keeps its device socket open, runs commands, and keeps the
//! status the tray, window and `status` command show (`docs/device-protocol.md`).

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bridge_driver::{Driver, Probe};
use bridge_protocol::frames::{DeviceFrame, Hello, ServiceFrame, close};
use bridge_protocol::model::{EnrollmentCreate, TestingEnvironment};
use bridge_protocol::DeviceId;
use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::{APP_VERSION, Config};
use crate::credential::{CredentialStore, StoredCredential, load_for};
use crate::dispatch::{Dispatcher, DriverLookup, Outbox, Uploader};
use crate::enroll::{self, EnrollOutcome, sleep_or_shutdown};
use crate::hosted::{AttachRecord, DriverFactory, HostedRegistry};
use crate::service::ServiceClient;
use crate::status::{AgentStatus, DeviceInfo, EnvironmentInfo, InUseInfo, Phase, StatusHandle, TakeoverInfo};
use crate::ws::{self, Backoff, ConnectError, Socket};

/// Something the Carbon did in the tray or window (or `bridge-agent` did for them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    /// Stop the Silicon using this computer (or, with a target, a device it carries).
    Stop { target: Option<DeviceId> },
    /// Done on a takeover.
    TakeoverDone { target: Option<DeviceId> },
    /// Revoke pair. The UI has already confirmed with the Carbon.
    RevokePair,
    /// Try connecting again now (after being superseded, or to skip a backoff wait).
    Reconnect,
    /// Check permissions and helpers again now.
    Reprobe,
}

pub struct AgentDeps {
    pub config: Config,
    pub local: Arc<dyn Driver>,
    pub hosted_factory: DriverFactory,
    pub credentials: Arc<dyn CredentialStore>,
    /// How often capabilities are re-checked (30 s; faster while setup is incomplete).
    pub probe_interval: Duration,
}

/// What the UI keeps to talk to the agent.
#[derive(Clone)]
pub struct AgentHandle {
    pub status: StatusHandle,
    pub actions: mpsc::UnboundedSender<UiAction>,
    pub shutdown: CancellationToken,
}

pub struct Agent {
    config: Config,
    service: ServiceClient,
    status: StatusHandle,
    local: Arc<dyn Driver>,
    hosted: Arc<HostedRegistry>,
    credentials: Arc<dyn CredentialStore>,
    credential: Arc<RwLock<Option<String>>>,
    outbox: Outbox,
    dispatcher: Dispatcher,
    actions: mpsc::UnboundedReceiver<UiAction>,
    shutdown: CancellationToken,
    probe_interval: Duration,
}

/// How a paired stretch ended.
#[derive(Debug, PartialEq)]
enum PairedEnd {
    Unpaired(String),
    Superseded,
    UpgradeRequired,
    Shutdown,
}

/// How one device-socket connection ended.
enum ConnEnd {
    Dropped(String),
    Paired(PairedEnd),
}

struct Lookup {
    local: Arc<dyn Driver>,
    hosted: Arc<HostedRegistry>,
}

impl DriverLookup for Lookup {
    fn driver_for(&self, target: Option<&DeviceId>) -> Result<Arc<dyn Driver>, String> {
        match target {
            None => Ok(self.local.clone()),
            Some(id) => self.hosted.driver(id),
        }
    }
}

struct ServiceUploader {
    service: ServiceClient,
    credential: Arc<RwLock<Option<String>>>,
}

#[async_trait]
impl Uploader for ServiceUploader {
    async fn upload(&self, upload_id: Uuid, path: &Path, name: &str, content_type: &str) -> Result<u64, String> {
        let cred = self.credential.read().unwrap().clone().ok_or("this computer isn't paired")?;
        self.service
            .upload_artifact(&cred, upload_id, path, name, content_type)
            .await
            .map(|(_, size)| size)
            .map_err(|e| e.message)
    }
}

impl Agent {
    pub fn new(deps: AgentDeps) -> (Self, AgentHandle) {
        let AgentDeps { config, local, hosted_factory, credentials, probe_interval } = deps;
        let service = ServiceClient::new(config.service_url.clone());
        let status = StatusHandle::new(AgentStatus {
            pid: std::process::id(),
            app_version: APP_VERSION.into(),
            service_url: config.service_url.to_string(),
            credential_store: credentials.describe(),
            ..Default::default()
        });
        let hosted = Arc::new(HostedRegistry::new(
            hosted_factory,
            config.hosted_dir(),
            config.agent_device.clone().unwrap_or_default(),
        ));
        let credential = Arc::new(RwLock::new(None));
        let outbox = Outbox::default();
        let dispatcher = Dispatcher::new(
            Arc::new(Lookup { local: local.clone(), hosted: hosted.clone() }),
            Arc::new(ServiceUploader { service: service.clone(), credential: credential.clone() }),
            outbox.clone(),
            config.work_dir(),
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let handle = AgentHandle { status: status.clone(), actions: tx, shutdown: shutdown.clone() };
        let agent = Self {
            config,
            service,
            status,
            local,
            hosted,
            credentials,
            credential,
            outbox,
            dispatcher,
            actions: rx,
            shutdown,
            probe_interval,
        };
        (agent, handle)
    }

    /// Runs until shutdown.
    pub async fn run(mut self) {
        let _ = tokio::fs::remove_dir_all(self.config.work_dir()).await;
        tokio::spawn(crate::status::persist(self.status.clone(), self.config.status_path()));
        self.hosted.restore();
        // The first probe fills in what the window shows while pairing.
        let probe = self.probe_local().await;
        self.apply_probe_to_status(&probe);

        loop {
            if self.shutdown.is_cancelled() {
                break;
            }
            let stored = match load_for(self.credentials.as_ref(), &self.config.service_url) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("couldn't read the device credential: {e:#}");
                    self.status.update(|s| s.last_error = Some(format!("Couldn't read the device credential: {e:#}")));
                    None
                }
            };
            let Some(stored) = stored else {
                match self.enroll().await {
                    Some(true) => continue,
                    Some(false) => {
                        // Upgrade required: wait for an update (or a retry an hour later).
                        self.status.update(|s| s.phase = Phase::UpgradeRequired);
                        if self.wait_for_retry(Duration::from_secs(3600)).await {
                            break;
                        }
                        continue;
                    }
                    None => break,
                }
            };
            *self.credential.write().unwrap() = Some(stored.device_credential.clone());
            self.status.update(|s| {
                s.pairing = None;
                let d = s.device.get_or_insert_with(Default::default);
                d.device_id = stored.device_id.to_string();
            });
            match self.paired(&stored).await {
                PairedEnd::Shutdown => break,
                PairedEnd::Unpaired(reason) => {
                    tracing::info!("unpaired ({reason}); showing a pairing code again");
                    self.forget_pair().await;
                }
                PairedEnd::Superseded => {
                    self.status.update(|s| s.phase = Phase::Superseded);
                    if self.wait_for_retry(Duration::MAX).await {
                        break;
                    }
                }
                PairedEnd::UpgradeRequired => {
                    self.status.update(|s| s.phase = Phase::UpgradeRequired);
                    if self.wait_for_retry(Duration::from_secs(3600)).await {
                        break;
                    }
                }
            }
        }
        // Leave the computer tidy: close agent-device sessions that were open.
        if let Some(u) = self.status.get().in_use {
            let _ = tokio::time::timeout(Duration::from_secs(10), self.local.session_ended(&u.session_id)).await;
        }
    }

    /// Waits for `Reconnect`, `d`, or shutdown. True on shutdown.
    async fn wait_for_retry(&mut self, d: Duration) -> bool {
        let sleep = tokio::time::sleep(d.min(Duration::from_secs(86_400 * 365)));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return false,
                _ = self.shutdown.cancelled() => return true,
                a = self.actions.recv() => match a {
                    Some(UiAction::Reconnect) => return false,
                    Some(other) => tracing::debug!("ignoring {other:?} while not connected"),
                    None => return true,
                },
            }
        }
    }

    /// Some(true) paired, Some(false) upgrade required, None shutdown.
    async fn enroll(&mut self) -> Option<bool> {
        let probe = self.probe_local().await;
        let info = EnrollmentCreate {
            os: crate::sysinfo::device_os(),
            os_version: probe.os_version.clone(),
            model: probe.model.clone(),
            app_version: APP_VERSION.into(),
            agent_device_version: probe.agent_device_version.clone(),
        };
        let service = self.service.clone();
        let status = self.status.clone();
        let shutdown = self.shutdown.clone();
        let fut = async move { enroll::enroll(&service, &status, &info, &shutdown).await };
        tokio::pin!(fut);
        let outcome = loop {
            tokio::select! {
                o = &mut fut => break o,
                // Actions for a paired computer mean nothing now; drop them so none fire later.
                a = self.actions.recv() => {
                    a.as_ref()?;
                    if a == Some(UiAction::Reprobe) {
                        let p = self.probe_local().await;
                        self.apply_probe_to_status(&p);
                    }
                }
            }
        };
        match outcome {
            EnrollOutcome::Shutdown => None,
            EnrollOutcome::UpgradeRequired => Some(false),
            EnrollOutcome::Paired(p) => {
                let stored = StoredCredential {
                    device_id: p.device_id.clone(),
                    device_credential: p.device_credential,
                    service_url: self.config.service_url.to_string(),
                };
                if let Err(e) = self.credentials.save(&stored) {
                    tracing::error!("couldn't store the device credential: {e:#}");
                    self.status.update(|s| s.last_error = Some(format!("Couldn't store the device credential: {e:#}")));
                    // Keep going in memory; the pair works until the app restarts.
                }
                tracing::info!("paired as device {}", p.device_id);
                let env = p.environment.as_ref().map(environment_info);
                self.status.update(|s| {
                    s.phase = Phase::Reconnecting;
                    s.pairing = None;
                    s.environment = env;
                    s.device = Some(DeviceInfo { device_id: p.device_id.to_string(), ..Default::default() });
                });
                if self.credentials.load().ok().flatten().is_none() {
                    // The store failed; run this pair from memory.
                    let stored_clone = stored.clone();
                    let end = self.paired(&stored_clone).await;
                    if end == PairedEnd::Shutdown {
                        return None;
                    }
                    self.forget_pair().await;
                }
                Some(true)
            }
        }
    }

    async fn forget_pair(&mut self) {
        if let Err(e) = self.credentials.clear() {
            tracing::error!("couldn't remove the device credential: {e:#}");
        }
        *self.credential.write().unwrap() = None;
        if let Some(u) = self.status.get().in_use {
            self.local.session_ended(&u.session_id).await;
        }
        for id in self.hosted.ids() {
            self.hosted.remove(&id);
        }
        self.status.update(|s| {
            s.phase = Phase::Enrolling;
            s.pairing = None;
            s.device = None;
            s.in_use = None;
            s.takeover = None;
            s.environment = None;
            s.attached.clear();
        });
    }

    /// Keeps the device socket up until the pair ends, the app is superseded, or shutdown.
    async fn paired(&mut self, stored: &StoredCredential) -> PairedEnd {
        let url = self.service.ws_url("api/v1/device/connect");
        let auth = crate::service::device_auth(&stored.device_credential);
        let mut backoff = Backoff::default();
        loop {
            self.status.update(|s| if s.phase != Phase::Online { s.phase = Phase::Reconnecting });
            let connect = ws::connect(&url, &auth);
            tokio::pin!(connect);
            let socket = loop {
                tokio::select! {
                    r = &mut connect => break r,
                    _ = self.shutdown.cancelled() => return PairedEnd::Shutdown,
                    a = self.actions.recv() => {
                        let Some(a) = a else { return PairedEnd::Shutdown };
                        if let Some(end) = self.offline_action(a, &stored.device_credential).await { return end; }
                    }
                }
            };
            let end = match socket {
                Ok(socket) => {
                    let started = Instant::now();
                    let end = self.connection(socket, stored).await;
                    self.outbox.clear();
                    if started.elapsed() > Duration::from_secs(60) {
                        backoff.reset();
                    }
                    end
                }
                Err(ConnectError::Http(401 | 403 | 404)) => ConnEnd::Paired(PairedEnd::Unpaired("credential refused".into())),
                Err(ConnectError::Http(426)) => ConnEnd::Paired(PairedEnd::UpgradeRequired),
                Err(e) => ConnEnd::Dropped(e.to_string()),
            };
            match end {
                ConnEnd::Paired(p) => return p,
                ConnEnd::Dropped(why) => {
                    tracing::info!("device socket down: {why}");
                    self.status.update(|s| {
                        s.phase = Phase::Reconnecting;
                        s.last_error = Some(why.clone());
                    });
                    let wait = backoff.next_delay();
                    let sleep = tokio::time::sleep(wait);
                    tokio::pin!(sleep);
                    loop {
                        tokio::select! {
                            _ = &mut sleep => break,
                            _ = self.shutdown.cancelled() => return PairedEnd::Shutdown,
                            a = self.actions.recv() => {
                                let Some(a) = a else { return PairedEnd::Shutdown };
                                if a == UiAction::Reconnect { break; }
                                if let Some(end) = self.offline_action(a, &stored.device_credential).await { return end; }
                            }
                        }
                    }
                }
            }
        }
    }

    /// A tray action while the socket is down: Stop and Revoke go over HTTP.
    async fn offline_action(&mut self, a: UiAction, credential: &str) -> Option<PairedEnd> {
        match a {
            UiAction::Stop { target: None } => {
                if let Err(e) = self.service.stop(credential).await {
                    self.status.update(|s| s.last_error = Some(format!("Couldn't stop: {}", e.message)));
                }
                None
            }
            UiAction::RevokePair => self.revoke(credential).await,
            UiAction::Reprobe => {
                let p = self.probe_local().await;
                self.apply_probe_to_status(&p);
                None
            }
            other => {
                tracing::debug!("ignoring {other:?} while Bridge is unreachable");
                None
            }
        }
    }

    async fn revoke(&mut self, credential: &str) -> Option<PairedEnd> {
        match self.service.revoke_pair(credential).await {
            Ok(()) => Some(PairedEnd::Unpaired("revoked on this computer".into())),
            Err(e) if e.is_auth() => Some(PairedEnd::Unpaired("already unpaired".into())),
            Err(e) => {
                self.status.update(|s| s.last_error = Some(format!("Couldn't revoke the pair: {}", e.message)));
                None
            }
        }
    }

    /// One live device socket.
    async fn connection(&mut self, socket: Socket, stored: &StoredCredential) -> ConnEnd {
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<DeviceFrame>();
        self.outbox.set(tx.clone());

        // hello first, then what's attached. The computer counts as online once hello is out.
        let probe = self.probe_local().await;
        let mut last_hello = hello_from(&probe);
        self.apply_probe_to_status(&probe);
        if send_frame(&mut sink, &DeviceFrame::Hello(last_hello.clone())).await.is_err() {
            return ConnEnd::Dropped("couldn't send hello".into());
        }
        self.status.update(|s| {
            s.phase = Phase::Online;
            s.last_error = None;
        });
        tracing::info!("connected to Bridge as device {}", stored.device_id);
        self.hosted.forget_sent();
        self.spawn_hosted_probe(true);
        if let Some(end) = self.refresh_device(&stored.device_credential).await {
            return ConnEnd::Paired(end);
        }

        let (probe_tx, mut probe_rx) = mpsc::unbounded_channel::<Probe>();
        let mut last_probe = Instant::now();
        let mut probing = false;
        let mut ticker = tokio::time::interval(Duration::from_secs(5).min(self.probe_interval));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let idle = Duration::from_secs(bridge_protocol::OFFLINE_AFTER_S + 15);
        let mut last_heard = Instant::now();

        loop {
            let deadline = last_heard + idle;
            tokio::select! {
                msg = stream.next() => {
                    last_heard = Instant::now();
                    let msg = match msg {
                        None => return ConnEnd::Dropped("Bridge closed the connection".into()),
                        Some(Err(e)) => return ConnEnd::Dropped(e.to_string()),
                        Some(Ok(m)) => m,
                    };
                    match msg {
                        Message::Text(text) => match serde_json::from_str::<ServiceFrame>(&text) {
                            Ok(frame) => {
                                if let Some(end) = self.handle_frame(frame, &tx, stored).await {
                                    let _ = sink.send(Message::Close(None)).await;
                                    return ConnEnd::Paired(end);
                                }
                            }
                            Err(e) => tracing::debug!("ignoring a frame this version doesn't know ({e}): {}", truncate(&text, 200)),
                        },
                        Message::Ping(data) => {
                            let _ = sink.send(Message::Pong(data)).await;
                        }
                        Message::Close(frame) => {
                            let code = frame.as_ref().map(|f| u16::from(f.code));
                            return match code {
                                Some(close::UNAUTHORIZED) => ConnEnd::Paired(PairedEnd::Unpaired("credential refused (4401)".into())),
                                Some(close::SUPERSEDED) => ConnEnd::Paired(PairedEnd::Superseded),
                                Some(close::UPGRADE_REQUIRED) => ConnEnd::Paired(PairedEnd::UpgradeRequired),
                                other => ConnEnd::Dropped(format!("Bridge closed the connection ({})", other.map_or("no code".into(), |c| c.to_string()))),
                            };
                        }
                        _ => {}
                    }
                }
                frame = rx.recv() => {
                    let Some(frame) = frame else { return ConnEnd::Dropped("outbox closed".into()) };
                    if let Err(e) = send_frame(&mut sink, &frame).await {
                        return ConnEnd::Dropped(format!("couldn't send: {e}"));
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => {
                    return ConnEnd::Dropped("nothing from Bridge for a minute".into());
                }
                _ = ticker.tick() => {
                    let setup_incomplete = self.status.get().setup_needs_carbon();
                    let due = last_probe.elapsed() >= self.probe_interval || (setup_incomplete && last_probe.elapsed() >= Duration::from_secs(5));
                    if due && !probing {
                        probing = true;
                        last_probe = Instant::now();
                        let local = self.local.clone();
                        let ptx = probe_tx.clone();
                        tokio::spawn(async move {
                            if let Ok(p) = tokio::time::timeout(Duration::from_secs(90), local.probe()).await {
                                let _ = ptx.send(p);
                            }
                        });
                        self.spawn_hosted_probe(false);
                    }
                }
                probe = probe_rx.recv() => {
                    probing = false;
                    if let Some(p) = probe {
                        self.apply_probe_to_status(&p);
                        let hello = hello_from(&p);
                        if let Some(frame) = hello_update(&last_hello, &hello) {
                            last_hello = hello;
                            if send_frame(&mut sink, &frame).await.is_err() {
                                return ConnEnd::Dropped("couldn't send hello".into());
                            }
                        }
                    }
                }
                a = self.actions.recv() => {
                    let Some(a) = a else { return ConnEnd::Paired(PairedEnd::Shutdown) };
                    match a {
                        UiAction::Stop { target } => {
                            let target = target.and_then(|id| id.to_string().parse().ok());
                            let _ = tx.send(DeviceFrame::Stop { target });
                        }
                        UiAction::TakeoverDone { target, .. } => {
                            let target = target.and_then(|id| id.to_string().parse().ok());
                            let _ = tx.send(DeviceFrame::TakeoverDone { target });
                        }
                        UiAction::RevokePair => {
                            // Flush what's queued, then revoke over HTTP.
                            if let Some(end) = self.revoke(&stored.device_credential).await {
                                let _ = sink.send(Message::Close(None)).await;
                                return ConnEnd::Paired(end);
                            }
                        }
                        UiAction::Reconnect => {}
                        UiAction::Reprobe => { last_probe = Instant::now().checked_sub(self.probe_interval).unwrap_or_else(Instant::now); }
                    }
                }
                _ = self.shutdown.cancelled() => {
                    let _ = sink.send(Message::Close(None)).await;
                    return ConnEnd::Paired(PairedEnd::Shutdown);
                }
            }
        }
    }

    /// Handles one frame from the service. Returns how the pair ended, when it did.
    async fn handle_frame(&mut self, frame: ServiceFrame, tx: &mpsc::UnboundedSender<DeviceFrame>, stored: &StoredCredential) -> Option<PairedEnd> {
        match frame {
            ServiceFrame::Command(c) => {
                tracing::info!("command {} `{}` in session {}", c.id, c.command, c.session_id);
                self.dispatcher.submit(c);
            }
            ServiceFrame::Cancel { id } => self.dispatcher.cancel(id),
            ServiceFrame::SessionStarted { target, session_id, silicon_id, since } => {
                let info = InUseInfo { silicon_id: silicon_id.clone(), session_id: session_id.to_string(), since: fmt_time(since) };
                tracing::info!("{silicon_id} started session {session_id}");
                self.dispatcher.session_started(target.as_ref(), session_id.as_str());
                match target {
                    None => {
                        self.status.update(|s| {
                            s.in_use = Some(info);
                            s.takeover = None;
                        });
                    }
                    Some(id) => {
                        self.hosted.set_in_use(&id, Some(info));
                        self.sync_attached_status();
                    }
                }
            }
            ServiceFrame::SessionEnded { target, session_id, reason } => {
                tracing::info!("session {session_id} ended: {}", reason.as_str());
                self.dispatcher.session_closed(target.as_ref(), session_id.as_str());
                let sid = session_id.to_string();
                match target {
                    None => {
                        self.status.update(|s| {
                            if s.in_use.as_ref().is_some_and(|u| u.session_id == sid) {
                                s.in_use = None;
                                s.takeover = None;
                            }
                        });
                    }
                    Some(id) => {
                        self.hosted.set_in_use(&id, None);
                        self.sync_attached_status();
                    }
                }
            }
            ServiceFrame::Takeover { target, session_id, reason, expires_at } => {
                let info = TakeoverInfo { session_id: session_id.to_string(), reason, expires_at: fmt_time(expires_at) };
                match target {
                    None => self.status.update(|s| s.takeover = Some(info)),
                    Some(id) => {
                        self.hosted.set_takeover(&id, Some(info));
                        self.sync_attached_status();
                    }
                }
            }
            ServiceFrame::TakeoverEnded { target, .. } => match target {
                None => self.status.update(|s| s.takeover = None),
                Some(id) => {
                    self.hosted.set_takeover(&id, None);
                    self.sync_attached_status();
                }
            },
            ServiceFrame::Refresh => {
                if let Some(end) = self.refresh_device(&stored.device_credential).await {
                    return Some(end);
                }
            }
            ServiceFrame::Attach { device_id, os, name, address, removed } => {
                if removed {
                    tracing::info!("no longer carrying {name} ({device_id})");
                    if let Some(d) = self.hosted.remove(&device_id) {
                        tokio::spawn(async move { d.session_ended("").await });
                    }
                    self.sync_attached_status();
                } else {
                    tracing::info!("carrying {name} ({}, {device_id})", os.as_str());
                    self.hosted.attach(AttachRecord { device_id: device_id.clone(), os, name, address });
                    self.sync_attached_status();
                    self.spawn_hosted_probe(false);
                }
            }
            ServiceFrame::SetupCode { device_id, code } => {
                let hosted = self.hosted.clone();
                let status = self.status.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = match hosted.driver(&device_id) {
                        Ok(d) => d.setup_code(&code).await,
                        Err(e) => Err(e),
                    };
                    hosted.set_setup_error(&device_id, result.err());
                    for f in hosted.probe_changes(false).await {
                        let _ = tx.send(DeviceFrame::Attached(f));
                    }
                    status.update(|s| s.attached = hosted.infos());
                });
            }
            ServiceFrame::Environment { environment } => {
                let env = environment.as_ref().map(environment_info);
                self.status.update(|s| s.environment = env);
            }
            ServiceFrame::Unpaired { reason } => return Some(PairedEnd::Unpaired(reason.as_str().into())),
            ServiceFrame::Superseded => return Some(PairedEnd::Superseded),
            ServiceFrame::Ping { nonce } => {
                let _ = tx.send(DeviceFrame::Pong { nonce });
            }
        }
        None
    }

    fn sync_attached_status(&self) {
        let infos = self.hosted.infos();
        self.status.update(|s| s.attached = infos);
    }

    fn spawn_hosted_probe(&self, force: bool) {
        let hosted = self.hosted.clone();
        let outbox = self.outbox.clone();
        let status = self.status.clone();
        tokio::spawn(async move {
            for f in hosted.probe_changes(force).await {
                outbox.send(DeviceFrame::Attached(f));
            }
            status.update(|s| s.attached = hosted.infos());
        });
    }

    /// Re-reads name, owner and environment. Returns `Unpaired` if the credential is refused.
    async fn refresh_device(&mut self, credential: &str) -> Option<PairedEnd> {
        match tokio::time::timeout(Duration::from_secs(15), self.service.device_self(credential)).await {
            Err(_) => tracing::info!("GET /api/v1/device timed out"),
            Ok(Err(e)) if e.is_auth() => return Some(PairedEnd::Unpaired("credential refused".into())),
            Ok(Err(e)) => tracing::info!("couldn't read this device's details: {e}"),
            Ok(Ok(d)) => {
                let env = d.environment.as_ref().map(environment_info);
                self.status.update(|s| {
                    s.device = Some(DeviceInfo {
                        device_id: d.device_id.to_string(),
                        name: Some(d.name.clone()),
                        owner: Some(d.owner.id.clone()),
                        team: Some(d.team.clone()),
                    });
                    s.environment = env;
                    s.in_use = d.in_use.as_ref().map(|u| InUseInfo {
                        silicon_id: u.silicon_id.clone(),
                        session_id: u.session_id.to_string(),
                        since: fmt_time(u.since),
                    });
                    s.takeover = d.takeover.as_ref().map(|t| TakeoverInfo {
                        session_id: t.session_id.to_string(),
                        reason: t.reason.clone(),
                        expires_at: fmt_time(t.expires_at),
                    });
                });
            }
        }
        None
    }

    async fn probe_local(&self) -> Probe {
        match tokio::time::timeout(Duration::from_secs(90), self.local.probe()).await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("the device check took longer than 90 s");
                crate::drivers::local::with_terminal(Probe {
                    os: crate::sysinfo::device_os(),
                    os_version: crate::sysinfo::os_version(),
                    model: crate::sysinfo::model(),
                    capabilities: vec![],
                    missing: vec![],
                    setup: bridge_protocol::model::Setup::complete(),
                    agent_device_version: None,
                    online: true,
                })
            }
        }
    }

    fn apply_probe_to_status(&self, p: &Probe) {
        self.status.update(|s| {
            s.capabilities = p.capabilities.clone();
            s.missing = p.missing.clone();
            s.setup = Some(p.setup.clone());
        });
    }
}

pub fn hello_from(p: &Probe) -> Hello {
    Hello {
        app_version: APP_VERSION.into(),
        os: p.os,
        os_version: p.os_version.clone(),
        model: p.model.clone(),
        agent_device_version: p.agent_device_version.clone(),
        capabilities: p.capabilities.clone(),
        missing: p.missing.clone(),
        setup: p.setup.clone(),
    }
}

/// What to send when a re-probe differs from the last `hello`: a new `hello` when capabilities
/// changed, `setup_progress` when only the setup did, nothing otherwise.
pub fn hello_update(last: &Hello, now: &Hello) -> Option<DeviceFrame> {
    let setup_only = Hello { setup: last.setup.clone(), ..now.clone() } == *last;
    if now == last {
        None
    } else if setup_only {
        Some(DeviceFrame::SetupProgress { setup: now.setup.clone() })
    } else {
        Some(DeviceFrame::Hello(now.clone()))
    }
}

async fn send_frame<S>(sink: &mut S, frame: &DeviceFrame) -> Result<(), String>
where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let text = serde_json::to_string(frame).map_err(|e| e.to_string())?;
    sink.send(Message::Text(text.into())).await.map_err(|e| e.to_string())
}

fn environment_info(e: &TestingEnvironment) -> EnvironmentInfo {
    EnvironmentInfo { environment_id: e.environment_id.to_string(), name: e.name.clone(), state: e.state.clone() }
}

fn fmt_time(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Wait helper for callers outside the agent.
pub async fn sleep_unless(d: Duration, token: &CancellationToken) -> bool {
    sleep_or_shutdown(d, token).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_protocol::model::{Setup, SetupStep, StepStatus};
    use bridge_protocol::{Capability, DeviceOs};

    fn hello(caps: Vec<Capability>, setup: Setup) -> Hello {
        Hello {
            app_version: "1".into(),
            os: DeviceOs::Macos,
            os_version: None,
            model: None,
            agent_device_version: None,
            capabilities: caps,
            missing: vec![],
            setup,
        }
    }

    #[test]
    fn hello_updates() {
        let a = hello(vec![Capability::Terminal], Setup::complete());
        assert!(hello_update(&a, &a).is_none());
        let pending = Setup::from_steps(vec![SetupStep {
            key: "accessibility".into(),
            title: "x".into(),
            status: StepStatus::NeedsCarbon,
            help: None,
            error: None,
            input: None,
        }]);
        let b = hello(vec![Capability::Terminal], pending.clone());
        assert!(matches!(hello_update(&a, &b), Some(DeviceFrame::SetupProgress { .. })));
        let c = hello(vec![Capability::Terminal, Capability::ScreenRead], pending);
        assert!(matches!(hello_update(&b, &c), Some(DeviceFrame::Hello(_))));
    }
}
