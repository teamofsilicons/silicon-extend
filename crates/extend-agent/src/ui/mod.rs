//! The tray / menu-bar icon, the Silicon Extend window, and the in-use banner.
//!
//! The UI owns the main thread (every desktop OS wants its event loop there); the agent runs on a
//! Tokio runtime in the background and the two talk through [`AgentHandle`]: status comes in as a
//! watch, taps go out as [`UiAction`]s.
//!
//! * Menu: the headline ("Pairing code: 4F9C2A", "Paired to c:alice", "si:chef is using this
//!   Mac"), the test environment, **Stop** / **Done**, Show Silicon Extend…, Start at login,
//!   Revoke pair…, Quit.
//! * Window: the big pairing code and how to use it, the setup steps with buttons that open the
//!   right settings page, the device and its Carbon, the test-environment banner, devices this
//!   computer carries, and Revoke pair with a confirmation.
//! * Banner: a small always-on-top strip, "si:chef is using this Mac  [Stop]", shown for as long
//!   as a Silicon is using the computer (and "… needs you: <reason>  [Done]" during a takeover).
//!   It never takes focus by itself, so it doesn't get in the way of the Silicon's typing.

pub mod icon;

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

#[derive(Debug)]
enum UserEvent {
    Status(Box<AgentStatus>),
    Menu(MenuEvent),
    Ipc { banner: bool, body: String },
    AgentStopped,
}

/// Runs the UI until Quit. `agent_thread` is joined (briefly) on the way out.
pub fn run(handle: AgentHandle, runtime: tokio::runtime::Handle, agent_thread: std::thread::JoinHandle<()>) -> ! {
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
}

fn icon_state(s: &AgentStatus) -> IconState {
    if s.in_use.is_some() || s.takeover.is_some() || s.attached.iter().any(|a| a.in_use.is_some()) {
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
/// Silicon, or the same one again), a takeover, or the banner going away always brings back the
/// full banner, so Stop, Done and the takeover reason are never hidden by an earlier choice.
fn keep_minimized(minimized: bool, before: &AgentStatus, after: &AgentStatus) -> bool {
    let session = |s: &AgentStatus| s.in_use.as_ref().map(|u| u.session_id.clone());
    // Not `expires_at`: the same takeover read back from the service may format it differently.
    let takeover = |s: &AgentStatus| s.takeover.as_ref().map(|t| (t.session_id.clone(), t.reason.clone()));
    minimized
        && (after.in_use.is_some() || after.takeover.is_some())
        && session(before) == session(after)
        && (after.takeover.is_none() || takeover(before) == takeover(after))
}

/// The banner's size. Collapsed, it still has the Silicon's name, Stop (or Done) and the test
/// environment's name, so stopping stays one tap.
fn banner_size(minimized: bool, environment: bool) -> LogicalSize<f64> {
    match (minimized, environment) {
        (true, false) => LogicalSize::new(250.0, 44.0),
        (true, true) => LogicalSize::new(360.0, 44.0),
        (false, _) => LogicalSize::new(420.0, 52.0),
    }
}

fn make_icon(state: IconState) -> Option<Icon> {
    Icon::from_rgba(icon::rgba(state), icon::SIZE, icon::SIZE).ok()
}

/// The JSON the page renders: the status plus a few words for this OS.
fn page_state(s: &AgentStatus) -> String {
    let mut v = serde_json::to_value(s).unwrap_or_default();
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
        if let Some(d) = &s.device
            && let Some(name) = &d.name
            && s.phase != Phase::Enrolling
        {
            add(&MenuItem::with_id("name", format!("This {word}: {name}"), false, None));
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
        for a in &s.attached {
            let line = match (&a.error, &a.in_use) {
                (Some(e), _) => format!("{}: {}", a.name, truncate(e, 50)),
                (None, Some(u)) => format!("{}: {} is using it", a.name, u.silicon_id),
                (None, None) if a.online => format!("{}: online", a.name),
                _ => format!("{}: offline", a.name),
            };
            add(&MenuItem::with_id(
                format!("attached:{}", a.device_id),
                line,
                false,
                None,
            ));
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
            crate::autostart::is_installed(),
            None,
        ));
        if s.phase == Phase::Superseded {
            add(&MenuItem::with_id("reconnect", "Connect this copy instead", true, None));
        }
        if s.device.is_some() && s.phase != Phase::Enrolling {
            add(&MenuItem::with_id("revoke", "Revoke pair…", true, None));
        }
        add(&PredefinedMenuItem::separator());
        add(&MenuItem::with_id("quit", "Quit Silicon Extend", true, None));
        menu
    }

    fn on_status(&mut self, s: AgentStatus, target: &EventLoopWindowTarget<UserEvent>) {
        let first_code = s.phase == Phase::Enrolling && !self.shown_code;
        let banner_needed = s.in_use.is_some() || s.takeover.is_some();
        let minimized = keep_minimized(self.banner_minimized, &self.status, &s);
        let resize = minimized != self.banner_minimized || s.environment.is_some() != self.status.environment.is_some();
        self.banner_minimized = minimized;
        self.status = s;
        if resize {
            self.fit_banner();
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

    fn push_state(&self) {
        let js = format!(
            "window.__extend && window.__extend({}, {})",
            page_state(&self.status),
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
        banner_size(self.banner_minimized, self.status.environment.is_some())
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
        match id {
            "stop" => self.send(UiAction::Stop { target: None }),
            "takeover_done" => self.send(UiAction::TakeoverDone { target: None }),
            "show" => self.show_main(target),
            "banner_restore" => {
                self.set_banner_minimized(false);
                self.show_banner(target);
            }
            "revoke" => {
                // The confirmation lives in the window.
                self.show_main(target);
                if let Some((_, v)) = &self.main {
                    let _ = v.evaluate_script("document.getElementById('confirm').classList.remove('hidden')");
                }
            }
            "reconnect" => self.send(UiAction::Reconnect),
            "autostart" => {
                let result = if crate::autostart::is_installed() {
                    crate::autostart::uninstall().map(|_| ())
                } else {
                    crate::autostart::install(&Default::default()).map(|_| ())
                };
                if let Err(e) = result {
                    tracing::warn!("couldn't change start at login: {e:#}");
                }
                if let Some(t) = &self.tray {
                    t.set_menu(Some(Box::new(self.build_menu())));
                }
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
            "revoke" if !banner => self.send(UiAction::RevokePair),
            "reconnect" => self.send(UiAction::Reconnect),
            "reprobe" => self.send(UiAction::Reprobe),
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
        if let Some(t) = self.agent_thread.take() {
            // Give the agent a moment to close its socket and agent-device sessions.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        self.tray = None;
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
        let v: serde_json::Value = serde_json::from_str(&page_state(&s)).unwrap();
        assert_eq!(v["pairing"]["code"], "4F9C2A");
        assert!(v["computer_word"].is_string());
        assert!(v["settings_steps"].is_array());
    }

    fn in_use(session: &str) -> AgentStatus {
        AgentStatus {
            phase: Phase::Online,
            in_use: Some(InUseInfo {
                silicon_id: "si:alpha".into(),
                session_id: session.into(),
                since: String::new(),
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
        assert!(banner_size(true, false).width >= 250.0);
        assert!(banner_size(true, true).width > banner_size(true, false).width);
        assert!(banner_size(true, true).width < banner_size(false, true).width);
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
    fn page_has_no_forbidden_words() {
        let lower = PAGE.to_ascii_lowercase();
        for w in ["human", "ai agent", "frontend", "backend", "organization", " org "] {
            assert!(!lower.contains(w), "page.html says {w:?}");
        }
    }
}
