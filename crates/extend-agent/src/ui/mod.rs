//! The tray / menu-bar icon, the Silicon Extend window, and the in-use banner.
//!
//! The UI owns the main thread (every desktop OS wants its event loop there); the agent runs on a
//! Tokio runtime in the background and the two talk through [`AgentHandle`]: status comes in as a
//! watch, taps go out as [`UiAction`]s.
//!
//! * Menu: the headline ("Pairing code: 4F9C2A", "Paired to c:alice and c:bob", "si:chef is
//!   using this Mac"), each Carbon's pair, the test environment, **Stop** / **Done** (for this
//!   computer and for each device it carries), Reconnect for a pair another connection took over,
//!   Show Silicon Extend…, Start at login, Pair with another Carbon…, Revoke pair…, Quit.
//! * Window: the big pairing code and how to use it, the setup steps with buttons that open the
//!   right settings page, the Carbons it is paired to (each with Revoke pair, confirmed, and
//!   Reconnect when taken over), "Pair with another Carbon" (after the shared-computer warning)
//!   and its code, Silicons' requests to wake it, the test-environment banner, devices this
//!   computer carries (and whether they are awake), start at login with a switch to turn it off,
//!   and "Download the update" when Extend needs a newer app.
//! * Banner: a small always-on-top strip, "si:chef is using this Mac  [Stop]", shown for as long
//!   as a Silicon is using the computer or a device it carries (one row each), and "… needs you:
//!   <reason>  [Done]" during a takeover. It never takes focus by itself, so it doesn't get in the
//!   way of the Silicon's typing.
//!
//! Once the computer is paired, start at login is turned on unless the Carbon turned it off
//! ([`crate::autostart::after_pairing`]).

pub mod icon;

use std::path::PathBuf;
use std::time::Duration;

use extend_protocol::DeviceId;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use wry::{WebView, WebViewBuilder};

use crate::agent::{AgentHandle, UiAction};
use crate::status::{AgentStatus, Phase};
use icon::IconState;

const PAGE: &str = include_str!("page.html");

/// What the window needs from the configuration.
#[derive(Debug, Clone)]
pub struct Context {
    /// Where the start-at-login choice is kept.
    pub state_dir: PathBuf,
    /// Where "Download the update" goes (`Config::download_url`).
    pub download_url: String,
}

/// Start at login as the window shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AutostartView {
    on: bool,
    /// Why the last change didn't happen, and what to do.
    error: Option<String>,
}

#[derive(Debug)]
enum UserEvent {
    Status(Box<AgentStatus>),
    Menu(MenuEvent),
    Ipc { banner: bool, body: String },
    AgentStopped,
}

/// Runs the UI until Quit. `agent_thread` is joined (briefly) on the way out.
pub fn run(
    handle: AgentHandle,
    runtime: tokio::runtime::Handle,
    agent_thread: std::thread::JoinHandle<()>,
    context: Context,
) -> ! {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS as _};
        // A menu-bar app: no Dock icon, no app menu.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();

    // Status changes wake the UI.
    {
        let proxy = proxy.clone();
        let mut rx = handle.status.subscribe();
        runtime.spawn(async move {
            loop {
                let s = rx.borrow_and_update().clone();
                if proxy.send_event(UserEvent::Status(Box::new(s))).is_err() {
                    return;
                }
                if rx.changed().await.is_err() {
                    let _ = proxy.send_event(UserEvent::AgentStopped);
                    return;
                }
            }
        });
    }
    // Shutdown from elsewhere (a signal, or the agent ending) closes the UI too.
    {
        let proxy = proxy.clone();
        let token = handle.shutdown.clone();
        runtime.spawn(async move {
            token.cancelled().await;
            let _ = proxy.send_event(UserEvent::AgentStopped);
        });
    }
    {
        let proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = proxy.send_event(UserEvent::Menu(e));
        }));
    }

    let mut ui = Ui {
        handle,
        proxy,
        tray: None,
        main: None,
        banner: None,
        banner_minimized: false,
        status: AgentStatus::default(),
        shown_code: false,
        agent_thread: Some(agent_thread),
        context,
        autostart: AutostartView {
            on: crate::autostart::is_installed(),
            error: None,
        },
        checked_autostart: false,
        download_error: None,
    };

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => ui.create_tray(),
            Event::UserEvent(UserEvent::Status(s)) => ui.on_status(*s, target),
            Event::UserEvent(UserEvent::Menu(e)) => ui.on_menu(e.id.0.as_str(), target, control_flow),
            Event::UserEvent(UserEvent::Ipc { banner, body }) => ui.on_ipc(banner, &body, target),
            Event::UserEvent(UserEvent::AgentStopped) => {
                ui.quit(control_flow);
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if let Some((w, _)) = &ui.main
                    && w.id() == window_id
                {
                    w.set_visible(false);
                }
            }
            _ => {}
        }
    })
}

struct Ui {
    handle: AgentHandle,
    proxy: EventLoopProxy<UserEvent>,
    tray: Option<TrayIcon>,
    main: Option<(Window, WebView)>,
    banner: Option<(Window, WebView)>,
    banner_minimized: bool,
    status: AgentStatus,
    shown_code: bool,
    agent_thread: Option<std::thread::JoinHandle<()>>,
    context: Context,
    autostart: AutostartView,
    /// Start at login was settled for this run once the computer was paired.
    checked_autostart: bool,
    /// Why "Download the update" couldn't open the browser.
    download_error: Option<String>,
}

/// One session the banner shows: this computer's own (`target` none) or one on a device it
/// carries. A takeover is listed even when its session has already been announced as ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BannerSession {
    target: Option<String>,
    session_id: Option<String>,
    takeover: Option<(String, String)>,
}

/// The banner's sessions, this computer's first, then the carried devices' in the menu's order.
fn banner_sessions(s: &AgentStatus) -> Vec<BannerSession> {
    let mut out = Vec::new();
    if s.in_use.is_some() || s.takeover.is_some() {
        out.push(BannerSession {
            target: None,
            session_id: s.in_use.as_ref().map(|u| u.session_id.clone()),
            takeover: s.takeover.as_ref().map(|t| (t.session_id.clone(), t.reason.clone())),
        });
    }
    for a in &s.attached {
        if a.in_use.is_some() || a.takeover.is_some() {
            out.push(BannerSession {
                target: Some(a.device_id.clone()),
                session_id: a.in_use.as_ref().map(|u| u.session_id.clone()),
                takeover: a.takeover.as_ref().map(|t| (t.session_id.clone(), t.reason.clone())),
            });
        }
    }
    out
}

/// The most rows the banner shows at once (a row each for this computer and carried devices).
const BANNER_MAX_ROWS: usize = 4;

fn icon_state(s: &AgentStatus) -> IconState {
    if s.in_use.is_some()
        || s.takeover.is_some()
        || s.attached.iter().any(|a| a.in_use.is_some() || a.takeover.is_some())
    {
        IconState::InUse
    } else if matches!(s.phase, Phase::Enrolling | Phase::Starting) {
        IconState::Unpaired
    } else if matches!(
        s.phase,
        Phase::Reconnecting | Phase::Superseded | Phase::UpgradeRequired
    ) || s.setup_needs_carbon()
    {
        IconState::Attention
    } else {
        IconState::Idle
    }
}

/// Whether a collapsed banner stays collapsed across a status change. A new session (another
/// Silicon, the same one again, or one on a carried device), a takeover, or the banner going away
/// always brings back the full banner, so Stop, Done and the takeover reason are never hidden by an
/// earlier choice. (Not `expires_at`: the same takeover read back from the service may format it
/// differently.)
fn keep_minimized(minimized: bool, before: &AgentStatus, after: &AgentStatus) -> bool {
    let sessions = |s: &AgentStatus| {
        banner_sessions(s)
            .into_iter()
            .map(|b| (b.target, b.session_id))
            .collect::<Vec<_>>()
    };
    let takeovers = |s: &AgentStatus| {
        banner_sessions(s)
            .into_iter()
            .filter_map(|b| b.takeover.map(|t| (b.target, t)))
            .collect::<Vec<_>>()
    };
    let now = takeovers(after);
    minimized
        && !banner_sessions(after).is_empty()
        && sessions(before) == sessions(after)
        && (now.is_empty() || takeovers(before) == now)
}

/// The banner's size. Collapsed, it still has the Silicon's name, Stop (or Done) and the test
/// environment's name, so stopping stays one tap. Expanded, each carried device in use adds a
/// row with its own Stop (or Done).
fn banner_size(minimized: bool, environment: bool, rows: usize) -> LogicalSize<f64> {
    let extra = rows.clamp(1, BANNER_MAX_ROWS) - 1;
    match (minimized, environment) {
        (true, false) => LogicalSize::new(250.0, 44.0),
        (true, true) => LogicalSize::new(360.0, 44.0),
        (false, _) => LogicalSize::new(420.0, 52.0 + 38.0 * extra as f64),
    }
}

fn make_icon(state: IconState) -> Option<Icon> {
    Icon::from_rgba(icon::rgba(state), icon::SIZE, icon::SIZE).ok()
}

/// What the page shows besides the status.
#[derive(Debug, Clone, Default)]
struct PageExtras {
    autostart: AutostartView,
    download_url: String,
    download_error: Option<String>,
}

/// The JSON the page renders: the status plus a few words for this OS.
fn page_state(s: &AgentStatus, extras: &PageExtras) -> String {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    v["autostart"] = serde_json::json!({"on": extras.autostart.on, "error": extras.autostart.error});
    v["download_url"] = extras.download_url.clone().into();
    v["download_host"] = url::Url::parse(&extras.download_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
        .into();
    v["download_error"] = extras.download_error.clone().into();
    // In memory only (never in status.json): the wake requests and which carried device a
    // Silicon asked to wake.
    v["wake_requests"] = serde_json::to_value(&s.wake_requests).unwrap_or_default();
    if let Some(list) = v["attached"].as_array_mut() {
        for (a, info) in list.iter_mut().zip(&s.attached) {
            a["wake_requested"] = info.wake_requested.into();
        }
    }
    v["share_warning"] = share_warning(&os_user()).into();
    let os_label = if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    v["computer_word"] = crate::sysinfo::computer_word().into();
    v["os_label"] = os_label.into();
    let settings: &[&str] = if cfg!(target_os = "macos") {
        &["accessibility", "screen_recording", "xcode"]
    } else {
        &[]
    };
    v["settings_steps"] = serde_json::json!(settings);
    v.to_string()
}

impl Ui {
    fn create_tray(&mut self) {
        let state = icon_state(&self.status);
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(self.build_menu()))
            .with_tooltip("Silicon Extend")
            .with_menu_on_left_click(true);
        if let Some(i) = make_icon(state) {
            builder = builder.with_icon(i).with_icon_as_template(icon::is_template(state));
        }
        match builder.build() {
            Ok(t) => self.tray = Some(t),
            Err(e) => tracing::warn!("couldn't create the tray icon ({e}); the window stays available"),
        }
    }

    fn build_menu(&self) -> Menu {
        let s = &self.status;
        let menu = Menu::new();
        let word = crate::sysinfo::computer_word();
        let add = |item: &dyn tray_icon::menu::IsMenuItem| {
            let _ = menu.append(item);
        };
        add(&MenuItem::with_id("headline", s.headline(), false, None));
        if s.phase != Phase::Enrolling {
            for line in pair_menu(s, word) {
                add(&MenuItem::with_id(line.id, line.label, line.enabled, None));
            }
        }
        if let Some(env) = &s.environment {
            add(&MenuItem::with_id(
                "env",
                format!("Test environment: {}", env.name),
                false,
                None,
            ));
        }
        if let Some(t) = &s.takeover {
            add(&MenuItem::with_id(
                "takeover_reason",
                format!("Waiting for you: {}", truncate(&t.reason, 60)),
                false,
                None,
            ));
            add(&MenuItem::with_id("takeover_done", "Done", true, None));
        }
        if let Some(u) = &s.in_use {
            add(&MenuItem::with_id("stop", format!("Stop {}", u.silicon_id), true, None));
        }
        for item in attached_menu(s) {
            add(&MenuItem::with_id(item.id, item.label, item.enabled, None));
        }
        add(&PredefinedMenuItem::separator());
        add(&MenuItem::with_id("show", "Show Silicon Extend…", true, None));
        if s.in_use.is_some() || s.takeover.is_some() {
            add(&MenuItem::with_id("banner_restore", "Show activity banner", true, None));
        }
        add(&CheckMenuItem::with_id(
            "autostart",
            "Start at login",
            true,
            self.autostart.on,
            None,
        ));
        if paired(s) && s.phase == Phase::Online && s.adding_pair.is_none() {
            add(&MenuItem::with_id(
                "pair_another",
                "Pair with another Carbon…",
                true,
                None,
            ));
        }
        if !s.pairs.is_empty() && s.phase != Phase::Enrolling {
            add(&MenuItem::with_id("revoke", "Revoke pair…", true, None));
        }
        add(&PredefinedMenuItem::separator());
        add(&MenuItem::with_id("quit", "Quit Silicon Extend", true, None));
        menu
    }

    fn on_status(&mut self, s: AgentStatus, target: &EventLoopWindowTarget<UserEvent>) {
        let first_code = s.phase == Phase::Enrolling && !self.shown_code;
        let banner_needed = !banner_sessions(&s).is_empty();
        let minimized = keep_minimized(self.banner_minimized, &self.status, &s);
        let resize = minimized != self.banner_minimized
            || s.environment.is_some() != self.status.environment.is_some()
            || banner_sessions(&s).len() != banner_sessions(&self.status).len();
        self.banner_minimized = minimized;
        self.status = s;
        if resize {
            self.fit_banner();
        }
        if !self.checked_autostart && paired(&self.status) {
            self.checked_autostart = true;
            self.settle_autostart();
        }
        if let Some(t) = &self.tray {
            let state = icon_state(&self.status);
            let _ = t.set_icon_with_as_template(make_icon(state), icon::is_template(state));
            let _ = t.set_tooltip(Some(format!("Silicon Extend: {}", self.status.headline())));
            t.set_menu(Some(Box::new(self.build_menu())));
        }
        // Setup and connection errors must be visible even before the service returns a code.
        if first_code {
            self.shown_code = true;
            self.show_main(target);
        }
        self.push_state();
        if banner_needed {
            self.show_banner(target);
        } else if let Some((w, _)) = &self.banner {
            w.set_visible(false);
        }
    }

    fn extras(&self) -> PageExtras {
        PageExtras {
            autostart: self.autostart.clone(),
            download_url: self.context.download_url.clone(),
            download_error: self.download_error.clone(),
        }
    }

    /// Once the computer is paired: start at login on, unless the Carbon turned it off.
    fn settle_autostart(&mut self) {
        match crate::autostart::apply_after_pairing(&self.context.state_dir) {
            Ok(done) => tracing::info!("{done}"),
            Err(e) => {
                tracing::warn!("couldn't turn start at login on: {e:#}");
                self.autostart.error = Some(format!(
                    "Couldn't turn on start at login: {} Silicon Extend won't open by itself after a restart until it is on; try the switch again.",
                    sentence(&format!("{e:#}"))
                ));
            }
        }
        self.autostart.on = crate::autostart::is_installed();
        self.refresh_menu();
        self.push_state();
    }

    /// The Carbon's switch (window or menu): on or off for good.
    fn set_autostart(&mut self, on: bool) {
        let result = crate::autostart::set_by_carbon(&self.context.state_dir, on, &Default::default());
        self.autostart.error = match result {
            Ok(done) => {
                tracing::info!("start at login {}: {done}", if on { "on" } else { "off" });
                None
            }
            Err(e) => {
                tracing::warn!("couldn't change start at login: {e:#}");
                Some(format!(
                    "Couldn't turn start at login {}: {}",
                    if on { "on" } else { "off" },
                    sentence(&format!("{e:#}"))
                ))
            }
        };
        self.autostart.on = crate::autostart::is_installed();
        self.refresh_menu();
        self.push_state();
    }

    fn refresh_menu(&self) {
        if let Some(t) = &self.tray {
            t.set_menu(Some(Box::new(self.build_menu())));
        }
    }

    /// Opens the configured download page in the Carbon's browser.
    fn open_download(&mut self) {
        self.download_error = open_in_browser(&self.context.download_url).err().map(|why| {
            format!(
                "Couldn't open your browser ({why}). Open {} yourself to download the update.",
                self.context.download_url
            )
        });
        self.push_state();
    }

    fn push_state(&self) {
        let js = format!(
            "window.__extend && window.__extend({}, {})",
            page_state(&self.status, &self.extras()),
            self.banner_minimized
        );
        for (_, view) in self.main.iter().chain(self.banner.iter()) {
            let _ = view.evaluate_script(&js);
        }
    }

    fn webview(&self, window: &Window, banner: bool) -> Option<WebView> {
        let proxy = self.proxy.clone();
        let builder = WebViewBuilder::new()
            .with_initialization_script(format!("window.__view = '{}';", if banner { "banner" } else { "main" }))
            .with_html(PAGE)
            // The banner is never focused; its Stop button must work on the first click.
            .with_accept_first_mouse(true)
            .with_ipc_handler(move |req: wry::http::Request<String>| {
                let _ = proxy.send_event(UserEvent::Ipc {
                    banner,
                    body: req.body().clone(),
                });
            });
        #[cfg(any(target_os = "macos", windows))]
        let built = builder.build(window);
        #[cfg(target_os = "linux")]
        let built = {
            use tao::platform::unix::WindowExtUnix as _;
            use wry::WebViewBuilderExtUnix as _;
            builder.build_gtk(window.default_vbox()?)
        };
        match built {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("couldn't show the Silicon Extend window: {e}");
                None
            }
        }
    }

    fn show_main(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        if self.main.is_none() {
            let window = match WindowBuilder::new()
                .with_title("Silicon Extend")
                .with_inner_size(LogicalSize::new(440.0, 600.0))
                .with_min_inner_size(LogicalSize::new(380.0, 420.0))
                .with_visible(false)
                .build(target)
            {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("couldn't open a window: {e}");
                    return;
                }
            };
            let Some(view) = self.webview(&window, false) else {
                return;
            };
            self.main = Some((window, view));
        }
        if let Some((w, _)) = &self.main {
            w.set_visible(true);
            w.set_focus();
        }
        self.push_state();
    }

    fn show_banner(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        if self.banner.is_none() {
            let size = self.banner_size();
            let mut builder = WindowBuilder::new()
                .with_title("Silicon Extend: in use")
                .with_inner_size(size)
                .with_resizable(false)
                .with_decorations(false)
                .with_always_on_top(true)
                .with_focused(false)
                .with_visible(false);
            if let Some(m) = target.primary_monitor() {
                let scale = m.scale_factor();
                let screen = m.size().to_logical::<f64>(scale);
                let origin = m.position().to_logical::<f64>(scale);
                // Bottom centre, above the Dock or taskbar: where screen-sharing indicators sit,
                // and clear of the menu bar and of apps' own toolbars.
                builder = builder.with_position(LogicalPosition::new(
                    origin.x + (screen.width - size.width) / 2.0,
                    origin.y + screen.height - size.height - 110.0,
                ));
            }
            let window = match builder.build(target) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("couldn't show the in-use banner: {e}");
                    return;
                }
            };
            let Some(view) = self.webview(&window, true) else {
                return;
            };
            self.banner = Some((window, view));
        }
        if let Some((w, _)) = &self.banner {
            w.set_visible(true);
        }
    }

    fn banner_size(&self) -> LogicalSize<f64> {
        banner_size(
            self.banner_minimized,
            self.status.environment.is_some(),
            banner_sessions(&self.status).len(),
        )
    }

    fn set_banner_minimized(&mut self, minimized: bool) {
        self.banner_minimized = minimized;
        self.fit_banner();
        self.push_state();
    }

    /// Sizes the banner for its state, keeping the position the Carbon chose on screen.
    fn fit_banner(&self) {
        if let Some((window, _)) = &self.banner {
            // Keep the position chosen by the user. Expanding near an edge must stay reachable.
            window.set_inner_size(self.banner_size());
            if let (Ok(position), Some(monitor)) = (window.outer_position(), window.current_monitor()) {
                let scale = window.scale_factor();
                let position = position.to_logical::<f64>(scale);
                let origin = monitor.position().to_logical::<f64>(scale);
                let screen = monitor.size().to_logical::<f64>(scale);
                let size = self.banner_size();
                window.set_outer_position(LogicalPosition::new(
                    position
                        .x
                        .clamp(origin.x, origin.x + (screen.width - size.width).max(0.0)),
                    position
                        .y
                        .clamp(origin.y, origin.y + (screen.height - size.height).max(0.0)),
                ));
            }
        }
    }

    fn send(&self, a: UiAction) {
        let _ = self.handle.actions.send(a);
    }

    fn on_menu(&mut self, id: &str, target: &EventLoopWindowTarget<UserEvent>, control_flow: &mut ControlFlow) {
        tracing::info!("menu: {id}");
        if let Some((action, device)) = id.split_once(':')
            && let Ok(device) = device.parse::<DeviceId>()
        {
            match action {
                "stop" => self.send(UiAction::Stop { target: Some(device) }),
                "done" => self.send(UiAction::TakeoverDone { target: Some(device) }),
                "reconnect" => self.send(UiAction::Reconnect {
                    device_id: Some(device),
                }),
                _ => {}
            }
            return;
        }
        match id {
            "stop" => self.send(UiAction::Stop { target: None }),
            "takeover_done" => self.send(UiAction::TakeoverDone { target: None }),
            "show" => self.show_main(target),
            "banner_restore" => {
                self.set_banner_minimized(false);
                self.show_banner(target);
            }
            "revoke" => {
                // The confirmation lives in the window (one pair), or the list of Carbons does.
                self.show_main(target);
                if let Some((_, v)) = &self.main {
                    let _ = v.evaluate_script("window.__askRevoke && window.__askRevoke()");
                }
            }
            "pair_another" => {
                // The shared-computer warning lives in the window.
                self.show_main(target);
                if let Some((_, v)) = &self.main {
                    let _ = v.evaluate_script("window.__askPairAnother && window.__askPairAnother()");
                }
            }
            "reconnect" => self.send(UiAction::Reconnect { device_id: None }),
            "autostart" => {
                let on = !crate::autostart::is_installed();
                self.set_autostart(on);
            }
            "quit" => self.quit(control_flow),
            _ => {}
        }
    }

    fn on_ipc(&mut self, banner: bool, body: &str, target: &EventLoopWindowTarget<UserEvent>) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(body) else {
            return;
        };
        let action = msg.get("action").and_then(|a| a.as_str()).unwrap_or("");
        tracing::info!("{} action: {action}", if banner { "banner" } else { "window" });
        let device = || {
            msg.get("target")
                .and_then(|t| t.as_str())
                .and_then(|t| t.parse::<DeviceId>().ok())
        };
        match action {
            "ready" => self.push_state(),
            "banner_drag" if banner => {
                if let Some((window, _)) = &self.banner
                    && let Err(error) = window.drag_window()
                {
                    tracing::warn!("couldn't move the activity banner: {error}");
                }
            }
            "banner_minimize" if banner => self.set_banner_minimized(true),
            "banner_restore" if banner => self.set_banner_minimized(false),
            "stop" => self.send(UiAction::Stop { target: device() }),
            "takeover_done" => self.send(UiAction::TakeoverDone { target: device() }),
            "revoke" if !banner => {
                if let Some(device_id) = device() {
                    self.send(UiAction::RevokePair { device_id });
                }
            }
            "reconnect" => self.send(UiAction::Reconnect { device_id: device() }),
            "pair_another" if !banner => self.send(UiAction::PairAnother),
            "cancel_pair_another" if !banner => self.send(UiAction::CancelPairAnother),
            "reprobe" => self.send(UiAction::Reprobe),
            "set_autostart" if !banner => {
                if let Some(on) = msg.get("on").and_then(|v| v.as_bool()) {
                    self.set_autostart(on);
                }
            }
            "open_download" if !banner => self.open_download(),
            "open_settings" => {
                let step = msg.get("step").and_then(|s| s.as_str()).unwrap_or("");
                open_settings(step);
                self.send(UiAction::Reprobe);
            }
            _ => {}
        }
        let _ = target;
    }

    fn quit(&mut self, control_flow: &mut ControlFlow) {
        self.handle.shutdown.cancel();
        // Gone from the screen at once; then the agent gets the time it gives its cleanups
        // (closing the device engine sessions still open: an iPhone keeps showing "Automation
        // Running" until its session is closed), and its runtime a moment to wind down.
        self.tray = None;
        for (window, _) in self.main.iter().chain(self.banner.iter()) {
            window.set_visible(false);
        }
        if let Some(t) = self.agent_thread.take() {
            let deadline = std::time::Instant::now() + crate::agent::QUIT_CLEANUP_LIMIT + Duration::from_secs(4);
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        *control_flow = ControlFlow::Exit;
    }
}

fn open_settings(step: &str) {
    #[cfg(target_os = "macos")]
    {
        if step == "screen_recording" {
            crate::drivers::probe_macos::request_screen_recording();
        }
        crate::drivers::probe_macos::open_settings(step);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = step;
}

/// Paired, and past pairing: the computer has at least one Carbon's pair.
fn paired(s: &AgentStatus) -> bool {
    !s.pairs.is_empty() && !matches!(s.phase, Phase::Enrolling | Phase::Starting | Phase::NotRunning)
}

/// The account a Silicon's terminal runs as here.
fn os_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "this computer's account".into())
}

/// What the Carbon reads before a second Carbon pairs this computer. The terminal runs as the
/// computer's own account; only the Silicons of the Carbon who installed Silicon Extend get it
/// (Carbon decision, 2026-09-27), but everyone's Silicons use the screen, keyboard and apps.
fn share_warning(user: &str) -> String {
    format!(
        "Silicons the Carbon who installed Silicon Extend here gives access to use its terminal as {user}, so they can reach what {user} can, including this app's other pairs. Silicons any Carbon gives access to use the screen, the keyboard and the apps, and can see what others leave open. Share a computer only with Carbons you trust."
    )
}

/// The menu's lines for the Carbons this computer is paired to: one each once there are several
/// (with Reconnect for a pair another connection took over).
fn pair_menu(s: &AgentStatus, word: &str) -> Vec<MenuLine> {
    let mut out = Vec::new();
    let many = s.pairs.len() > 1;
    for p in &s.pairs {
        let owner = p.owner.as_deref().unwrap_or("a Carbon");
        if many || p.name.is_some() {
            let label = match (&p.name, many) {
                (Some(name), true) => format!("{owner}: {name}"),
                (Some(name), false) => format!("This {word}: {name}"),
                (None, _) => owner.to_owned(),
            };
            out.push(MenuLine {
                id: format!("pair:{}", p.device_id),
                label,
                enabled: false,
            });
        }
        if p.phase == crate::status::PairPhase::Superseded {
            out.push(MenuLine {
                id: format!("reconnect:{}", p.device_id),
                label: if many {
                    format!("Reconnect {owner}'s pair")
                } else {
                    "Connect this copy again".into()
                },
                enabled: true,
            });
        }
    }
    out
}

/// A line of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuLine {
    id: String,
    label: String,
    enabled: bool,
}

/// The menu's lines for the devices this computer carries: a status line each, and for one in
/// use, Stop (and Done during a takeover), one click each, as for this computer.
fn attached_menu(s: &AgentStatus) -> Vec<MenuLine> {
    let mut out = Vec::new();
    for a in &s.attached {
        let line = |label: String| MenuLine {
            id: format!("attached:{}", a.device_id),
            label,
            enabled: false,
        };
        if let Some(e) = &a.error {
            out.push(line(format!("{}: {}", a.name, truncate(e, 50))));
            continue;
        }
        if let Some(t) = &a.takeover {
            out.push(line(format!(
                "{}: waiting for you: {}",
                a.name,
                truncate(&t.reason, 50)
            )));
            out.push(MenuLine {
                id: format!("done:{}", a.device_id),
                label: format!("Done on {}", a.name),
                enabled: true,
            });
        }
        match &a.in_use {
            Some(u) => out.push(MenuLine {
                id: format!("stop:{}", a.device_id),
                label: format!("Stop {} on {}", u.silicon_id, a.name),
                enabled: true,
            }),
            None if a.takeover.is_none() => out.push(line(format!(
                "{}: {}",
                a.name,
                if a.online { "online" } else { "offline" }
            ))),
            None => {}
        }
    }
    out
}

/// Opens an http(s) link in the default browser.
fn open_in_browser(link: &str) -> Result<(), String> {
    let parsed = url::Url::parse(link).map_err(|e| format!("{link} isn't a link: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("{link} isn't a web link"));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.arg(parsed.as_str());
        c
    };
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", parsed.as_str()]);
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(parsed.as_str());
        c
    };
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    // Reaped in the background: the opener exits as soon as it has handed the link over.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `s` ending in exactly one full stop.
fn sentence(s: &str) -> String {
    format!("{}.", s.trim().trim_end_matches('.'))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{InUseInfo, PairingInfo, TakeoverInfo};

    #[test]
    fn icon_follows_the_status() {
        let mut s = AgentStatus {
            phase: Phase::Enrolling,
            ..Default::default()
        };
        assert_eq!(icon_state(&s), IconState::Unpaired);
        s.phase = Phase::Online;
        assert_eq!(icon_state(&s), IconState::Idle);
        s.phase = Phase::Reconnecting;
        assert_eq!(icon_state(&s), IconState::Attention);
        s.in_use = Some(InUseInfo {
            silicon_id: "si:chef".into(),
            session_id: "a3f".into(),
            since: String::new(),
            pair: None,
            carbon: None,
            side: None,
        });
        assert_eq!(icon_state(&s), IconState::InUse);
    }

    #[test]
    fn page_state_carries_os_words() {
        let s = AgentStatus {
            phase: Phase::Enrolling,
            pairing: Some(PairingInfo {
                code: "4F9C2A".into(),
                expires_at: "2026-09-26T10:05:00Z".into(),
            }),
            ..Default::default()
        };
        let extras = PageExtras {
            autostart: AutostartView { on: true, error: None },
            download_url: "https://downloads.example.org/extend/mac".into(),
            download_error: None,
        };
        let v: serde_json::Value = serde_json::from_str(&page_state(&s, &extras)).unwrap();
        assert_eq!(v["pairing"]["code"], "4F9C2A");
        assert!(v["computer_word"].is_string());
        assert!(v["settings_steps"].is_array());
        // The update's download page and start at login come from the configuration and the
        // entry on disk, not from the page.
        assert_eq!(v["download_url"], "https://downloads.example.org/extend/mac");
        assert_eq!(v["download_host"], "downloads.example.org");
        assert_eq!(v["autostart"], serde_json::json!({"on": true, "error": null}));
    }

    #[test]
    fn the_update_view_has_a_download_button_and_start_at_login_a_switch() {
        // The update view: one primary action that asks the app to open the configured page.
        let upgrade = PAGE
            .split_once("<section id=\"upgrade\"")
            .unwrap()
            .1
            .split_once("</section>")
            .unwrap()
            .0;
        assert!(upgrade.contains("data-action=\"open_download\""), "{upgrade}");
        assert!(upgrade.contains("button primary"), "{upgrade}");
        assert!(
            !upgrade.contains("extend.teamofsilicons.com"),
            "the page names the configured host"
        );
        // Start at login: a visible switch that sends set_autostart with on/off.
        let startup = PAGE
            .split_once("<section id=\"startup\"")
            .unwrap()
            .1
            .split_once("</section>")
            .unwrap()
            .0;
        assert!(startup.contains("data-action=\"set_autostart\""), "{startup}");
        assert!(PAGE.contains("msg.on = el.getAttribute('data-on') === 'true'"));
    }

    fn carried(id: &str, name: &str) -> crate::status::AttachedInfo {
        crate::status::AttachedInfo {
            device_id: id.into(),
            name: name.into(),
            os: extend_protocol::DeviceOs::Ios,
            online: true,
            in_use: None,
            takeover: None,
            setup: None,
            error: None,
            awake: None,
            sleep_state: None,
            host: None,
            wake_requested: false,
        }
    }

    #[test]
    fn carried_devices_have_one_click_stop_and_done_in_the_menu() {
        let mut phone = carried("3f2a1b0c", "Alice's iPhone");
        let tv = carried("9d8e7f6a", "Living room TV");
        let mut s = AgentStatus {
            phase: Phase::Online,
            attached: vec![phone.clone(), tv.clone()],
            ..Default::default()
        };
        let menu = attached_menu(&s);
        assert!(menu.iter().all(|m| !m.enabled), "{menu:?}");
        phone.in_use = Some(InUseInfo {
            silicon_id: "si:chef".into(),
            session_id: "b40".into(),
            since: String::new(),
            pair: None,
            carbon: None,
            side: None,
        });
        s.attached = vec![phone.clone(), tv.clone()];
        let menu = attached_menu(&s);
        let stop = menu.iter().find(|m| m.id == "stop:3f2a1b0c").expect("a Stop item");
        assert!(stop.enabled);
        assert_eq!(stop.label, "Stop si:chef on Alice's iPhone");
        // The menu's ids name the device the action goes to.
        let (action, device) = stop.id.split_once(':').unwrap();
        assert_eq!(action, "stop");
        assert!(device.parse::<DeviceId>().is_ok());
        phone.takeover = Some(TakeoverInfo {
            session_id: "b40".into(),
            reason: "Approve Face ID".into(),
            expires_at: String::new(),
        });
        s.attached = vec![phone, tv];
        let menu = attached_menu(&s);
        let done = menu.iter().find(|m| m.id == "done:3f2a1b0c").expect("a Done item");
        assert!(done.enabled);
        assert!(menu.iter().any(|m| m.id == "stop:3f2a1b0c" && m.enabled));
        assert!(
            menu.iter()
                .any(|m| m.label.contains("waiting for you: Approve Face ID") && !m.enabled)
        );
        assert!(menu.iter().any(|m| m.label == "Living room TV: online" && !m.enabled));
    }

    #[test]
    fn the_banner_shows_carried_devices_in_use_too() {
        let mut phone = carried("3f2a1b0c", "Alice's iPhone");
        let idle = AgentStatus {
            phase: Phase::Online,
            attached: vec![phone.clone()],
            ..Default::default()
        };
        assert!(banner_sessions(&idle).is_empty());
        phone.in_use = Some(InUseInfo {
            silicon_id: "si:chef".into(),
            session_id: "b40".into(),
            since: String::new(),
            pair: None,
            carbon: None,
            side: None,
        });
        let phone_only = AgentStatus {
            attached: vec![phone.clone()],
            ..idle.clone()
        };
        let rows = banner_sessions(&phone_only);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.as_deref(), Some("3f2a1b0c"));
        assert_eq!(icon_state(&phone_only), IconState::InUse);
        // This computer's own session comes first; each session gets a row.
        let both = AgentStatus {
            attached: vec![phone.clone()],
            ..in_use("a3f")
        };
        let rows = banner_sessions(&both);
        assert_eq!(
            rows.iter().map(|r| r.target.clone()).collect::<Vec<_>>(),
            vec![None, Some("3f2a1b0c".into())]
        );
        assert!(banner_size(false, false, 2).height > banner_size(false, false, 1).height);
        assert_eq!(
            banner_size(false, false, 9).height,
            banner_size(false, false, BANNER_MAX_ROWS).height
        );
        assert_eq!(banner_size(true, false, 3).height, banner_size(true, false, 1).height);
        // A carried device's new session opens a collapsed banner again.
        assert!(keep_minimized(true, &in_use("a3f"), &in_use("a3f")));
        assert!(!keep_minimized(true, &in_use("a3f"), &both));
        assert!(keep_minimized(true, &both, &both.clone()));
        assert!(keep_minimized(true, &phone_only, &phone_only.clone()));
        let mut paused = phone.clone();
        paused.takeover = Some(TakeoverInfo {
            session_id: "b40".into(),
            reason: "Approve Face ID".into(),
            expires_at: String::new(),
        });
        let taken = AgentStatus {
            attached: vec![paused],
            ..idle
        };
        assert!(!keep_minimized(true, &phone_only, &taken));
        // The page renders a row with Stop for each carried device, and targets its buttons.
        assert!(PAGE.contains("id=\"banner-more\""));
        assert!(PAGE.contains("function bannerRows("));
    }

    fn in_use(session: &str) -> AgentStatus {
        AgentStatus {
            phase: Phase::Online,
            in_use: Some(InUseInfo {
                silicon_id: "si:alpha".into(),
                session_id: session.into(),
                since: String::new(),
                pair: None,
                carbon: None,
                side: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_collapsed_banner_opens_again_for_a_new_session_or_a_takeover() {
        let a3f = in_use("a3f");
        // The Carbon's choice holds while the same session goes on.
        assert!(keep_minimized(true, &a3f, &a3f.clone()));
        assert!(!keep_minimized(false, &a3f, &a3f.clone()));
        // The session ends (the banner hides): the next one starts expanded.
        assert!(!keep_minimized(true, &a3f, &AgentStatus::default()));
        // Another session replaces it directly.
        assert!(!keep_minimized(true, &a3f, &in_use("b40")));
        // A takeover shows its reason and Done.
        let mut takeover = a3f.clone();
        takeover.takeover = Some(TakeoverInfo {
            session_id: "a3f".into(),
            reason: "Approve the admin prompt".into(),
            expires_at: "2026-09-26T10:30:00Z".into(),
        });
        assert!(!keep_minimized(true, &a3f, &takeover));
        // Collapsing again during that takeover sticks, until a different takeover arrives.
        assert!(keep_minimized(true, &takeover, &takeover.clone()));
        let mut another = takeover.clone();
        another.takeover.as_mut().unwrap().reason = "Enter the one-time code".into();
        assert!(!keep_minimized(true, &takeover, &another));
    }

    #[test]
    fn the_collapsed_banner_keeps_room_for_stop_and_the_test_environment() {
        assert!(banner_size(true, false, 1).width >= 250.0);
        assert!(banner_size(true, true, 1).width > banner_size(true, false, 1).width);
        assert!(banner_size(true, true, 1).width < banner_size(false, true, 1).width);
        // Collapsed, the page hides only the long text and the collapse button.
        let mut css = String::new();
        let mut rest = PAGE;
        while let Some((before, after)) = rest.split_once("/*") {
            css.push_str(before);
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        }
        css.push_str(rest);
        let hidden: Vec<&str> = css
            .split('}')
            .filter_map(|rule| rule.split_once('{'))
            .filter(|(_, body)| body.replace(' ', "").contains("display:none"))
            .flat_map(|(selectors, _)| selectors.split(','))
            .map(str::trim)
            .filter(|s| s.starts_with("body.banner-minimized"))
            .collect();
        assert!(hidden.iter().any(|s| s.ends_with("#banner-text")), "{hidden:?}");
        for kept in ["#banner-button", "#banner-env"] {
            assert!(
                !hidden.iter().any(|s| s.ends_with(kept)),
                "{kept} is hidden in the collapsed banner: {hidden:?}"
            );
        }
    }

    #[test]
    fn messages_end_in_one_full_stop() {
        assert_eq!(sentence("Move it to Applications."), "Move it to Applications.");
        assert_eq!(sentence("permission denied"), "permission denied.");
    }

    #[test]
    fn each_carbon_gets_a_menu_line_and_a_taken_over_pair_a_reconnect() {
        use crate::status::{PairInfo, PairPhase};
        let one = AgentStatus {
            phase: Phase::Online,
            pairs: vec![PairInfo {
                device_id: "7c1e09ab".into(),
                name: Some("Studio Mac".into()),
                owner: Some("c:alice".into()),
                phase: PairPhase::Online,
                ..Default::default()
            }],
            ..Default::default()
        };
        let lines = pair_menu(&one, "Mac");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].label, "This Mac: Studio Mac");
        let mut two = one.clone();
        two.pairs.push(PairInfo {
            device_id: "0d44e1f2".into(),
            name: Some("Family Mac".into()),
            owner: Some("c:bob".into()),
            phase: PairPhase::Superseded,
            ..Default::default()
        });
        let lines = pair_menu(&two, "Mac");
        let labels: Vec<&str> = lines.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(
            labels,
            ["c:alice: Studio Mac", "c:bob: Family Mac", "Reconnect c:bob's pair"]
        );
        assert_eq!(lines[2].id, "reconnect:0d44e1f2");
        assert!(lines[2].enabled && !lines[0].enabled);
        let (action, device) = lines[2].id.split_once(':').unwrap();
        assert_eq!(action, "reconnect");
        assert!(device.parse::<DeviceId>().is_ok());
    }

    #[test]
    fn the_shared_computer_warning_names_the_account_and_no_carbon() {
        let w = share_warning("alice");
        assert!(w.contains("use its terminal as alice"), "{w}");
        assert!(w.contains("including this app's other pairs"), "{w}");
        assert!(w.ends_with("Share a computer only with Carbons you trust."), "{w}");
        // The page shows it before any code, and has a Revoke pair per Carbon.
        assert!(PAGE.contains("data-action=\"pair_another\""));
        assert!(PAGE.contains("id=\"share-body\""));
        assert!(PAGE.contains("data-action=\"ask_revoke\" data-target="));
    }

    #[test]
    fn wake_requests_reach_the_page_but_not_the_status_file() {
        let s = AgentStatus {
            phase: Phase::Online,
            wake_requests: vec![crate::status::WakeInfo {
                wake_id: "w".into(),
                pair: "7c1e09ab".into(),
                carbon: Some("c:alice".into()),
                silicon_id: Some("si:chef".into()),
                reason: Some("Check the order screen".into()),
                expires_at: "2026-09-27T10:32:00Z".into(),
            }],
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::from_str(&page_state(&s, &PageExtras::default())).unwrap();
        assert_eq!(v["wake_requests"][0]["reason"], "Check the order screen");
        assert!(!serde_json::to_string(&s.for_file()).unwrap().contains("order screen"));
    }

    #[test]
    fn page_has_no_forbidden_words() {
        let lower = PAGE.to_ascii_lowercase();
        for w in ["human", "ai agent", "frontend", "backend", "organization", " org "] {
            assert!(!lower.contains(w), "page.html says {w:?}");
        }
    }
}
