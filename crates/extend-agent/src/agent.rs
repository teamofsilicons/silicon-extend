//! The agent: pairs the computer (with one Carbon, or several through "Pair with another Carbon"),
//! keeps one device socket per pair, runs commands, and keeps the status the tray, window and
//! `status` command show (`docs/device-protocol.md`).
//!
//! Each pair is a normal device connection of its own, authenticated with that pair's credential
//! ([`Link`]). They share one command dispatcher, one registry of carried devices, one screen
//! watch and one probe of this computer. What arrives on a pair's socket is answered on that
//! socket (results, `attached`, `wake_request_shown`, `credential_saved`), and its files are
//! uploaded with that pair's credential. Stop and Done may go on any of them: the service applies
//! them to the whole physical device. Ending one pair (unpaired, revoked here) never touches the
//! others.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use extend_driver::{Driver, Probe};
use extend_protocol::frames::{CommandOutcome, DeviceFrame, Hello, ServiceFrame, close};
use extend_protocol::model::{CommandError, EnrollmentCreate, MissingCapability, TestingEnvironment};
use extend_protocol::{Capability, DeviceId};
use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::{Notify, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::awake::{AwakeNow, AwakeReporter, AwakeTracker};
use crate::config::{APP_VERSION, Config};
use crate::credential::{CredentialStore, StoredCredential, load_for};
use crate::dispatch::{Dispatcher, DriverLookup, Outbox, Reply, Uploader};
use crate::display::DisplayKeeper;
use crate::drivers::screen_lock::{ScreenBlock, ScreenReading};
use crate::enroll::{self, EnrollOutcome, Purpose, Start, sleep_or_shutdown};
use crate::hosted::{AttachRecord, DriverFactory, HostedRegistry, WAKE_PROBE_EVERY};
use crate::notify::{Notifier, WakeBook, WakeEntry};
use crate::service::{ServiceClient, device_auth};
use crate::status::{
    AddingPair, AgentStatus, EnvironmentInfo, InUseInfo, PairInfo, PairPhase, Phase, StatusHandle, TakeoverInfo,
    WakeInfo,
};
use crate::ws::{self, Backoff, ConnectError, Socket};

/// Something the Carbon did in the tray or window (or `extend-agent` did for them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    /// Stop the Silicon using this computer (or, with a target, a device it carries).
    Stop { target: Option<DeviceId> },
    /// Done on a takeover.
    TakeoverDone { target: Option<DeviceId> },
    /// Revoke one Carbon's pair. The UI has already confirmed with the Carbon.
    RevokePair { device_id: DeviceId },
    /// Try connecting a pair again now (after it was taken over, or to skip a backoff wait);
    /// `None`: every pair.
    Reconnect { device_id: Option<DeviceId> },
    /// "Pair with another Carbon", after the UI showed the shared-computer warning.
    PairAnother,
    /// Take that code down.
    CancelPairAnother,
    /// Check permissions and helpers again now.
    Reprobe,
}

/// Reads whether this computer's screen can be used right now, and how long since the last input
/// ([`crate::drivers::screen_lock::read`]).
pub type ScreenWatch = Arc<dyn Fn() -> ScreenReading + Send + Sync>;

/// How often the agent asks [`ScreenWatch`] whether the screen was locked or unlocked.
pub const SCREEN_WATCH_EVERY: Duration = Duration::from_secs(3);

/// How long the agent gives its cleanups when it stops (closing the sessions still open, an
/// iPhone's included). The UI waits a little longer than this before the app exits.
pub const QUIT_CLEANUP_LIMIT: Duration = Duration::from_secs(10);

pub struct AgentDeps {
    pub config: Config,
    pub local: Arc<dyn Driver>,
    pub hosted_factory: DriverFactory,
    pub credentials: Arc<dyn CredentialStore>,
    /// How often capabilities are re-checked (30 s; faster while setup is incomplete).
    pub probe_interval: Duration,
    /// Checked every [`SCREEN_WATCH_EVERY`]: when the screen is locked, unlocked, falls asleep or
    /// wakes, this computer is checked again at once and `awake` goes to Extend, so it hears
    /// within seconds what a Silicon can do now. `None`: a computer that is always awake
    /// (headless), checked only on the probe interval.
    pub screen_watch: Option<ScreenWatch>,
    /// Shows Silicons' requests to wake this computer ([`crate::notify::for_this_computer`]).
    pub notifier: Arc<dyn Notifier>,
    /// Keeps the display on while a Silicon uses this computer
    /// ([`crate::display::for_this_computer`]).
    pub display: Arc<dyn DisplayKeeper>,
}

/// What the UI keeps to talk to the agent.
#[derive(Clone)]
pub struct AgentHandle {
    pub status: StatusHandle,
    pub actions: mpsc::UnboundedSender<UiAction>,
    pub shutdown: CancellationToken,
}

pub struct Agent {
    core: Arc<Core>,
    actions: mpsc::UnboundedReceiver<UiAction>,
}

/// What every pair's connection shares.
struct Core {
    config: Config,
    service: ServiceClient,
    status: StatusHandle,
    local: Arc<dyn Driver>,
    hosted: Arc<HostedRegistry>,
    credentials: Arc<dyn CredentialStore>,
    dispatcher: Dispatcher,
    shutdown: CancellationToken,
    probe_interval: Duration,
    /// Check this computer again now (a session's setup or cleanup ran, the screen changed).
    reprobe: Arc<Notify>,
    /// A connection wants a probe to say hello with.
    fresh: Notify,
    /// The latest probe of this computer.
    probes: watch::Sender<Option<Arc<Probe>>>,
    screen_watch: Option<ScreenWatch>,
    awake: AwakeReporter,
    wakes: Mutex<WakeBook>,
    notifier: Arc<dyn Notifier>,
    display: Arc<dyn DisplayKeeper>,
    /// One per pair, by the pair's device id.
    links: Mutex<BTreeMap<DeviceId, Arc<Link>>>,
    /// Bumped when pairs come or go or learn whether they are the first: hellos are recomputed
    /// (the terminal rule for computers several Carbons paired depends on it).
    pairs_changed: watch::Sender<u64>,
    /// A carried-device probe is running.
    hosted_probing: std::sync::atomic::AtomicBool,
    /// The instance each pair's `GET /api/v1/device` named, to notice credentials of two devices.
    instances: Mutex<BTreeMap<DeviceId, Uuid>>,
    /// A wake notification is on screen (so an empty list has something to take down).
    notified: std::sync::atomic::AtomicBool,
}

/// One Carbon's pair of this computer.
struct Link {
    id: DeviceId,
    cred: Arc<RwLock<StoredCredential>>,
    outbox: Outbox,
    ctl: mpsc::UnboundedSender<LinkCtl>,
    cancel: CancellationToken,
    uploader: Arc<dyn Uploader>,
}

impl Link {
    fn credential(&self) -> String {
        self.cred.read().unwrap().device_credential.clone()
    }
    fn reply(&self) -> Reply {
        Reply {
            pair: Some(self.id.clone()),
            outbox: self.outbox.clone(),
            uploader: self.uploader.clone(),
        }
    }
}

enum LinkCtl {
    Reconnect,
}

enum Event {
    /// The service ended a pair (unpaired, or its credential was refused).
    PairEnded {
        id: DeviceId,
        reason: String,
    },
    Enrolled {
        purpose: Purpose,
        outcome: EnrollOutcome,
    },
}

/// What the service's greeting of a connection said about the devices this computer carries.
#[derive(Default)]
struct Greeting {
    /// Devices it attached again.
    attached: HashSet<DeviceId>,
    /// Devices it announced a live session for.
    announced: HashSet<DeviceId>,
}

/// How a pair's connection ended for good (or until the Carbon reconnects it).
#[derive(Debug, PartialEq)]
enum PairedEnd {
    Unpaired(String),
    Superseded,
    UpgradeRequired,
    /// This app closed it (the pair was revoked here).
    Closed,
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

/// Uploads with one pair's credential (the one its command came through).
struct ServiceUploader {
    service: ServiceClient,
    credential: Arc<RwLock<StoredCredential>>,
}

#[async_trait]
impl Uploader for ServiceUploader {
    async fn upload(&self, upload_id: Uuid, path: &Path, name: &str, content_type: &str) -> Result<u64, String> {
        let cred = self.credential.read().unwrap().device_credential.clone();
        self.service
            .upload_artifact(&cred, upload_id, path, name, content_type)
            .await
            .map(|(_, size)| size)
            .map_err(|e| e.message)
    }
}

/// Uploads nothing: the dispatcher's default route, never used by the agent (every command is
/// answered through its pair).
struct NoUploads;

#[async_trait]
impl Uploader for NoUploads {
    async fn upload(&self, _: Uuid, _: &Path, _: &str, _: &str) -> Result<u64, String> {
        Err("this command came through no pair".into())
    }
}

impl Agent {
    pub fn new(deps: AgentDeps) -> (Self, AgentHandle) {
        let AgentDeps {
            config,
            local,
            hosted_factory,
            credentials,
            probe_interval,
            screen_watch,
            notifier,
            display,
        } = deps;
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
        let dispatcher = Dispatcher::new(
            Arc::new(Lookup {
                local: local.clone(),
                hosted: hosted.clone(),
            }),
            Arc::new(NoUploads),
            Outbox::default(),
            config.work_dir(),
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let handle = AgentHandle {
            status: status.clone(),
            actions: tx,
            shutdown: shutdown.clone(),
        };
        let core = Arc::new(Core {
            config,
            service,
            status,
            local,
            hosted,
            credentials,
            dispatcher,
            shutdown,
            probe_interval,
            reprobe: Arc::new(Notify::new()),
            fresh: Notify::new(),
            probes: watch::channel(None).0,
            screen_watch,
            awake: AwakeReporter::new(),
            wakes: Mutex::new(WakeBook::default()),
            notifier,
            display,
            links: Mutex::new(BTreeMap::new()),
            pairs_changed: watch::channel(0).0,
            hosted_probing: std::sync::atomic::AtomicBool::new(false),
            instances: Mutex::new(BTreeMap::new()),
            notified: std::sync::atomic::AtomicBool::new(false),
        });
        (Self { core, actions: rx }, handle)
    }

    /// Runs until shutdown.
    pub async fn run(self) {
        let Agent { core, mut actions } = self;
        let _ = tokio::fs::remove_dir_all(core.config.work_dir()).await;
        tokio::spawn(crate::status::persist(core.status.clone(), core.config.status_path()));
        core.hosted.restore();
        tokio::spawn(prober(core.clone()));
        tokio::spawn(watch_screen(core.clone()));
        tokio::spawn(wake_janitor(core.clone()));
        core.sync_attached_status();

        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<Event>();
        let stored = match load_for(core.credentials.as_ref(), &core.config.service_url) {
            Ok(list) => list,
            Err(e) => {
                tracing::error!("couldn't read the device credentials: {e:#}");
                core.status
                    .update(|s| s.last_error = Some(format!("Couldn't read the device credentials: {e:#}")));
                vec![]
            }
        };
        for c in stored {
            core.start_link(c, &events_tx);
        }

        let mut first: Option<CancellationToken> = None;
        let mut another: Option<CancellationToken> = None;
        // After a first pairing refused for an old app: when to try again.
        let mut retry_first_at: Option<tokio::time::Instant> = None;
        loop {
            if core.links.lock().unwrap().is_empty() && first.is_none() && retry_first_at.is_none() {
                first = Some(core.start_enrollment(None, &events_tx));
            }
            let retry_first = async {
                match retry_first_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = core.shutdown.cancelled() => break,
                _ = retry_first => retry_first_at = None,
                a = actions.recv() => {
                    let Some(a) = a else { break };
                    if matches!(a, UiAction::Reconnect { device_id: None }) {
                        retry_first_at = None;
                    }
                    core.on_action(a, &mut another, &events_tx).await;
                }
                e = events_rx.recv() => {
                    let Some(e) = e else { break };
                    match e {
                        Event::PairEnded { id, reason } => {
                            tracing::info!("the pair {id} ended ({reason})");
                            core.forget_pair(&id).await;
                        }
                        Event::Enrolled { purpose: Purpose::First, outcome } => {
                            first = None;
                            match outcome {
                                EnrollOutcome::Paired(p) => core.paired(p, true, &events_tx),
                                EnrollOutcome::UpgradeRequired => {
                                    core.status.update(|s| s.phase = Phase::UpgradeRequired);
                                    retry_first_at = Some(tokio::time::Instant::now() + Duration::from_secs(3600));
                                }
                                EnrollOutcome::Refused(why) => {
                                    core.status.update(|s| s.last_error = Some(why));
                                    retry_first_at = Some(tokio::time::Instant::now() + Duration::from_secs(60));
                                }
                                EnrollOutcome::Shutdown => {}
                            }
                        }
                        Event::Enrolled { purpose: Purpose::AnotherCarbon, outcome } => {
                            another = None;
                            match outcome {
                                EnrollOutcome::Paired(p) => {
                                    core.status.update(|s| s.adding_pair = None);
                                    core.paired(p, false, &events_tx);
                                }
                                EnrollOutcome::Refused(why) => core.adding_failed(why),
                                EnrollOutcome::UpgradeRequired => core.adding_failed(
                                    "Update Silicon Extend to pair this computer with another Carbon.".into(),
                                ),
                                EnrollOutcome::Shutdown => core.status.update(|s| s.adding_pair = None),
                            }
                        }
                    }
                }
            }
        }
        // Leave the computer tidy: stop what is still running, then close the device engine's
        // sessions that were open (this computer's and those of the devices it carries: an
        // iPhone shows "Automation Running" until its session is closed), each after its device's
        // running command has wound down.
        for link in core.links() {
            link.cancel.cancel();
        }
        let known = core.sessions_in_use();
        let cleanups = core.dispatcher.close_all(&known);
        let _ = tokio::time::timeout(QUIT_CLEANUP_LIMIT, futures::future::join_all(cleanups)).await;
        core.display.hold(false);
        let notifier = core.notifier.clone();
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || notifier.clear()),
        )
        .await;
    }
}

impl Core {
    // ───── Pairs ─────

    /// Starts `stored`'s connection.
    fn start_link(self: &Arc<Self>, stored: StoredCredential, events: &mpsc::UnboundedSender<Event>) {
        let id = stored.device_id.clone();
        let cred = Arc::new(RwLock::new(stored.clone()));
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let link = Arc::new(Link {
            id: id.clone(),
            cred: cred.clone(),
            outbox: Outbox::default(),
            ctl: ctl_tx,
            cancel: self.shutdown.child_token(),
            uploader: Arc::new(ServiceUploader {
                service: self.service.clone(),
                credential: cred,
            }),
        });
        {
            let mut links = self.links.lock().unwrap();
            if let Some(old) = links.insert(id.clone(), link.clone()) {
                old.cancel.cancel();
            }
        }
        self.status.update(|s| {
            if !s.pairs.iter().any(|p| p.device_id == id.as_str()) {
                s.pairs.push(PairInfo {
                    device_id: id.to_string(),
                    first_pair: stored.first_pair,
                    ..Default::default()
                });
            }
            s.pairing = None;
            if s.phase == Phase::Enrolling || s.phase == Phase::Starting {
                s.phase = Phase::Reconnecting;
            }
        });
        self.pairs_changed();
        tokio::spawn(run_link(self.clone(), link, ctl_rx, events.clone()));
    }

    /// A code was claimed: keep the new pair's credential and connect it.
    fn paired(self: &Arc<Self>, p: enroll::Paired, first: bool, events: &mpsc::UnboundedSender<Event>) {
        let mut stored = StoredCredential::new(p.device_id.clone(), p.device_credential, &self.config.service_url);
        stored.first_pair = Some(first);
        if let Err(e) = self.credentials.save(&stored) {
            tracing::error!("couldn't store the device credential: {e:#}");
            self.status
                .update(|s| s.last_error = Some(format!("Couldn't store the device credential: {e:#}")));
            // Keep going in memory; the pair works until the app restarts.
        }
        tracing::info!("paired as device {}", p.device_id);
        let env = p.environment.as_ref().map(environment_info);
        self.status.update(|s| {
            s.pairing = None;
            if first {
                s.environment = env;
            }
        });
        self.start_link(stored, events);
    }

    fn link(&self, id: &DeviceId) -> Option<Arc<Link>> {
        self.links.lock().unwrap().get(id).cloned()
    }

    fn links(&self) -> Vec<Arc<Link>> {
        self.links.lock().unwrap().values().cloned().collect()
    }

    /// A connected pair, preferring `prefer`.
    fn online_link(&self, prefer: Option<&DeviceId>) -> Option<Arc<Link>> {
        let s = self.status.get();
        let online = |id: &DeviceId| s.pair(id.as_str()).is_some_and(|p| p.phase == PairPhase::Online);
        if let Some(id) = prefer
            && online(id)
            && let Some(l) = self.link(id)
        {
            return Some(l);
        }
        self.links().into_iter().find(|l| online(&l.id))
    }

    fn pairs_changed(&self) {
        let withheld: Vec<(String, bool)> = self
            .links()
            .iter()
            .map(|l| (l.id.to_string(), self.terminal_withheld(&l.id)))
            .collect();
        self.status.update(|s| {
            for p in &mut s.pairs {
                if let Some((_, w)) = withheld.iter().find(|(id, _)| *id == p.device_id) {
                    p.terminal_withheld = *w;
                }
            }
        });
        self.pairs_changed.send_modify(|n| *n += 1);
    }

    /// Carbon decision (2026-09-27): the terminal runs as this computer's own account, so on a
    /// computer several Carbons paired only Silicons given access through the pair made by the
    /// app's first enrollment (the Carbon who installed Silicon Extend here) get it. The service
    /// applies the same rule; the agent also refuses a terminal command that arrives through
    /// another pair.
    fn terminal_withheld(&self, id: &DeviceId) -> bool {
        let links = self.links.lock().unwrap();
        links.len() > 1
            && links
                .get(id)
                .is_some_and(|l| l.cred.read().unwrap().first_pair != Some(true))
    }

    fn hello_for(&self, id: &DeviceId, p: &Probe) -> Hello {
        let mut h = hello_from(p);
        if self.terminal_withheld(id) {
            withhold_terminal(&mut h);
        }
        h
    }

    fn set_pair_phase(&self, id: &DeviceId, phase: PairPhase) {
        self.status.update(|s| {
            if let Some(p) = s.pairs.iter_mut().find(|p| p.device_id == id.as_str()) {
                p.phase = phase;
            }
            s.phase = overall_phase(&s.pairs, s.phase);
        });
    }

    /// Forgets one pair: its credential, its connection, and everything that ran through it
    /// (this computer's session, if it ran through it, and the devices carried for it). The
    /// other pairs go on; after the last one, the app shows a pairing code again.
    async fn forget_pair(self: &Arc<Self>, id: &DeviceId) {
        // Already forgotten (revoked here, then its `unpaired` arrived too): nothing more to end.
        if self.link(id).is_none() && self.status.get().pair(id.as_str()).is_none() {
            return;
        }
        if let Err(e) = self.credentials.remove(id) {
            tracing::error!("couldn't remove the device credential of {id}: {e:#}");
        }
        let link = self.links.lock().unwrap().remove(id);
        if let Some(l) = &link {
            l.cancel.cancel();
            l.outbox.clear();
        }
        let none_left = self.links.lock().unwrap().is_empty();
        let s = self.status.get();
        // Every Silicon's access through this pair ends now: running and queued commands are
        // cancelled, and each session is cleaned up on its device's queue after its command has
        // wound down (the service's own `session_ended` frames may never be read on this path).
        let mut known: Vec<(Option<DeviceId>, String)> = Vec::new();
        let own_through_it = s
            .in_use
            .as_ref()
            .is_some_and(|u| u.pair.as_deref() == Some(id.as_str()) || none_left);
        if own_through_it && let Some(u) = &s.in_use {
            known.push((None, u.session_id.clone()));
        }
        let carried: Vec<DeviceId> = self
            .hosted
            .ids()
            .into_iter()
            .filter(|c| match self.hosted.host_of(c) {
                Some(h) => &h == id,
                None => none_left,
            })
            .collect();
        for c in &carried {
            if let Some(u) = self.hosted.in_use(c) {
                known.push((Some(c.clone()), u.session_id));
            }
        }
        let _ = self.dispatcher.close_pair(id, &known);
        for c in &carried {
            self.hosted.remove(c);
        }
        self.instances.lock().unwrap().remove(id);
        self.wakes.lock().unwrap().remove_pair(id);
        self.status.update(|s| {
            s.pairs.retain(|p| p.device_id != id.as_str());
            if own_through_it {
                s.in_use = None;
                s.takeover = None;
            }
            if none_left {
                s.phase = Phase::Enrolling;
                s.pairing = None;
                s.environment = None;
                s.adding_pair = None;
            } else {
                s.phase = overall_phase(&s.pairs, s.phase);
            }
        });
        self.sync_attached_status();
        self.pairs_changed();
        self.update_display();
        let _ = self.render_wakes(false).await;
    }

    fn adding_failed(&self, why: String) {
        self.status.update(|s| {
            s.adding_pair = Some(AddingPair {
                pairing: None,
                error: Some(why),
            })
        });
    }

    /// Starts a first pairing (`credential` none) or "Pair with another Carbon".
    fn start_enrollment(
        self: &Arc<Self>,
        credential: Option<String>,
        events: &mpsc::UnboundedSender<Event>,
    ) -> CancellationToken {
        let token = self.shutdown.child_token();
        let core = self.clone();
        let events = events.clone();
        let stop = token.clone();
        tokio::spawn(async move {
            let (purpose, outcome) = match credential {
                None => {
                    let probe = core.latest_probe().await;
                    let info = EnrollmentCreate {
                        os: crate::sysinfo::device_os(),
                        os_version: probe.os_version.clone(),
                        model: probe.model.clone(),
                        app_version: APP_VERSION.into(),
                        engine_version: probe.engine_version.clone(),
                    };
                    let o = enroll::enroll(&core.service, &core.status, Start::First(&info), &stop).await;
                    (Purpose::First, o)
                }
                Some(credential) => {
                    let o = enroll::enroll(
                        &core.service,
                        &core.status,
                        Start::AnotherCarbon {
                            credential: &credential,
                        },
                        &stop,
                    )
                    .await;
                    (Purpose::AnotherCarbon, o)
                }
            };
            let _ = events.send(Event::Enrolled { purpose, outcome });
        });
        token
    }

    // ───── What the Carbon does ─────

    async fn on_action(
        self: &Arc<Self>,
        a: UiAction,
        another: &mut Option<CancellationToken>,
        events: &mpsc::UnboundedSender<Event>,
    ) {
        match a {
            UiAction::Stop { target: None } => {
                // The Carbon wants it to stop: the display may turn off again at once.
                self.display.hold(false);
                let own = self
                    .status
                    .get()
                    .in_use
                    .and_then(|u| u.pair)
                    .and_then(|p| p.parse().ok());
                match self.online_link(own.as_ref()) {
                    Some(l) if l.outbox.send(DeviceFrame::Stop { target: None }) => {}
                    _ => {
                        // No live connection: Stop over HTTP with any pair's credential.
                        let Some(l) = self.links().into_iter().next() else {
                            return;
                        };
                        if let Err(e) = self.service.stop(&l.credential()).await {
                            self.status
                                .update(|s| s.last_error = Some(format!("Couldn't stop: {}", e.message)));
                        }
                    }
                }
            }
            UiAction::Stop { target: Some(id) } => {
                let host = self.hosted.host_of(&id);
                let sent = self
                    .online_link(host.as_ref())
                    .filter(|l| host.is_none() || host.as_ref() == Some(&l.id))
                    .is_some_and(|l| l.outbox.send(DeviceFrame::Stop { target: Some(id) }));
                if !sent {
                    self.status.update(|s| {
                        s.last_error = Some(
                            "Couldn't stop: this computer isn't connected to Extend right now. Try again in a moment."
                                .into(),
                        )
                    });
                }
            }
            UiAction::TakeoverDone { target } => {
                let host = target.as_ref().and_then(|t| self.hosted.host_of(t));
                let own = self
                    .status
                    .get()
                    .in_use
                    .and_then(|u| u.pair)
                    .and_then(|p| p.parse().ok());
                let prefer = host.or(own);
                if let Some(l) = self.online_link(prefer.as_ref()) {
                    l.outbox.send(DeviceFrame::TakeoverDone { target });
                }
            }
            UiAction::RevokePair { device_id } => {
                let Some(link) = self.link(&device_id) else { return };
                match self.service.revoke_pair(&link.credential()).await {
                    Ok(()) => self.forget_pair(&device_id).await,
                    Err(e) if e.is_auth() => self.forget_pair(&device_id).await,
                    Err(e) => self
                        .status
                        .update(|s| s.last_error = Some(format!("Couldn't revoke the pair: {}", e.message))),
                }
            }
            UiAction::Reconnect { device_id } => {
                for l in self.links() {
                    if device_id.as_ref().is_none_or(|id| *id == l.id) {
                        let _ = l.ctl.send(LinkCtl::Reconnect);
                    }
                }
            }
            UiAction::PairAnother => {
                if another.as_ref().is_some_and(|t| !t.is_cancelled()) {
                    return;
                }
                let Some(link) = self.online_link(None).or_else(|| self.links().into_iter().next()) else {
                    return;
                };
                self.status.update(|s| s.adding_pair = Some(AddingPair::default()));
                *another = Some(self.start_enrollment(Some(link.credential()), events));
            }
            UiAction::CancelPairAnother => {
                if let Some(t) = another.take() {
                    t.cancel();
                }
                self.status.update(|s| s.adding_pair = None);
            }
            UiAction::Reprobe => self.reprobe.notify_one(),
        }
    }

    // ───── Probing ─────

    async fn probe_local(&self) -> Probe {
        match tokio::time::timeout(Duration::from_secs(90), self.local.probe()).await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("the device check took longer than 90 s");
                fallback_probe()
            }
        }
    }

    /// A probe taken after now (at most one probe's time away).
    async fn fresh_probe(&self) -> Arc<Probe> {
        let mut rx = self.probes.subscribe();
        rx.borrow_and_update();
        self.fresh.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(95), rx.changed()).await;
        let latest = rx.borrow().clone();
        latest.unwrap_or_else(|| Arc::new(fallback_probe()))
    }

    /// The latest probe, or a fresh one when there is none yet.
    async fn latest_probe(&self) -> Arc<Probe> {
        let latest = self.probes.borrow().clone();
        match latest {
            Some(p) => p,
            None => self.fresh_probe().await,
        }
    }

    fn apply_probe_to_status(&self, p: &Probe) {
        self.status.update(|s| {
            s.capabilities = p.capabilities.clone();
            s.missing = p.missing.clone();
            s.setup = Some(p.setup.clone());
        });
    }

    fn sync_attached_status(&self) {
        let infos = self.hosted.infos();
        self.status.update(|s| s.attached = infos);
    }

    /// Sends a carried device's `attached` on the connection of the pair it is carried for.
    fn send_attached(&self, host: Option<DeviceId>, status: extend_protocol::frames::AttachedStatus) {
        let link = match &host {
            Some(h) => self.link(h),
            None => self.links().into_iter().next(),
        };
        if let Some(l) = link {
            l.outbox.send(DeviceFrame::Attached(status));
        }
    }

    /// Checks the carried devices (all, or those carried for `host`) and reports what changed.
    fn spawn_hosted_probe(self: &Arc<Self>, host: Option<DeviceId>, force: bool) {
        use std::sync::atomic::Ordering;
        if host.is_none() && self.hosted_probing.swap(true, Ordering::SeqCst) {
            return;
        }
        let core = self.clone();
        tokio::spawn(async move {
            let sent = match &host {
                Some(h) => core.hosted.probe_host(h, force).await,
                None => core.hosted.probe_changes(force).await,
            };
            for (h, f) in sent {
                core.send_attached(h, f);
            }
            core.sync_attached_status();
            if host.is_none() {
                core.hosted_probing.store(false, Ordering::SeqCst);
            }
        });
    }

    /// Checks the carried devices a Silicon asked to wake.
    fn spawn_waking_probe(self: &Arc<Self>) {
        let waking = self.hosted.waking(time::OffsetDateTime::now_utc());
        if waking.is_empty() {
            return;
        }
        let core = self.clone();
        tokio::spawn(async move {
            for (h, f) in core.hosted.probe_these(&waking, false).await {
                core.send_attached(h, f);
            }
            core.sync_attached_status();
        });
    }

    /// Checks this computer again once a session's setup or cleanup has run. Releasing this
    /// computer from an ended session (or failing to) changes what it can do, and the service
    /// should know before the Silicon's next command, not at the next periodic check.
    fn reprobe_after(&self, done: Option<tokio::sync::oneshot::Receiver<()>>) {
        let Some(done) = done else { return };
        let reprobe = self.reprobe.clone();
        tokio::spawn(async move {
            if done.await.is_ok() {
                reprobe.notify_one();
            }
        });
    }

    // ───── Sessions, the display and wake requests ─────

    /// The sessions the service last said are live: this computer's and each carried device's.
    fn sessions_in_use(&self) -> Vec<(Option<DeviceId>, String)> {
        let mut known: Vec<(Option<DeviceId>, String)> = self
            .status
            .get()
            .in_use
            .map(|u| (None, u.session_id))
            .into_iter()
            .collect();
        for a in self.hosted.infos() {
            if let (Some(u), Ok(id)) = (a.in_use, a.device_id.parse::<DeviceId>()) {
                known.push((Some(id), u.session_id));
            }
        }
        known
    }

    /// The side of the session running anywhere on this computer or the devices it carries.
    fn active_side(&self) -> Option<String> {
        self.status
            .get()
            .in_use
            .and_then(|u| u.side)
            .or_else(|| self.hosted.active_side())
    }

    /// Holds the display on while this computer's own session runs, no takeover is paused on the
    /// Carbon, and it is awake.
    fn update_display(&self) {
        let s = self.status.get();
        let want = crate::display::wanted(s.in_use.is_some(), s.takeover.is_some(), self.awake.now().awake);
        self.display.hold(want);
    }

    /// Puts the wake requests on screen as they may be shown now (another side's redacted while
    /// a session runs), in the window and in the system notification. Returns whether the
    /// notification could be shown, and why not.
    async fn render_wakes(&self, alert: bool) -> Result<(), String> {
        let side = self.active_side();
        let shown = self.wakes.lock().unwrap().visible(side.as_deref());
        let owners: Vec<(String, Option<String>)> = self
            .status
            .get()
            .pairs
            .iter()
            .map(|p| (p.device_id.clone(), p.owner.clone()))
            .collect();
        let infos: Vec<WakeInfo> = shown
            .iter()
            .map(|w| WakeInfo {
                wake_id: w.wake_id.to_string(),
                pair: w.pair.to_string(),
                carbon: owners
                    .iter()
                    .find(|(id, _)| *id == w.pair.as_str())
                    .and_then(|(_, o)| o.clone()),
                silicon_id: w.silicon_id.clone(),
                reason: w.reason.clone(),
                expires_at: fmt_time(w.expires_at),
            })
            .collect();
        self.status.update(|s| s.wake_requests = infos);
        let n = crate::notify::notification(&shown, crate::sysinfo::computer_word(), alert);
        // Nothing to show and nothing shown: the notification service isn't bothered (this runs
        // at every session start, before its first command).
        let was_shown = self.notified.swap(n.is_some(), std::sync::atomic::Ordering::SeqCst);
        if n.is_none() && !was_shown {
            return Ok(());
        }
        let notifier = self.notifier.clone();
        let shown_now = tokio::task::spawn_blocking(move || match n {
            Some(n) => notifier.show(&n),
            None => {
                notifier.clear();
                Ok(())
            }
        });
        match tokio::time::timeout(Duration::from_secs(5), shown_now).await {
            Ok(Ok(r)) => r,
            _ => Err("The notification didn't appear in time.".into()),
        }
    }

    // ───── One pair's frames ─────

    /// Re-reads name, owner, environment, and whether the pair is the first. Returns `Unpaired`
    /// if the credential is refused.
    async fn refresh_device(self: &Arc<Self>, link: &Link) -> Option<PairedEnd> {
        match tokio::time::timeout(Duration::from_secs(15), self.service.device_self(&link.credential())).await {
            Err(_) => tracing::info!("GET /api/v1/device timed out"),
            Ok(Err(e)) if e.is_auth() => return Some(PairedEnd::Unpaired("credential refused".into())),
            Ok(Err(e)) => tracing::info!("couldn't read this device's details: {e}"),
            Ok(Ok(d)) => {
                if let Some(salt) = &d.hardware_salt {
                    self.hosted.set_salt(Some(salt.clone()));
                }
                if let Some(instance) = d.instance_id {
                    let mut instances = self.instances.lock().unwrap();
                    instances.insert(link.id.clone(), instance);
                    let distinct: HashSet<&Uuid> = instances.values().collect();
                    if distinct.len() > 1 {
                        // A restored backup, say: each pair keeps working on its own.
                        tracing::warn!("this app holds credentials of {} different devices", distinct.len());
                    }
                }
                if let Some(first) = d.first_pair {
                    let changed = {
                        let mut c = link.cred.write().unwrap();
                        let changed = c.first_pair != Some(first);
                        c.first_pair = Some(first);
                        changed.then(|| c.clone())
                    };
                    if let Some(c) = changed {
                        if let Err(e) = self.credentials.save(&c) {
                            tracing::warn!("couldn't note which pair is the first: {e:#}");
                        }
                        self.pairs_changed();
                    }
                }
                let env = d.environment.as_ref().map(environment_info);
                let id = link.id.to_string();
                self.status.update(|s| {
                    if let Some(p) = s.pairs.iter_mut().find(|p| p.device_id == id) {
                        p.name = Some(d.name.clone());
                        p.owner = Some(d.owner.id.clone());
                        p.team = Some(d.team.clone());
                        p.first_pair = d.first_pair.or(p.first_pair);
                    }
                    s.environment = env;
                    // The service names the session only on the pair it runs through.
                    match &d.in_use {
                        Some(u) => {
                            let side = s
                                .in_use
                                .as_ref()
                                .filter(|old| old.session_id == u.session_id.as_str())
                                .and_then(|old| old.side.clone());
                            s.in_use = Some(InUseInfo {
                                silicon_id: u.silicon_id.clone(),
                                session_id: u.session_id.to_string(),
                                since: fmt_time(u.since),
                                pair: Some(id.clone()),
                                carbon: Some(d.owner.id.clone()),
                                side,
                            });
                            s.takeover = d.takeover.as_ref().map(|t| TakeoverInfo {
                                session_id: t.session_id.to_string(),
                                reason: t.reason.clone(),
                                expires_at: fmt_time(t.expires_at),
                            });
                        }
                        None => {
                            if s.in_use
                                .as_ref()
                                .is_some_and(|u| u.pair.as_deref() == Some(id.as_str()))
                            {
                                s.in_use = None;
                                s.takeover = None;
                            }
                        }
                    }
                });
                self.update_display();
            }
        }
        None
    }

    /// The service greets every connection with the devices carried for that pair (an `attach`
    /// each) and each one's live session (a `session_started` right after its `attach`). A device
    /// the greeting attached again without a session has none: a session this computer still
    /// counts was ended while it was away (its `session_ended` never came), and the device's
    /// driver is told to end whatever it still has open. A carried device the greeting didn't
    /// attach again says nothing (the service may have failed to read its devices), so it is left
    /// as it is. Only the devices carried for `host` are looked at: another pair reconnecting
    /// never ends a session that runs through this one.
    fn end_sessions_not_announced(&self, host: &DeviceId, greeting: &Greeting) {
        for id in self.hosted.ids_for_host(host) {
            if !greeting.attached.contains(&id) || greeting.announced.contains(&id) {
                continue;
            }
            if let Some(u) = self.hosted.in_use(&id) {
                tracing::info!("session {} on {id} ended while this computer was away", u.session_id);
                let _ = self.dispatcher.session_closed(Some(&id), &u.session_id);
                self.hosted.set_in_use(&id, None);
            }
            if self.hosted.driver(&id).is_ok() {
                let _ = self.dispatcher.no_session_on(&id);
            }
        }
        self.sync_attached_status();
    }

    /// Tells a carried device's driver its session is live though no command runs (a takeover
    /// started or ended), so it doesn't take the session for one whose end never arrived.
    fn session_active(&self, target: &DeviceId, session_id: &str) {
        if let Ok(driver) = self.hosted.driver(target) {
            let session_id = session_id.to_owned();
            tokio::spawn(async move { driver.session_active(&session_id).await });
        }
    }

    /// Handles one frame from the service on `link`'s connection. Returns how the pair ended,
    /// when it did.
    async fn handle_frame(
        self: &Arc<Self>,
        link: &Arc<Link>,
        frame: ServiceFrame,
        tx: &mpsc::UnboundedSender<DeviceFrame>,
    ) -> Option<PairedEnd> {
        match frame {
            ServiceFrame::Command(c) => {
                tracing::info!("command {} `{}` in session {}", c.id, c.command, c.session_id);
                if c.target.is_none() && c.command == "terminal" && self.terminal_withheld(&link.id) {
                    let _ = tx.send(DeviceFrame::Result(refused(
                        c.id,
                        extend_protocol::TERMINAL_NOT_SHARED_REASON,
                    )));
                    return None;
                }
                self.dispatcher.submit_to(c, link.reply());
            }
            ServiceFrame::Cancel { id } => self.dispatcher.cancel(id),
            ServiceFrame::SessionStarted {
                target,
                session_id,
                silicon_id,
                since,
                side,
            } => {
                let carbon = self.status.get().pair(link.id.as_str()).and_then(|p| p.owner.clone());
                let info = InUseInfo {
                    silicon_id,
                    session_id: session_id.to_string(),
                    since: fmt_time(since),
                    pair: Some(link.id.to_string()),
                    carbon,
                    side,
                };
                tracing::info!(
                    "session {session_id} started on {}",
                    target.as_ref().map_or("this computer".into(), |t| t.to_string())
                );
                match &target {
                    None => self.status.update(|s| {
                        s.in_use = Some(info);
                        s.takeover = None;
                    }),
                    Some(id) => {
                        self.hosted.set_in_use(id, Some(info));
                        self.sync_attached_status();
                    }
                }
                // Other sides' wake requests are redacted, on screen and in the notification,
                // before any command of this session runs: its commands arrive after this frame
                // on this connection, which reads nothing more until this returns.
                let _ = self.render_wakes(false).await;
                let setup = self.dispatcher.session_started(target.as_ref(), session_id.as_str());
                if target.is_none() {
                    self.reprobe_after(setup);
                    self.update_display();
                }
            }
            ServiceFrame::SessionEnded {
                target,
                session_id,
                reason,
            } => {
                tracing::info!("session {session_id} ended: {}", reason.as_str());
                let cleanup = self.dispatcher.session_closed(target.as_ref(), session_id.as_str());
                let sid = session_id.to_string();
                match &target {
                    None => {
                        self.reprobe_after(cleanup);
                        self.status.update(|s| {
                            if s.in_use.as_ref().is_some_and(|u| u.session_id == sid) {
                                s.in_use = None;
                                s.takeover = None;
                            }
                        });
                        self.update_display();
                    }
                    Some(id) => {
                        self.hosted.set_in_use(id, None);
                        self.sync_attached_status();
                    }
                }
                let _ = self.render_wakes(false).await;
            }
            ServiceFrame::Takeover {
                target,
                session_id,
                reason,
                expires_at,
            } => {
                let info = TakeoverInfo {
                    session_id: session_id.to_string(),
                    reason,
                    expires_at: fmt_time(expires_at),
                };
                match target {
                    None => {
                        self.status.update(|s| s.takeover = Some(info));
                        // The Carbon has the computer now: its own settings decide the display.
                        self.update_display();
                    }
                    Some(id) => {
                        self.hosted.set_takeover(&id, Some(info));
                        self.sync_attached_status();
                        self.session_active(&id, session_id.as_str());
                    }
                }
            }
            ServiceFrame::TakeoverEnded { target, session_id } => match target {
                None => {
                    self.status.update(|s| s.takeover = None);
                    self.update_display();
                }
                Some(id) => {
                    self.hosted.set_takeover(&id, None);
                    self.sync_attached_status();
                    self.session_active(&id, session_id.as_str());
                }
            },
            ServiceFrame::Refresh => {
                if let Some(end) = self.refresh_device(link).await {
                    return Some(end);
                }
            }
            ServiceFrame::Attach {
                device_id,
                os,
                name,
                address,
                removed,
            } => {
                if removed {
                    tracing::info!("no longer carrying {device_id}");
                    if let Some(d) = self.hosted.remove(&device_id) {
                        tokio::spawn(async move { d.session_ended("").await });
                    }
                    self.sync_attached_status();
                } else {
                    tracing::info!("carrying {device_id} ({}) for {}", os.as_str(), link.id);
                    self.hosted.attach(AttachRecord {
                        device_id: device_id.clone(),
                        os,
                        name,
                        address,
                        host: Some(link.id.clone()),
                    });
                    self.sync_attached_status();
                    let core = self.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        for (_, f) in core.hosted.probe_these(&[device_id], false).await {
                            let _ = tx.send(DeviceFrame::Attached(f));
                        }
                        core.sync_attached_status();
                    });
                }
            }
            ServiceFrame::SetupCode { device_id, code } => {
                let core = self.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = match core.hosted.driver(&device_id) {
                        Ok(d) => d.setup_code(&code).await,
                        Err(e) => Err(e),
                    };
                    core.hosted.set_setup_error(&device_id, result.err());
                    for (_, f) in core.hosted.probe_these(&[device_id], false).await {
                        let _ = tx.send(DeviceFrame::Attached(f));
                    }
                    core.sync_attached_status();
                });
            }
            ServiceFrame::SetupRetry { target, step } => {
                let core = self.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match target {
                        None => {
                            tracing::info!(
                                "retrying setup ({}) now",
                                step.as_deref().unwrap_or("every failed step")
                            );
                            core.local.retry_setup(step.as_deref()).await;
                            core.reprobe.notify_one();
                        }
                        Some(id) => match core.hosted.retry_setup(&id, step.as_deref()).await {
                            Ok(()) => {
                                for (_, f) in core.hosted.probe_these(&[id], true).await {
                                    let _ = tx.send(DeviceFrame::Attached(f));
                                }
                                core.sync_attached_status();
                            }
                            Err(e) => tracing::warn!("couldn't retry the setup of {id}: {e}"),
                        },
                    }
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
            ServiceFrame::WakeRequest {
                target,
                wake_id,
                silicon_id,
                reason,
                side,
                alert,
                created_at,
                expires_at,
            } => match target {
                // A device this computer carries: it can't show anything itself, so it is only
                // checked more often until the request ends.
                Some(id) => {
                    self.hosted.watch_wake(&id, wake_id, expires_at);
                    self.sync_attached_status();
                    self.spawn_waking_probe();
                }
                None => {
                    self.wakes.lock().unwrap().upsert(WakeEntry {
                        wake_id,
                        pair: link.id.clone(),
                        silicon_id,
                        reason,
                        side,
                        alert,
                        created_at,
                        expires_at,
                    });
                    let shown = self.render_wakes(alert).await;
                    if self.wakes.lock().unwrap().first_answer(wake_id) {
                        let note = shown
                            .as_ref()
                            .err()
                            .map(|n| n.chars().take(extend_protocol::WAKE_NOTE_MAX_CHARS).collect());
                        let _ = tx.send(DeviceFrame::WakeRequestShown {
                            wake_id,
                            shown: shown.is_ok(),
                            note,
                        });
                    }
                }
            },
            ServiceFrame::WakeRequestEnded { target, wake_id, .. } => match target {
                Some(id) => {
                    self.hosted.unwatch_wake(&id, &wake_id);
                    self.sync_attached_status();
                }
                None => {
                    if self.wakes.lock().unwrap().remove(&wake_id) {
                        let _ = self.render_wakes(false).await;
                    }
                }
            },
            ServiceFrame::Credential { device_credential } => {
                // A new credential for this pair (a computer several Carbons paired, after a
                // session): stored first, then confirmed; the old one works until then. Neither
                // is ever logged.
                match self.credentials.replace(&link.id, device_credential.expose()) {
                    Ok(updated) => {
                        *link.cred.write().unwrap() = updated;
                        let _ = tx.send(DeviceFrame::CredentialSaved);
                        tracing::info!("stored the new credential of {}", link.id);
                    }
                    Err(e) => tracing::error!(
                        "couldn't store the new credential of {}; keeping the old one: {e:#}",
                        link.id
                    ),
                }
            }
        }
        None
    }
}

/// The overall phase from the pairs': online when any pair is.
fn overall_phase(pairs: &[PairInfo], current: Phase) -> Phase {
    if pairs.is_empty() {
        return current;
    }
    let any = |p: PairPhase| pairs.iter().any(|x| x.phase == p);
    if any(PairPhase::Online) {
        Phase::Online
    } else if any(PairPhase::Connecting) || any(PairPhase::Reconnecting) {
        Phase::Reconnecting
    } else if any(PairPhase::UpgradeRequired) {
        Phase::UpgradeRequired
    } else {
        Phase::Superseded
    }
}

/// A command the device refuses before it runs.
fn refused(id: Uuid, message: &str) -> CommandOutcome {
    CommandOutcome {
        id,
        ok: false,
        output: serde_json::Value::Null,
        text: Some(message.to_owned()),
        error: Some(CommandError {
            code: "unsupported_on_device".into(),
            message: message.to_owned(),
            details: serde_json::Value::Null,
        }),
        files: vec![],
    }
}

/// Keeps one pair connected until it ends, is taken over (then waits for Reconnect), or shutdown.
async fn run_link(
    core: Arc<Core>,
    link: Arc<Link>,
    mut ctl: mpsc::UnboundedReceiver<LinkCtl>,
    events: mpsc::UnboundedSender<Event>,
) {
    let url = core.service.ws_url("api/v1/device/connect");
    let mut backoff = Backoff::default();
    loop {
        if link.cancel.is_cancelled() {
            return;
        }
        let phase = core.status.get().pair(link.id.as_str()).map(|p| p.phase);
        if phase != Some(PairPhase::Online) && phase != Some(PairPhase::Connecting) {
            core.set_pair_phase(&link.id, PairPhase::Reconnecting);
        }
        let socket = tokio::select! {
            r = connect_link(&core, &link, &url) => r,
            _ = link.cancel.cancelled() => return,
        };
        let end = match socket {
            Ok(socket) => {
                let started = Instant::now();
                let end = connection(&core, &link, socket, &mut ctl).await;
                link.outbox.clear();
                if started.elapsed() > Duration::from_secs(60) {
                    backoff.reset();
                }
                end
            }
            Err(ConnectError::Http(401 | 403 | 404)) => {
                ConnEnd::Paired(PairedEnd::Unpaired("credential refused".into()))
            }
            Err(ConnectError::Http(426)) => ConnEnd::Paired(PairedEnd::UpgradeRequired),
            Err(e) => ConnEnd::Dropped(e.to_string()),
        };
        match end {
            ConnEnd::Paired(PairedEnd::Shutdown | PairedEnd::Closed) => return,
            ConnEnd::Paired(PairedEnd::Unpaired(reason)) => {
                let _ = events.send(Event::PairEnded {
                    id: link.id.clone(),
                    reason,
                });
                return;
            }
            ConnEnd::Paired(PairedEnd::Superseded) => {
                tracing::info!("another connection took over the pair {}", link.id);
                core.set_pair_phase(&link.id, PairPhase::Superseded);
                if wait_for_retry(&link, &mut ctl, Duration::MAX).await {
                    return;
                }
            }
            ConnEnd::Paired(PairedEnd::UpgradeRequired) => {
                core.set_pair_phase(&link.id, PairPhase::UpgradeRequired);
                if wait_for_retry(&link, &mut ctl, Duration::from_secs(3600)).await {
                    return;
                }
            }
            ConnEnd::Dropped(why) => {
                tracing::info!("device socket of {} down: {why}", link.id);
                core.set_pair_phase(&link.id, PairPhase::Reconnecting);
                core.status.update(|s| s.last_error = Some(why.clone()));
                if wait_for_retry(&link, &mut ctl, backoff.next_delay()).await {
                    return;
                }
            }
        }
    }
}

/// Waits for Reconnect, `d`, or the pair's end. True when the pair ended (or the app stops).
async fn wait_for_retry(link: &Link, ctl: &mut mpsc::UnboundedReceiver<LinkCtl>, d: Duration) -> bool {
    let sleep = tokio::time::sleep(d.min(Duration::from_secs(86_400 * 365)));
    tokio::pin!(sleep);
    tokio::select! {
        _ = &mut sleep => false,
        _ = link.cancel.cancelled() => true,
        c = ctl.recv() => match c {
            Some(LinkCtl::Reconnect) => false,
            None => true,
        },
    }
}

/// Connects with the pair's credential, falling back to the one it replaced when the service
/// never took the rotated one.
async fn connect_link(core: &Core, link: &Link, url: &url::Url) -> Result<Socket, ConnectError> {
    let cred = link.cred.read().unwrap().clone();
    match ws::connect(url, &device_auth(&cred.device_credential)).await {
        Ok(socket) => {
            if cred.previous_credential.is_some() {
                // The service promoted the new credential with this connection.
                keep_credential(core, link, |c| c.previous_credential = None);
            }
            Ok(socket)
        }
        Err(ConnectError::Http(401 | 403 | 404)) if cred.previous_credential.is_some() => {
            let previous = cred.previous_credential.clone().unwrap_or_default();
            let socket = ws::connect(url, &device_auth(&previous)).await?;
            tracing::warn!(
                "Extend didn't take the new credential of {}; using the one it replaced",
                link.id
            );
            keep_credential(core, link, |c| {
                if let Some(p) = c.previous_credential.take() {
                    c.device_credential = p;
                }
            });
            Ok(socket)
        }
        Err(e) => Err(e),
    }
}

fn keep_credential(core: &Core, link: &Link, change: impl FnOnce(&mut StoredCredential)) {
    let updated = {
        let mut c = link.cred.write().unwrap();
        change(&mut c);
        c.clone()
    };
    if let Err(e) = core.credentials.save(&updated) {
        tracing::warn!("couldn't update the stored credential of {}: {e:#}", link.id);
    }
}

/// One live device socket of one pair.
async fn connection(
    core: &Arc<Core>,
    link: &Arc<Link>,
    socket: Socket,
    ctl: &mut mpsc::UnboundedReceiver<LinkCtl>,
) -> ConnEnd {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<DeviceFrame>();
    link.outbox.set(tx.clone());
    // Watched from before hello, so a change while this connection says hello (another pair
    // added, this pair learning it is the first) still gets a hello of its own.
    let mut probes = core.probes.subscribe();
    probes.borrow_and_update();
    let mut pairs_changed = core.pairs_changed.subscribe();
    pairs_changed.borrow_and_update();
    let mut awake = core.awake.subscribe();
    awake.borrow_and_update();

    // hello first, then whether it is awake, then what's attached. The pair counts as online once
    // hello is out.
    let probe = core.fresh_probe().await;
    let mut last_hello = core.hello_for(&link.id, &probe);
    if send_frame(&mut sink, &DeviceFrame::Hello(last_hello.clone()))
        .await
        .is_err()
        || send_frame(&mut sink, &core.awake.frame()).await.is_err()
    {
        // The service may have closed at once (taken over, unpaired): its close says which.
        return closed_early(&mut stream)
            .await
            .unwrap_or_else(|| ConnEnd::Dropped("couldn't send hello".into()));
    }
    core.set_pair_phase(&link.id, PairPhase::Online);
    core.status.update(|s| s.last_error = None);
    tracing::info!("connected to Extend as device {}", link.id);
    core.hosted.forget_sent_for(&link.id);
    core.spawn_hosted_probe(Some(link.id.clone()), true);
    if let Some(end) = core.refresh_device(link).await {
        return ConnEnd::Paired(end);
    }

    let idle = Duration::from_secs(extend_protocol::OFFLINE_AFTER_S + 15);
    let mut last_heard = Instant::now();
    // What the service's greeting has said about the carried devices, until it ends.
    let mut greeting: Option<Greeting> = Some(Greeting::default());

    loop {
        let deadline = last_heard + idle;
        let mut new_hello: Option<Hello> = None;
        tokio::select! {
            msg = stream.next() => {
                last_heard = Instant::now();
                let msg = match msg {
                    None => return ConnEnd::Dropped("Extend closed the connection".into()),
                    Some(Err(e)) => return ConnEnd::Dropped(e.to_string()),
                    Some(Ok(m)) => m,
                };
                match msg {
                    Message::Text(text) => match serde_json::from_str::<ServiceFrame>(&text) {
                        Ok(frame) => {
                            // The greeting is over at the first ping, which the service sends
                            // only once it has announced every carried device's live session.
                            match (&frame, greeting.as_mut()) {
                                (ServiceFrame::Attach { device_id, removed: false, .. }, Some(g)) => {
                                    g.attached.insert(device_id.clone());
                                }
                                (ServiceFrame::SessionStarted { target: Some(id), .. }, Some(g)) => {
                                    g.announced.insert(id.clone());
                                }
                                (ServiceFrame::Ping { .. }, Some(_)) => {
                                    if let Some(g) = greeting.take() {
                                        core.end_sessions_not_announced(&link.id, &g);
                                    }
                                }
                                _ => {}
                            }
                            if let Some(end) = core.handle_frame(link, frame, &tx).await {
                                let _ = sink.send(Message::Close(None)).await;
                                return ConnEnd::Paired(end);
                            }
                        }
                        // Never echo the text: a newer service's frame may carry a credential.
                        Err(e) => tracing::debug!("ignoring a frame this version doesn't know ({e})"),
                    },
                    Message::Ping(data) => {
                        let _ = sink.send(Message::Pong(data)).await;
                    }
                    Message::Close(frame) => {
                        return closed_with(frame.as_ref().map(|f| u16::from(f.code)));
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
                return ConnEnd::Dropped("nothing from Extend for a minute".into());
            }
            Ok(()) = probes.changed() => {
                let p = probes.borrow_and_update().clone();
                if let Some(p) = p {
                    new_hello = Some(core.hello_for(&link.id, &p));
                }
            }
            Ok(()) = pairs_changed.changed() => {
                pairs_changed.borrow_and_update();
                let p = core.probes.borrow().clone();
                if let Some(p) = p {
                    new_hello = Some(core.hello_for(&link.id, &p));
                }
            }
            Ok(()) = awake.changed() => {
                awake.borrow_and_update();
                if let Err(e) = send_frame(&mut sink, &core.awake.frame()).await {
                    return ConnEnd::Dropped(format!("couldn't send: {e}"));
                }
            }
            Some(_) = ctl.recv() => {}
            _ = link.cancel.cancelled() => {
                let _ = sink.send(Message::Close(None)).await;
                return ConnEnd::Paired(if core.shutdown.is_cancelled() { PairedEnd::Shutdown } else { PairedEnd::Closed });
            }
        }
        if let Some(hello) = new_hello
            && let Some(frame) = hello_update(&last_hello, &hello)
        {
            last_hello = hello;
            if send_frame(&mut sink, &frame).await.is_err() {
                return ConnEnd::Dropped("couldn't send hello".into());
            }
        }
    }
}

/// How a close with `code` ends the connection.
fn closed_with(code: Option<u16>) -> ConnEnd {
    match code {
        Some(close::UNAUTHORIZED) => ConnEnd::Paired(PairedEnd::Unpaired("credential refused (4401)".into())),
        Some(close::SUPERSEDED) => ConnEnd::Paired(PairedEnd::Superseded),
        Some(close::UPGRADE_REQUIRED) => ConnEnd::Paired(PairedEnd::UpgradeRequired),
        other => ConnEnd::Dropped(format!(
            "Extend closed the connection ({})",
            other.map_or("no code".into(), |c| c.to_string())
        )),
    }
}

/// The close the service sent before the app could say hello, if one is waiting.
async fn closed_early<S>(stream: &mut S) -> Option<ConnEnd>
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let wait = async {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Close(frame) = msg {
                return Some(closed_with(frame.as_ref().map(|f| u16::from(f.code))));
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_secs(2), wait).await.ok().flatten()
}

/// Probes this computer: at start, every probe interval (every 5 s while setup needs the
/// Carbon), when something changed, and when a connection needs a fresh one. The carried devices
/// are checked on the interval too, and every 5 s while a Silicon's request to wake one is open.
async fn prober(core: Arc<Core>) {
    let mut ticker = tokio::time::interval(WAKE_PROBE_EVERY.min(core.probe_interval));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_full: Option<Instant> = None;
    loop {
        let mut carried_too = false;
        tokio::select! {
            _ = core.shutdown.cancelled() => return,
            _ = ticker.tick() => {
                let setup_incomplete = core.status.get().setup_needs_carbon();
                let due = last_full.is_none_or(|t| {
                    t.elapsed() >= core.probe_interval || (setup_incomplete && t.elapsed() >= Duration::from_secs(5))
                });
                if !due {
                    core.spawn_waking_probe();
                    continue;
                }
                carried_too = true;
            }
            _ = core.reprobe.notified() => {}
            _ = core.fresh.notified() => {}
        }
        if carried_too {
            last_full = Some(Instant::now());
            core.spawn_hosted_probe(None, false);
        }
        let p = core.probe_local().await;
        core.apply_probe_to_status(&p);
        core.probes.send_replace(Some(Arc::new(p)));
    }
}

/// Reads the screen every [`SCREEN_WATCH_EVERY`]: a lock, unlock, sleep or wake has this computer
/// checked again at once, goes to Extend as `awake` on every pair's connection, answers the wake
/// requests (an awake computer needs no waking), and holds or releases the display.
async fn watch_screen(core: Arc<Core>) {
    let Some(watch) = core.screen_watch.clone() else {
        // Nothing to read (a headless server): always awake.
        core.awake.set(AwakeNow::UNKNOWN_IS_AWAKE);
        core.status.update(|s| s.awake = Some(true));
        return;
    };
    let read = |w: ScreenWatch| async move { tokio::task::spawn_blocking(move || w()).await.unwrap_or_default() };
    let mut tracker = AwakeTracker::default();
    let mut last: Option<Option<ScreenBlock>> = None;
    loop {
        let now = read(watch.clone()).await;
        if let Some(before) = last
            && before != now.block
        {
            tracing::info!("screen: {} → {}", describe_screen(before), describe_screen(now.block));
            core.reprobe.notify_one();
        }
        last = Some(now.block);
        if let Some(state) = tracker.observe(&now) {
            core.awake.set(state);
            core.status.update(|s| {
                s.awake = Some(state.awake);
                s.sleep_state = state.sleep_state;
            });
            // Awake with the Carbon there (or no way to tell): the requests are answered, and
            // the service resolves them from the `awake` frame.
            let answered = state.awake && state.input_seen != Some(false) && core.wakes.lock().unwrap().clear();
            if answered {
                let _ = core.render_wakes(false).await;
            }
            core.update_display();
        }
        tokio::select! {
            _ = core.shutdown.cancelled() => return,
            _ = tokio::time::sleep(SCREEN_WATCH_EVERY) => {}
        }
    }
}

/// Forgets wake requests past their expiry.
async fn wake_janitor(core: Arc<Core>) {
    loop {
        tokio::select! {
            _ = core.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let expired = core.wakes.lock().unwrap().expire(time::OffsetDateTime::now_utc());
        if expired {
            let _ = core.render_wakes(false).await;
        }
        core.sync_attached_status();
    }
}

fn describe_screen(s: Option<ScreenBlock>) -> &'static str {
    match s {
        None => "usable",
        Some(ScreenBlock::Locked) => "locked",
        Some(ScreenBlock::OtherSession) => "another account",
        Some(ScreenBlock::Asleep) => "asleep",
        Some(ScreenBlock::AdminPrompt) => "admin prompt",
    }
}

/// A probe when this computer's own check didn't answer in time: only the terminal is known.
fn fallback_probe() -> Probe {
    crate::drivers::local::with_terminal(Probe {
        os: crate::sysinfo::device_os(),
        os_version: crate::sysinfo::os_version(),
        model: crate::sysinfo::model(),
        capabilities: vec![],
        missing: vec![],
        setup: extend_protocol::model::Setup::complete(),
        engine_version: None,
        online: true,
        awake: None,
        sleep_state: None,
        hardware_id: None,
    })
}

pub fn hello_from(p: &Probe) -> Hello {
    Hello {
        app_version: APP_VERSION.into(),
        os: p.os,
        os_version: p.os_version.clone(),
        model: p.model.clone(),
        engine_version: p.engine_version.clone(),
        capabilities: p.capabilities.clone(),
        missing: p.missing.clone(),
        setup: p.setup.clone(),
        features: vec![extend_protocol::feature::SETUP_RETRY.into()],
    }
}

/// The hello for a pair whose Silicons don't get the terminal (a computer several Carbons
/// paired; not the first pair): the terminal is missing, with a reason that names no Carbon.
pub fn withhold_terminal(h: &mut Hello) {
    h.capabilities.retain(|c| *c != Capability::Terminal);
    h.missing.retain(|m| m.capability != Capability::Terminal);
    h.missing.push(MissingCapability {
        capability: Capability::Terminal,
        reason: extend_protocol::TERMINAL_NOT_SHARED_REASON.into(),
    });
}

/// What to send when a re-probe differs from the last `hello`: a new `hello` when capabilities
/// changed, `setup_progress` when only the setup did, nothing otherwise.
pub fn hello_update(last: &Hello, now: &Hello) -> Option<DeviceFrame> {
    let setup_only = Hello {
        setup: last.setup.clone(),
        ..now.clone()
    } == *last;
    if now == last {
        None
    } else if setup_only {
        Some(DeviceFrame::SetupProgress {
            setup: now.setup.clone(),
        })
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
    EnvironmentInfo {
        environment_id: e.environment_id.to_string(),
        name: e.name.clone(),
        state: e.state.clone(),
    }
}

fn fmt_time(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Wait helper for callers outside the agent.
pub async fn sleep_unless(d: Duration, token: &CancellationToken) -> bool {
    sleep_or_shutdown(d, token).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use extend_protocol::model::{Setup, SetupStep, StepStatus};
    use extend_protocol::{Capability, DeviceOs};

    fn hello(caps: Vec<Capability>, setup: Setup) -> Hello {
        Hello {
            app_version: "1".into(),
            os: DeviceOs::Macos,
            os_version: None,
            model: None,
            engine_version: None,
            capabilities: caps,
            missing: vec![],
            setup,
            features: vec![],
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

    #[test]
    fn a_shared_computers_later_pairs_get_no_terminal() {
        let mut h = hello(vec![Capability::ScreenRead, Capability::Terminal], Setup::complete());
        withhold_terminal(&mut h);
        assert_eq!(h.capabilities, vec![Capability::ScreenRead]);
        assert_eq!(h.missing.len(), 1);
        assert_eq!(h.missing[0].capability, Capability::Terminal);
        assert_eq!(h.missing[0].reason, extend_protocol::TERMINAL_NOT_SHARED_REASON);
        // Twice is the same as once.
        withhold_terminal(&mut h);
        assert_eq!(h.missing.len(), 1);
    }

    #[test]
    fn hellos_advertise_setup_retry() {
        let p = fallback_probe();
        let h = hello_from(&p);
        assert!(h.supports(extend_protocol::feature::SETUP_RETRY));
        assert!(h.capabilities.contains(&Capability::Terminal));
        let v = serde_json::to_value(DeviceFrame::Hello(h)).unwrap();
        assert!(v.get("agent_device_version").is_none(), "{v}");
    }

    #[test]
    fn the_overall_phase_follows_the_pairs() {
        let pair = |phase| PairInfo {
            phase,
            ..Default::default()
        };
        assert_eq!(overall_phase(&[], Phase::Enrolling), Phase::Enrolling);
        assert_eq!(
            overall_phase(&[pair(PairPhase::Superseded), pair(PairPhase::Online)], Phase::Starting),
            Phase::Online
        );
        assert_eq!(
            overall_phase(
                &[pair(PairPhase::Superseded), pair(PairPhase::Reconnecting)],
                Phase::Online
            ),
            Phase::Reconnecting
        );
        assert_eq!(
            overall_phase(&[pair(PairPhase::Superseded)], Phase::Online),
            Phase::Superseded
        );
        assert_eq!(
            overall_phase(
                &[pair(PairPhase::UpgradeRequired), pair(PairPhase::Superseded)],
                Phase::Online
            ),
            Phase::UpgradeRequired
        );
    }
}
