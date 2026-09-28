//! Whether this computer's screen can be used right now, and whether the computer is awake. A
//! Silicon can't see or use a computer that is locked, and a Mac also not while it is asleep
//! (`UNDERSTANDING.md`, "Good to know"). Only its Carbon can unlock or wake it: Extend never does.
//!
//! * Mac: the login session's dictionary (`CGSessionCopyCurrentDictionary`) says whether the
//!   screen is locked (`CGSSessionScreenIsLocked`) and whether this session is the one on screen
//!   (`kCGSSessionOnConsoleKey`: not while another account or the login window is showing); an
//!   attached main display that is asleep (`CGDisplayIsAsleep`) counts as asleep. The time since
//!   the last keyboard, mouse or trackpad input comes from `CGEventSourceSecondsSinceLastEventType`.
//! * Linux: logind's `LockedHint` for this graphical session (`loginctl show-session`), which
//!   GNOME, KDE and other desktops set while their lock screen is up, and `Active` (no: another
//!   account is on screen). `IdleHint` says whether the Carbon has been using it.
//! * Windows: the session's lock state and whether it is the one on screen
//!   (`WTSQuerySessionInformationW`), then whether the input desktop is the user's: when it isn't
//!   and the session isn't locked, an admin prompt's secure desktop is up. That computer is awake
//!   (the Carbon is at it); only the screen can't be used. `GetLastInputInfo` gives the last input.
//!
//! The platform probes report the capabilities that need the screen as missing with the reason,
//! and the agent reads [`read`] every few seconds so a lock or unlock reaches Extend at once (a
//! new `hello` and an `awake` frame), not at the next periodic check.

use std::time::Duration;

use extend_protocol::Capability;
use extend_protocol::model::{MissingCapability, SleepState};

/// Why the screen can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenBlock {
    /// The lock screen is up.
    Locked,
    /// Another account, or the login window, is on screen (fast user switching).
    OtherSession,
    /// The display is asleep (a Mac).
    Asleep,
    /// An admin prompt's secure desktop is in front (Windows). The computer is awake: the
    /// Carbon is at it.
    AdminPrompt,
}

impl ScreenBlock {
    /// What happened, why it matters and what to do, for `missing[].reason`. A Silicon reads it:
    /// it can't unlock or wake the computer, and Extend never does; it can ask its Carbon.
    pub fn reason(self) -> String {
        let word = crate::sysinfo::computer_word();
        match self {
            Self::Locked => format!(
                "This {word} is locked, so a Silicon can't see or use its screen. Only its Carbon can unlock it; ask with `extend device wake`. The terminal still works."
            ),
            Self::OtherSession => format!(
                "This {word} is showing the login window or another account, so a Silicon can't see or use its screen. Only its Carbon can switch back to this account; ask with `extend device wake`. The terminal still works."
            ),
            Self::Asleep => format!(
                "This {word} is asleep (its display is off), so a Silicon can't see or use its screen. Only its Carbon can wake it; ask with `extend device wake`. The terminal still works."
            ),
            Self::AdminPrompt => format!(
                "An admin prompt is in front on this {word}, so a Silicon can't see or use its screen. Admin prompts always need its Carbon. The terminal still works."
            ),
        }
    }

    /// How `awake` reports it: `None` when the computer counts as awake (an admin prompt).
    pub fn sleep_state(self) -> Option<SleepState> {
        match self {
            Self::Locked => Some(SleepState::Locked),
            Self::OtherSession => Some(SleepState::OtherSession),
            Self::Asleep => Some(SleepState::Asleep),
            Self::AdminPrompt => None,
        }
    }
}

/// One look at the computer: what blocks its screen, and how long since the last real input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenReading {
    pub block: Option<ScreenBlock>,
    /// Time since the last keyboard, mouse or touch input, when the OS says.
    pub input_idle: Option<Duration>,
}

impl ScreenReading {
    /// A computer whose screen is usable, with no word on input (tests, headless).
    pub fn usable() -> Self {
        Self::default()
    }
    pub fn blocked(block: ScreenBlock) -> Self {
        Self {
            block: Some(block),
            input_idle: None,
        }
    }
}

/// What still works while the screen can't be used: nothing here needs to see or touch it.
pub const WORKS_WITHOUT_SCREEN: &[Capability] = &[
    Capability::Terminal,
    Capability::Takeover,
    Capability::AppsList,
    Capability::Logs,
];

/// Moves every capability that needs the screen from `caps` to `missing`, with `block`'s reason.
/// A capability already missing keeps its own reason (it won't work after unlocking either).
pub fn withhold(block: ScreenBlock, caps: &mut Vec<Capability>, missing: &mut Vec<MissingCapability>) {
    let reason = block.reason();
    caps.retain(|c| {
        if WORKS_WITHOUT_SCREEN.contains(c) {
            return true;
        }
        if !missing.iter().any(|m| m.capability == *c) {
            missing.push(MissingCapability {
                capability: *c,
                reason: reason.clone(),
            });
        }
        false
    });
}

/// A Mac's state, from its session dictionary and main display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MacSession {
    /// `CGSSessionScreenIsLocked` is set.
    pub screen_locked: bool,
    /// `kCGSSessionOnConsoleKey`, when the dictionary has it.
    pub on_console: Option<bool>,
    /// The main display is attached and asleep.
    pub display_asleep: bool,
}

pub fn mac_block(s: MacSession) -> Option<ScreenBlock> {
    if s.screen_locked {
        Some(ScreenBlock::Locked)
    } else if s.on_console == Some(false) {
        Some(ScreenBlock::OtherSession)
    } else if s.display_asleep {
        Some(ScreenBlock::Asleep)
    } else {
        None
    }
}

/// Reads `loginctl show-session <id> -p LockedHint -p Active …` (`Name=value` lines).
pub fn loginctl_block(output: &str) -> Option<ScreenBlock> {
    let is = |key: &str, value: &str| {
        output
            .lines()
            .filter_map(|l| l.trim().split_once('='))
            .any(|(k, v)| k == key && v.trim() == value)
    };
    if is("LockedHint", "yes") {
        Some(ScreenBlock::Locked)
    } else if is("Active", "no") {
        Some(ScreenBlock::OtherSession)
    } else {
        None
    }
}

/// `IdleHint` from the same output: `no` means the Carbon is using the computer.
pub fn loginctl_input_idle(output: &str) -> Option<Duration> {
    output
        .lines()
        .filter_map(|l| l.trim().split_once('='))
        .find(|(k, _)| *k == "IdleHint")
        .and_then(|(_, v)| match v.trim() {
            "no" => Some(Duration::ZERO),
            "yes" => Some(Duration::MAX),
            _ => None,
        })
}

/// A Windows session's state, from `WTSQuerySessionInformationW` and the input desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowsSession {
    /// `WTSConnectState` is `WTSActive`: this session is the one on screen.
    pub active: Option<bool>,
    /// `WTSSessionInfoEx` says locked (`WTS_SESSIONSTATE_LOCK`), unlocked, or can't say.
    pub locked: Option<bool>,
    /// The input desktop isn't this user's (the lock screen, or an admin prompt).
    pub input_desktop_foreign: bool,
}

pub fn windows_block(s: WindowsSession) -> Option<ScreenBlock> {
    if s.active == Some(false) {
        Some(ScreenBlock::OtherSession)
    } else if s.locked == Some(true) {
        Some(ScreenBlock::Locked)
    } else if s.input_desktop_foreign {
        // Unlocked by its own account, yet the input desktop isn't the user's: the secure desktop
        // of an admin prompt. Without a lock state (an older Windows), count it as locked.
        if s.locked == Some(false) {
            Some(ScreenBlock::AdminPrompt)
        } else {
            Some(ScreenBlock::Locked)
        }
    } else {
        None
    }
}

/// The logind session to ask about: `XDG_SESSION_ID` when the agent runs inside the session,
/// else the user's graphical session from `loginctl show-user <uid> -p Display --value`.
pub fn session_id(xdg_session_id: Option<&str>, show_user_display: Option<&str>) -> Option<String> {
    let valid = |s: &str| {
        let s = s.trim();
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then(|| s.to_owned())
    };
    xdg_session_id
        .and_then(valid)
        .or_else(|| show_user_display.and_then(valid))
}

/// Whether this computer's screen can be used right now (`None`: it can, or it can't be told).
pub fn current() -> Option<ScreenBlock> {
    platform::current()
}

/// [`current`], with the time since the last input.
pub fn read() -> ScreenReading {
    ScreenReading {
        block: platform::current(),
        input_idle: platform::input_idle(),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void};

    use super::{MacSession, ScreenBlock, mac_block};

    type CFTypeRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const SINT64: i64 = 4; // kCFNumberSInt64Type

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFBooleanGetValue(b: CFTypeRef) -> u8;
        fn CFNumberGetValue(n: CFTypeRef, kind: i64, out: *mut c_void) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsAsleep(display: u32) -> i32;
        fn CGDisplayIsOnline(display: u32) -> i32;
        fn CGEventSourceSecondsSinceLastEventType(state: i32, kind: u32) -> f64;
    }

    /// Seconds since the last HID input (`kCGEventSourceStateHIDSystemState`, any input type).
    pub fn input_idle() -> Option<std::time::Duration> {
        // SAFETY: a plain query.
        let secs = unsafe { CGEventSourceSecondsSinceLastEventType(1, u32::MAX) };
        (secs.is_finite() && secs >= 0.0).then(|| std::time::Duration::from_secs_f64(secs))
    }

    /// A boolean or number value in `dict`, as a bool.
    ///
    /// SAFETY: `dict` is a live CFDictionary.
    unsafe fn flag(dict: CFTypeRef, key: &std::ffi::CStr) -> Option<bool> {
        unsafe {
            let k = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8);
            if k.is_null() {
                return None;
            }
            let v = CFDictionaryGetValue(dict, k);
            CFRelease(k);
            if v.is_null() {
                return None;
            }
            let kind = CFGetTypeID(v);
            if kind == CFBooleanGetTypeID() {
                Some(CFBooleanGetValue(v) != 0)
            } else if kind == CFNumberGetTypeID() {
                let mut n: i64 = 0;
                (CFNumberGetValue(v, SINT64, (&mut n as *mut i64).cast()) != 0).then_some(n != 0)
            } else {
                None
            }
        }
    }

    pub fn current() -> Option<ScreenBlock> {
        // SAFETY: plain queries; the copied dictionary is released once read.
        let session = unsafe {
            let dict = CGSessionCopyCurrentDictionary();
            let mut s = MacSession::default();
            if !dict.is_null() {
                s.screen_locked = flag(dict, c"CGSSessionScreenIsLocked").unwrap_or(false);
                s.on_console = flag(dict, c"kCGSSessionOnConsoleKey");
                CFRelease(dict);
            }
            let main = CGMainDisplayID();
            s.display_asleep = CGDisplayIsOnline(main) != 0 && CGDisplayIsAsleep(main) != 0;
            s
        };
        mac_block(session)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::time::Duration;

    use super::{ScreenBlock, loginctl_block, loginctl_input_idle, session_id};
    use crate::drivers::probe_linux::capture;

    fn show_session() -> Option<String> {
        let loginctl = crate::config::which("loginctl")?;
        let timeout = Duration::from_secs(2);
        let from_env = std::env::var("XDG_SESSION_ID").ok();
        let display = if from_env.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            None
        } else {
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() }.to_string();
            capture(&loginctl, &["show-user", &uid, "-p", "Display", "--value"], timeout).ok()
        };
        let id = session_id(from_env.as_deref(), display.as_deref())?;
        capture(
            &loginctl,
            &[
                "show-session",
                &id,
                "-p",
                "LockedHint",
                "-p",
                "Active",
                "-p",
                "IdleHint",
            ],
            timeout,
        )
        .ok()
    }

    pub fn current() -> Option<ScreenBlock> {
        loginctl_block(&show_session()?)
    }

    pub fn input_idle() -> Option<Duration> {
        loginctl_input_idle(&show_session()?)
    }
}

#[cfg(windows)]
mod platform {
    use super::ScreenBlock;

    pub fn current() -> Option<ScreenBlock> {
        super::windows_block(crate::drivers::windows::session_state())
    }

    pub fn input_idle() -> Option<std::time::Duration> {
        crate::drivers::windows::input_idle()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    pub fn current() -> Option<super::ScreenBlock> {
        None
    }
    pub fn input_idle() -> Option<std::time::Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_session_states() {
        assert_eq!(mac_block(MacSession::default()), None);
        let on_screen = MacSession {
            on_console: Some(true),
            ..Default::default()
        };
        assert_eq!(mac_block(on_screen), None);
        assert_eq!(
            mac_block(MacSession {
                screen_locked: true,
                ..on_screen
            }),
            Some(ScreenBlock::Locked)
        );
        assert_eq!(
            mac_block(MacSession {
                on_console: Some(false),
                ..Default::default()
            }),
            Some(ScreenBlock::OtherSession)
        );
        assert_eq!(
            mac_block(MacSession {
                display_asleep: true,
                ..on_screen
            }),
            Some(ScreenBlock::Asleep)
        );
        // Locked wins: unlocking is what the Carbon has to do first.
        assert_eq!(
            mac_block(MacSession {
                screen_locked: true,
                on_console: Some(false),
                display_asleep: true
            }),
            Some(ScreenBlock::Locked)
        );
    }

    #[test]
    fn loginctl_output() {
        assert_eq!(loginctl_block("LockedHint=yes\n"), Some(ScreenBlock::Locked));
        assert_eq!(loginctl_block("LockedHint=no\n"), None);
        assert_eq!(
            loginctl_block("Active=yes\nLockedHint=yes\nType=x11\n"),
            Some(ScreenBlock::Locked)
        );
        // Another account is on screen: this session isn't active.
        assert_eq!(
            loginctl_block("LockedHint=no\nActive=no\nIdleHint=yes\n"),
            Some(ScreenBlock::OtherSession)
        );
        // An older logind without the property, or an error, says nothing.
        assert_eq!(loginctl_block(""), None);
        assert_eq!(loginctl_block("Failed to get session: No such session"), None);
        assert_eq!(loginctl_input_idle("IdleHint=no\n"), Some(Duration::ZERO));
        assert_eq!(loginctl_input_idle("LockedHint=no\nIdleHint=yes"), Some(Duration::MAX));
        assert_eq!(loginctl_input_idle("LockedHint=no"), None);
    }

    #[test]
    fn windows_sessions() {
        let at = |active, locked, foreign| {
            windows_block(WindowsSession {
                active,
                locked,
                input_desktop_foreign: foreign,
            })
        };
        assert_eq!(at(Some(true), Some(false), false), None);
        assert_eq!(at(Some(true), Some(true), true), Some(ScreenBlock::Locked));
        assert_eq!(at(Some(false), Some(false), true), Some(ScreenBlock::OtherSession));
        // A UAC prompt: unlocked, yet the input desktop is the secure one. Awake, screen blocked.
        let uac = at(Some(true), Some(false), true);
        assert_eq!(uac, Some(ScreenBlock::AdminPrompt));
        assert_eq!(uac.unwrap().sleep_state(), None);
        assert!(uac.unwrap().reason().contains("Admin prompts always need its Carbon"));
        // No lock state to go by (an older Windows): a foreign input desktop counts as locked.
        assert_eq!(at(None, None, true), Some(ScreenBlock::Locked));
    }

    #[test]
    fn which_logind_session() {
        assert_eq!(session_id(Some("2"), Some("7")), Some("2".into()));
        assert_eq!(session_id(Some(" "), Some("c3\n")), Some("c3".into()));
        assert_eq!(session_id(None, Some("")), None);
        assert_eq!(session_id(None, None), None);
        // Never handed on to loginctl unless it is a plain session id.
        assert_eq!(session_id(Some("2; rm -rf ~"), None), None);
    }

    #[test]
    fn a_blocked_screen_withholds_what_needs_it() {
        let mut caps = vec![
            Capability::ScreenRead,
            Capability::InputText,
            Capability::AppsLaunch,
            Capability::AppsList,
            Capability::Logs,
            Capability::Takeover,
            Capability::Terminal,
        ];
        let mut missing = vec![MissingCapability {
            capability: Capability::ScreenCapture,
            reason: "Allow Screen Recording".into(),
        }];
        withhold(ScreenBlock::Locked, &mut caps, &mut missing);
        assert_eq!(
            caps,
            vec![
                Capability::AppsList,
                Capability::Logs,
                Capability::Takeover,
                Capability::Terminal
            ]
        );
        let reason = |c| missing.iter().find(|m| m.capability == c).map(|m| m.reason.clone());
        let locked = ScreenBlock::Locked.reason();
        assert!(locked.contains("is locked") && locked.contains("unlock it"), "{locked}");
        for c in [Capability::ScreenRead, Capability::InputText, Capability::AppsLaunch] {
            assert_eq!(reason(c), Some(locked.clone()), "{c:?}");
        }
        // Already missing for its own reason: that reason stays.
        assert_eq!(
            reason(Capability::ScreenCapture).as_deref(),
            Some("Allow Screen Recording")
        );
    }

    /// Asks the real platform (read-only): it answers without crashing, whatever the answer.
    #[test]
    fn the_platform_answers() {
        let _ = current();
        let _ = read();
    }

    #[test]
    fn reasons_say_what_why_and_what_to_do() {
        for (block, act, state) in [
            (ScreenBlock::Locked, "Only its Carbon can unlock it", SleepState::Locked),
            (
                ScreenBlock::OtherSession,
                "Only its Carbon can switch back",
                SleepState::OtherSession,
            ),
            (ScreenBlock::Asleep, "Only its Carbon can wake it", SleepState::Asleep),
        ] {
            let r = block.reason();
            assert!(
                r.starts_with(&format!("This {} is ", crate::sysinfo::computer_word())),
                "{r}"
            );
            assert!(r.contains("can't see or use") && r.contains(act), "{r}");
            // The terminal keeps working whether or not the computer is awake.
            assert!(r.ends_with("The terminal still works."), "{r}");
            assert!(r.contains("`extend device wake`"), "{r}");
            assert_eq!(block.sleep_state(), Some(state));
        }
    }
}
