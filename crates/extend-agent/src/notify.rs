//! Wake notifications: when a Silicon asks this computer's Carbon to wake it (`extend device
//! wake`), the computer shows a system notification with the Silicon's name and reason
//! (`UNDERSTANDING.md`, "Waking a device").
//!
//! There is one notification for the computer, listing every open request from every pair's
//! connection ("si:chef and 1 more ask to use this Mac"). While a session runs, a request from
//! another side (its side tag differs from the session's, or it has none) shows without the
//! Silicon and reason: "A Silicon asked to use this device; its Carbon was told through Ting".
//! The agent re-posts the notification that way before the session's first command, so a
//! Silicon on the computer never reads another side's request, in the notification centre or its
//! history either (the old one is removed or replaced, never left behind).
//!
//! The pure part (what to show, redaction, collapsing, escaping, answering once per request) is
//! unit-tested here; the [`Notifier`] backends only put text on screen:
//!
//! * Mac: `UNUserNotificationCenter`, one request id replaced each time, and
//!   `removeDeliveredNotifications` to take it down. Only inside the app bundle (the API aborts a
//!   process without one); `extend-agent run` from a terminal answers "not shown" instead.
//! * Windows: a WinRT toast under the app's AUMID, with one tag and group, removed from the
//!   history (`ToastNotificationHistory.Remove`) before it is shown again or when it goes.
//! * Linux: `org.freedesktop.Notifications.Notify` over D-Bus with `replaces_id` (and
//!   `CloseNotification`). Without that backend, it reports that the request could not be shown;
//!   an untrackable notification could expose another side after its session starts.
//!
//! Reasons are shown as plain text: XML-escaped for toasts and markup-escaped for D-Bus,
//! never interpreted anywhere.

use std::collections::{BTreeMap, HashSet};

use extend_protocol::DeviceId;
use extend_protocol::model::Timestamp;
use uuid::Uuid;

/// What a redacted request shows instead of its Silicon and reason.
pub const REDACTED: &str = "A Silicon asked to use this device; its Carbon was told through Ting.";
/// The same for several redacted requests.
pub const REDACTED_MANY: &str = "Silicons asked to use this device; their Carbons were told through Ting.";

/// One open request to wake this computer, as the service sent it.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeEntry {
    pub wake_id: Uuid,
    /// The pair it was made through (and whose connection answers `wake_request_shown`).
    pub pair: DeviceId,
    pub silicon_id: Option<String>,
    pub reason: Option<String>,
    pub side: Option<String>,
    /// Sound for this one (at most every 15 minutes per device, the service decides).
    pub alert: bool,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

/// A request as it may be shown right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub wake_id: Uuid,
    pub pair: DeviceId,
    /// Both absent when redacted.
    pub silicon_id: Option<String>,
    pub reason: Option<String>,
    pub expires_at: Timestamp,
}

impl Shown {
    pub fn redacted(&self) -> bool {
        self.silicon_id.is_none()
    }
}

/// The open wake requests for this computer, from every pair's connection.
#[derive(Debug, Default)]
pub struct WakeBook {
    entries: BTreeMap<Uuid, WakeEntry>,
    /// Requests `wake_request_shown` was already sent for (once per request, ever).
    answered: HashSet<Uuid>,
}

impl WakeBook {
    /// Adds or refreshes a request. A frame without a Silicon and reason replaces what was held
    /// for it (the service redacted it). Returns whether anything changed.
    pub fn upsert(&mut self, e: WakeEntry) -> bool {
        if self.entries.get(&e.wake_id) == Some(&e) {
            return false;
        }
        self.entries.insert(e.wake_id, e);
        true
    }

    /// Forgets a request (it ended, whatever the reason).
    pub fn remove(&mut self, wake_id: &Uuid) -> bool {
        self.entries.remove(wake_id).is_some()
    }

    /// Forgets the requests that came through `pair` (its Carbon's pair ended).
    pub fn remove_pair(&mut self, pair: &DeviceId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, e| &e.pair != pair);
        before != self.entries.len()
    }

    /// Forgets requests past their expiry.
    pub fn expire(&mut self, now: Timestamp) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, e| e.expires_at > now);
        before != self.entries.len()
    }

    /// The computer is awake: every request is answered (the service resolves them).
    pub fn clear(&mut self) -> bool {
        let had = !self.entries.is_empty();
        self.entries.clear();
        had
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True the first time it is asked for `wake_id`: `wake_request_shown` goes once per request.
    pub fn first_answer(&mut self, wake_id: Uuid) -> bool {
        self.answered.insert(wake_id)
    }

    /// Whether `wake_id`'s latest frame asked for a sound.
    pub fn alert(&self, wake_id: &Uuid) -> bool {
        self.entries.get(wake_id).is_some_and(|e| e.alert)
    }

    /// The requests, oldest first, as they may be shown while a session with `active_side` runs
    /// (`None`: no session anywhere on this computer or the devices it carries). Another side's
    /// request, or one without a side, is redacted while a session runs.
    pub fn visible(&self, active_side: Option<&str>) -> Vec<Shown> {
        let mut out: Vec<Shown> = self
            .entries
            .values()
            .map(|e| {
                let hidden = match active_side {
                    None => false,
                    Some(side) => e.side.as_deref() != Some(side),
                };
                let named = !hidden && e.silicon_id.is_some();
                Shown {
                    wake_id: e.wake_id,
                    pair: e.pair.clone(),
                    silicon_id: if named { e.silicon_id.clone() } else { None },
                    reason: if named { e.reason.clone() } else { None },
                    expires_at: e.expires_at,
                }
            })
            .collect();
        out.sort_by_key(|s| {
            self.entries
                .get(&s.wake_id)
                .map(|e| e.created_at)
                .unwrap_or(s.expires_at)
        });
        out
    }
}

/// The one notification: a title and body in plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// Play the notification sound (only for the request that asked for it).
    pub alert: bool,
}

/// What to show for `shown` on a computer called `word` ("Mac", "computer"), or nothing.
pub fn notification(shown: &[Shown], word: &str, alert: bool) -> Option<Notification> {
    let first = shown.first()?;
    let others = shown.len() - 1;
    let named: Vec<&Shown> = shown.iter().filter(|s| !s.redacted()).collect();
    let title = match (&first.silicon_id, others) {
        (Some(id), 0) => format!("{id} asks to use this {word}"),
        (Some(id), n) => format!("{id} and {n} more ask to use this {word}"),
        (None, 0) => format!("A Silicon asks to use this {word}"),
        (None, n) => match named.first() {
            Some(s) => format!(
                "{} and {n} more ask to use this {word}",
                s.silicon_id.as_deref().unwrap_or("A Silicon")
            ),
            None => format!("{} Silicons ask to use this {word}", n + 1),
        },
    };
    let mut lines: Vec<String> = Vec::new();
    if shown.len() == 1 && !first.redacted() {
        lines.push(one_line(first.reason.as_deref().unwrap_or("")));
    } else {
        for s in &named {
            lines.push(format!(
                "{}: {}",
                s.silicon_id.as_deref().unwrap_or(""),
                one_line(s.reason.as_deref().unwrap_or(""))
            ));
        }
        match shown.len() - named.len() {
            0 => {}
            1 => lines.push(REDACTED.into()),
            _ => lines.push(REDACTED_MANY.into()),
        }
    }
    lines.push(format!("Wake and unlock this {word} to let a Silicon use it."));
    Some(Notification {
        title,
        body: lines.join("\n"),
        alert,
    })
}

/// A reason on one line (the service allows newlines; a notification row doesn't need them).
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// For a toast's XML.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 can't carry at all.
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\t' | '\r') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// For a freedesktop notification body, which servers may read as markup.
pub fn markup_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The toast's XML: plain text only, silent unless `alert`.
pub fn toast_xml(n: &Notification) -> String {
    let lines: String = n
        .body
        .lines()
        .map(|l| format!("<text>{}</text>", xml_escape(l)))
        .collect();
    let audio = if n.alert { "" } else { "<audio silent=\"true\"/>" };
    format!(
        "<toast scenario=\"reminder\"><visual><binding template=\"ToastGeneric\"><text>{}</text>{lines}</binding></visual>{audio}</toast>",
        xml_escape(&n.title)
    )
}

/// Why a notification couldn't be shown: a sentence for `wake_request_shown.note`.
pub type NotShown = String;

/// Puts the wake notification on screen. Calls may block (the agent runs them off its runtime).
pub trait Notifier: Send + Sync {
    /// Shows `n`, replacing (and removing from the history) whatever this notifier showed before.
    fn show(&self, n: &Notification) -> Result<(), NotShown>;
    /// Takes the notification down, from the screen and from the history.
    fn clear(&self);
}

/// No notifications (tests, and hosts with no way to show them).
pub struct Silent(pub String);

impl Notifier for Silent {
    fn show(&self, _n: &Notification) -> Result<(), NotShown> {
        Err(self.0.clone())
    }
    fn clear(&self) {}
}

/// Records what would be on screen (tests).
#[derive(Default)]
pub struct Recorder {
    pub shown: std::sync::Mutex<Vec<Option<Notification>>>,
}

impl Recorder {
    /// What is on screen now.
    pub fn current(&self) -> Option<Notification> {
        self.shown.lock().unwrap().last().cloned().flatten()
    }
}

impl Notifier for Recorder {
    fn show(&self, n: &Notification) -> Result<(), NotShown> {
        self.shown.lock().unwrap().push(Some(n.clone()));
        Ok(())
    }
    fn clear(&self) {
        let mut s = self.shown.lock().unwrap();
        if s.last().is_some_and(Option::is_some) {
            s.push(None);
        }
    }
}

/// The notifier for this computer.
pub fn for_this_computer() -> std::sync::Arc<dyn Notifier> {
    platform::notifier()
}

#[cfg(target_os = "macos")]
mod platform {
    //! `UNUserNotificationCenter`, inside the app bundle only.

    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotificationRequest,
        UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    };

    use super::{NotShown, Notification, Notifier, Silent};

    /// The one request id: showing again replaces it.
    const ID: &str = "extend.wake";
    const OFF: &str = "Notifications are off for Silicon Extend on this Mac.";

    pub fn notifier() -> Arc<dyn Notifier> {
        // The notification centre raises an Objective-C exception (and the process aborts) when
        // the process isn't an app bundle with an identifier, as `extend-agent` run from a
        // terminal or from `cargo test` isn't.
        let bundled = NSBundle::mainBundle().bundleIdentifier().is_some()
            && NSBundle::mainBundle().bundlePath().to_string().ends_with(".app");
        if !bundled {
            return Arc::new(Silent(
                "This Mac runs Silicon Extend outside its app, which can't show notifications.".into(),
            ));
        }
        let me = MacNotifier;
        // Asked once at start; macOS shows its prompt only the first time.
        me.authorize();
        Arc::new(me)
    }

    struct MacNotifier;

    impl MacNotifier {
        fn center() -> objc2::rc::Retained<UNUserNotificationCenter> {
            UNUserNotificationCenter::currentNotificationCenter()
        }

        fn authorize(&self) {
            let block = RcBlock::new(|granted: Bool, _err: *mut NSError| {
                tracing::info!("notifications allowed: {}", granted.as_bool());
            });
            Self::center().requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &block,
            );
        }

        fn authorized(&self) -> Option<bool> {
            let (tx, rx) = mpsc::channel();
            let block = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
                // SAFETY: the centre hands a live settings object to its completion handler.
                let status = unsafe { settings.as_ref() }.authorizationStatus();
                let _ = tx.send(status);
            });
            Self::center().getNotificationSettingsWithCompletionHandler(&block);
            let status = rx.recv_timeout(Duration::from_secs(3)).ok()?;
            Some(status != UNAuthorizationStatus::Denied && status != UNAuthorizationStatus::NotDetermined)
        }
    }

    impl Notifier for MacNotifier {
        fn show(&self, n: &Notification) -> Result<(), NotShown> {
            if self.authorized() == Some(false) {
                return Err(OFF.into());
            }
            let center = Self::center();
            // Redaction: the old text goes from Notification Center before the new one arrives.
            let ids = NSArray::from_retained_slice(&[NSString::from_str(ID)]);
            center.removeDeliveredNotificationsWithIdentifiers(&ids);
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&n.title));
            content.setBody(&NSString::from_str(&n.body));
            if n.alert {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let request =
                UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(ID), &content, None);
            let (tx, rx) = mpsc::channel();
            let block = RcBlock::new(move |err: *mut NSError| {
                let _ = tx.send(err.is_null());
            });
            center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(true) | Err(_) => Ok(()),
                Ok(false) => Err(OFF.into()),
            }
        }

        fn clear(&self) {
            let ids = NSArray::from_retained_slice(&[NSString::from_str(ID)]);
            let center = Self::center();
            center.removePendingNotificationRequestsWithIdentifiers(&ids);
            center.removeDeliveredNotificationsWithIdentifiers(&ids);
        }
    }
}

#[cfg(windows)]
mod platform {
    //! A WinRT toast under the app's AUMID (registered in `autostart.rs`).

    use std::sync::Arc;

    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{NotificationSetting, ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;

    use super::{NotShown, Notification, Notifier, toast_xml};

    const TAG: &str = "wake";
    const GROUP: &str = "extend";
    const OFF: &str = "Notifications are off for Silicon Extend on this computer.";

    pub fn notifier() -> Arc<dyn Notifier> {
        if let Err(e) = crate::autostart::register_aumid() {
            tracing::warn!("couldn't register Silicon Extend for notifications: {e:#}");
        }
        Arc::new(Toasts)
    }

    struct Toasts;

    fn aumid() -> HSTRING {
        HSTRING::from(crate::autostart::AUMID)
    }

    impl Toasts {
        fn remove(&self) {
            if let Ok(history) = ToastNotificationManager::History() {
                let _ = history.RemoveGroupedTagWithId(&HSTRING::from(TAG), &HSTRING::from(GROUP), &aumid());
            }
        }
    }

    impl Notifier for Toasts {
        fn show(&self, n: &Notification) -> Result<(), NotShown> {
            let run = || -> windows::core::Result<Result<(), NotShown>> {
                let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid())?;
                if notifier.Setting()? != NotificationSetting::Enabled {
                    return Ok(Err(OFF.into()));
                }
                let doc = XmlDocument::new()?;
                doc.LoadXml(&HSTRING::from(toast_xml(n)))?;
                let toast = ToastNotification::CreateToastNotification(&doc)?;
                toast.SetTag(&HSTRING::from(TAG))?;
                toast.SetGroup(&HSTRING::from(GROUP))?;
                // Redaction: the old toast leaves the Action Center before the new one shows.
                self.remove();
                notifier.Show(&toast)?;
                Ok(Ok(()))
            };
            match run() {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("couldn't show the wake notification: {e}");
                    Err("Windows didn't show the notification.".into())
                }
            }
        }

        fn clear(&self) {
            self.remove();
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! Retractable notifications through `org.freedesktop.Notifications` (the tray build).

    use std::sync::{Arc, Mutex};

    #[cfg(feature = "tray")]
    use super::markup_escape;
    use super::{NotShown, Notification, Notifier};

    const NO_SERVICE: &str = "Silicon Extend couldn't show a wake notification that it can later remove. Use the Linux desktop build with a running D-Bus notification service; the request remains available in Extend.";

    pub fn notifier() -> Arc<dyn Notifier> {
        Arc::new(Freedesktop { last_id: Mutex::new(0) })
    }

    struct Freedesktop {
        /// The id the server gave the notification on screen (0: none), for `replaces_id`.
        last_id: Mutex<u32>,
    }

    #[cfg(feature = "tray")]
    fn notify_dbus(replaces: u32, n: &Notification) -> Result<u32, String> {
        use std::collections::HashMap;
        use std::time::Duration;

        use dbus::arg::{RefArg, Variant};
        use dbus::blocking::Connection;

        let conn = Connection::new_session().map_err(|e| e.to_string())?;
        let proxy = conn.with_proxy(
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            Duration::from_secs(3),
        );
        let mut hints: HashMap<&str, Variant<Box<dyn RefArg>>> = HashMap::new();
        hints.insert("suppress-sound", Variant(Box::new(!n.alert)));
        hints.insert("urgency", Variant(Box::new(1u8)));
        hints.insert("category", Variant(Box::new("device".to_owned())));
        let (id,): (u32,) = proxy
            .method_call(
                "org.freedesktop.Notifications",
                "Notify",
                (
                    "Silicon Extend",
                    replaces,
                    "",
                    markup_escape(&n.title),
                    markup_escape(&n.body),
                    Vec::<&str>::new(),
                    hints,
                    0i32,
                ),
            )
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    #[cfg(not(feature = "tray"))]
    fn notify_dbus(_replaces: u32, _n: &Notification) -> Result<u32, String> {
        Err("this build has no D-Bus".into())
    }

    #[cfg(feature = "tray")]
    fn close_dbus(id: u32) {
        use std::time::Duration;
        if let Ok(conn) = dbus::blocking::Connection::new_session() {
            let proxy = conn.with_proxy(
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                Duration::from_secs(3),
            );
            let _: Result<(), _> = proxy.method_call("org.freedesktop.Notifications", "CloseNotification", (id,));
        }
    }

    #[cfg(not(feature = "tray"))]
    fn close_dbus(_id: u32) {}

    impl Notifier for Freedesktop {
        fn show(&self, n: &Notification) -> Result<(), NotShown> {
            let mut last = self.last_id.lock().unwrap();
            match notify_dbus(*last, n) {
                Ok(id) => {
                    *last = id;
                    Ok(())
                }
                Err(e) => {
                    // A fallback without a notification id cannot replace private text when
                    // another side starts using the device, or withdraw an ended request.
                    tracing::warn!("couldn't show a retractable wake notification: {e}");
                    Err(NO_SERVICE.to_owned())
                }
            }
        }

        fn clear(&self) {
            let mut last = self.last_id.lock().unwrap();
            if *last != 0 {
                close_dbus(*last);
                *last = 0;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        use super::*;

        /// Isolate the real backend in a child process: neither D-Bus nor a fallback command
        /// can contact the user's desktop. The fake CLI would succeed and record any invocation.
        #[test]
        fn unavailable_backend_never_posts_an_untrackable_notification() {
            const CHILD: &str = "EXTEND_NOTIFY_BACKEND_TEST_CHILD";
            const MARKER: &str = "EXTEND_NOTIFY_BACKEND_TEST_MARKER";
            if std::env::var_os(CHILD).is_some() {
                let notifier = notifier();
                for (title, body) in [
                    ("si:private asks to use this computer", "another side's private reason"),
                    ("A Silicon asks to use this computer", super::super::REDACTED),
                ] {
                    let result = notifier.show(&Notification {
                        title: title.into(),
                        body: body.into(),
                        alert: false,
                    });
                    assert_eq!(result, Err(NO_SERVICE.to_owned()));
                    notifier.clear();
                }
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let program = dir.path().join("notify-send");
            std::fs::write(
                &program,
                "#!/bin/sh\n: > \"$EXTEND_NOTIFY_BACKEND_TEST_MARKER\"\nexit 0\n",
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            let marker = dir.path().join("cli-was-called");
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "notify::platform::tests::unavailable_backend_never_posts_an_untrackable_notification",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env(MARKER, &marker)
                .env("PATH", dir.path())
                .env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    format!("unix:path={}/no-session-bus", dir.path().display()),
                )
                .output()
                .unwrap();
            assert!(
                !marker.exists(),
                "the unavailable backend invoked an untrackable notification CLI"
            );
            assert!(
                output.status.success(),
                "isolated backend check failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    pub fn notifier() -> std::sync::Arc<dyn super::Notifier> {
        std::sync::Arc::new(super::Silent("This computer can't show notifications.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn entry(n: u128, pair: &str, who: Option<&str>, side: Option<&str>) -> WakeEntry {
        WakeEntry {
            wake_id: Uuid::from_u128(n),
            pair: pair.parse().unwrap(),
            silicon_id: who.map(str::to_owned),
            reason: who.map(|w| format!("{w} needs the order screen")),
            side: side.map(str::to_owned),
            alert: false,
            created_at: datetime!(2026-09-27 10:00 UTC) + time::Duration::minutes(n as i64),
            expires_at: datetime!(2026-09-27 10:30 UTC) + time::Duration::minutes(n as i64),
        }
    }

    #[test]
    fn one_request_names_the_silicon_and_its_reason() {
        let mut book = WakeBook::default();
        assert!(book.upsert(entry(1, "7c1e09ab", Some("si:chef"), Some("sideA"))));
        assert!(!book.upsert(entry(1, "7c1e09ab", Some("si:chef"), Some("sideA"))));
        let n = notification(&book.visible(None), "Mac", true).unwrap();
        assert_eq!(n.title, "si:chef asks to use this Mac");
        assert_eq!(
            n.body,
            "si:chef needs the order screen\nWake and unlock this Mac to let a Silicon use it."
        );
        assert!(n.alert);
        assert!(notification(&[], "Mac", false).is_none());
    }

    #[test]
    fn several_requests_collapse_into_one_notification() {
        let mut book = WakeBook::default();
        book.upsert(entry(1, "7c1e09ab", Some("si:chef"), Some("sideA")));
        book.upsert(entry(2, "0d44e1f2", Some("si:scout"), Some("sideB")));
        let n = notification(&book.visible(None), "computer", false).unwrap();
        assert_eq!(n.title, "si:chef and 1 more ask to use this computer");
        assert_eq!(
            n.body,
            "si:chef: si:chef needs the order screen\nsi:scout: si:scout needs the order screen\nWake and unlock this computer to let a Silicon use it."
        );
    }

    #[test]
    fn another_sides_request_is_redacted_while_a_session_runs() {
        let mut book = WakeBook::default();
        book.upsert(entry(1, "7c1e09ab", Some("si:chef"), Some("sideA")));
        book.upsert(entry(2, "0d44e1f2", Some("si:scout"), Some("sideB")));
        book.upsert(entry(3, "0d44e1f2", Some("si:sous"), None));
        // A session on side A: B's request and the one without a side lose their names.
        let shown = book.visible(Some("sideA"));
        assert_eq!(shown[0].silicon_id.as_deref(), Some("si:chef"));
        assert!(shown[1].redacted() && shown[1].reason.is_none());
        assert!(shown[2].redacted());
        let n = notification(&shown, "Mac", false).unwrap();
        assert_eq!(n.title, "si:chef and 2 more ask to use this Mac");
        assert!(!n.body.contains("scout") && !n.body.contains("sous"), "{}", n.body);
        assert!(n.body.contains(REDACTED_MANY), "{}", n.body);
        // Only other sides': nobody is named anywhere.
        let n = notification(&book.visible(Some("sideC")), "Mac", false).unwrap();
        assert_eq!(n.title, "3 Silicons ask to use this Mac");
        assert!(!n.body.contains("si:"), "{}", n.body);
        let one = {
            let mut b = WakeBook::default();
            b.upsert(entry(2, "0d44e1f2", Some("si:scout"), Some("sideB")));
            notification(&b.visible(Some("sideA")), "Mac", false).unwrap()
        };
        assert_eq!(one.title, "A Silicon asks to use this Mac");
        assert_eq!(
            one.body,
            format!("{REDACTED}\nWake and unlock this Mac to let a Silicon use it.")
        );
    }

    #[test]
    fn a_redacted_frame_replaces_what_was_held() {
        let mut book = WakeBook::default();
        book.upsert(entry(1, "7c1e09ab", Some("si:chef"), Some("sideA")));
        let mut redacted = entry(1, "7c1e09ab", None, Some("sideA"));
        redacted.reason = None;
        assert!(book.upsert(redacted));
        assert!(book.visible(None)[0].redacted());
    }

    #[test]
    fn requests_go_on_end_expiry_awake_and_pair_end() {
        let mut book = WakeBook::default();
        book.upsert(entry(1, "7c1e09ab", Some("si:chef"), None));
        book.upsert(entry(2, "0d44e1f2", Some("si:scout"), None));
        book.upsert(entry(3, "0d44e1f2", Some("si:sous"), None));
        assert!(book.remove(&Uuid::from_u128(1)));
        assert!(!book.remove(&Uuid::from_u128(1)));
        assert!(book.expire(datetime!(2026-09-27 10:32:30 UTC)));
        assert_eq!(book.visible(None).len(), 1);
        assert!(book.remove_pair(&"0d44e1f2".parse().unwrap()));
        assert!(book.is_empty());
        book.upsert(entry(4, "7c1e09ab", Some("si:chef"), None));
        assert!(book.clear());
        assert!(!book.clear());
    }

    #[test]
    fn shown_is_answered_once_per_request() {
        let mut book = WakeBook::default();
        assert!(book.first_answer(Uuid::from_u128(1)));
        assert!(!book.first_answer(Uuid::from_u128(1)));
        assert!(book.first_answer(Uuid::from_u128(2)));
    }

    #[test]
    fn text_is_escaped_for_each_backend() {
        let n = Notification {
            title: "si:chef asks to use this computer".into(),
            body: "<b>Fix</b> the \"build\" & 'deploy'\u{7}".into(),
            alert: false,
        };
        let xml = toast_xml(&n);
        assert!(
            xml.contains("&lt;b&gt;Fix&lt;/b&gt; the &quot;build&quot; &amp; &apos;deploy&apos; "),
            "{xml}"
        );
        assert!(xml.contains("<audio silent=\"true\"/>"), "{xml}");
        assert!(
            !toast_xml(&Notification {
                alert: true,
                ..n.clone()
            })
            .contains("audio")
        );
        assert_eq!(markup_escape("<i>a</i> & b"), "&lt;i&gt;a&lt;/i&gt; &amp; b");
        // Newlines in a reason don't break the notification's lines.
        assert_eq!(one_line("line one\nline   two"), "line one line two");
    }

    #[test]
    fn the_recorder_keeps_what_is_on_screen() {
        let r = Recorder::default();
        assert!(r.current().is_none());
        let n = Notification {
            title: "t".into(),
            body: "b".into(),
            alert: false,
        };
        r.show(&n).unwrap();
        assert_eq!(r.current(), Some(n));
        r.clear();
        assert!(r.current().is_none());
    }
}
