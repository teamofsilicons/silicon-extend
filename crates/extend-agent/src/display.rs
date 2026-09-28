//! Keeping the display on while a Silicon uses this computer (`UNDERSTANDING.md`, "Waking a
//! device"): an awake computer doesn't turn its screen off on its own in the middle of a Silicon's
//! task. It never turns a screen on, never wakes a sleeping computer, and the Carbon can still
//! lock it at any time.
//!
//! The agent holds it only while this computer's own session runs, no takeover is paused on the
//! Carbon, and the computer is awake; it releases it at the session's end (idle end and Stop
//! included), at a takeover, and when the computer locks or sleeps.
//!
//! * Mac: an IOKit `PreventUserIdleDisplaySleep` power assertion.
//! * Windows: `SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED)`. The state belongs
//!   to the thread that set it, so one long-lived thread owns it, driven through a channel.
//! * Linux: `org.freedesktop.ScreenSaver.Inhibit` over a D-Bus connection kept open for as long
//!   as it is held (the desktop drops an inhibit whose connection closes), falling back to the
//!   desktop portal's `Inhibit` (idle). Without either, nothing is held and that is logged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Keeps the display from turning off by itself, or lets it again. Idempotent.
pub trait DisplayKeeper: Send + Sync {
    fn hold(&self, on: bool);
}

/// Holds nothing (tests, headless).
pub struct NoKeeper;

impl DisplayKeeper for NoKeeper {
    fn hold(&self, _on: bool) {}
}

/// Records every change (tests).
#[derive(Default)]
pub struct Recorder {
    pub held: AtomicBool,
    pub changes: std::sync::Mutex<Vec<bool>>,
}

impl DisplayKeeper for Recorder {
    fn hold(&self, on: bool) {
        if self.held.swap(on, Ordering::SeqCst) != on {
            self.changes.lock().unwrap().push(on);
        }
    }
}

/// Whether the display should be held: this computer's own session runs, nothing is paused on
/// the Carbon, and the computer is awake.
pub fn wanted(own_session: bool, takeover: bool, awake: bool) -> bool {
    own_session && !takeover && awake
}

/// The keeper for this computer.
pub fn for_this_computer() -> Arc<dyn DisplayKeeper> {
    platform::keeper()
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    use super::DisplayKeeper;

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const LEVEL_ON: u32 = 255; // kIOPMAssertionLevelOn

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const std::ffi::c_char, encoding: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub fn keeper() -> Arc<dyn DisplayKeeper> {
        Arc::new(Assertion(Mutex::new(None)))
    }

    /// The live assertion's id.
    struct Assertion(Mutex<Option<u32>>);

    impl DisplayKeeper for Assertion {
        fn hold(&self, on: bool) {
            let mut id = self.0.lock().unwrap();
            match (on, *id) {
                (true, None) => {
                    let mut new = 0u32;
                    // SAFETY: plain IOKit calls with CF strings released after use.
                    let r = unsafe {
                        let kind =
                            CFStringCreateWithCString(std::ptr::null(), c"PreventUserIdleDisplaySleep".as_ptr(), UTF8);
                        let name = CFStringCreateWithCString(
                            std::ptr::null(),
                            c"Silicon Extend: a Silicon is using this Mac".as_ptr(),
                            UTF8,
                        );
                        let r = IOPMAssertionCreateWithName(kind, LEVEL_ON, name, &mut new);
                        CFRelease(kind);
                        CFRelease(name);
                        r
                    };
                    if r == 0 {
                        *id = Some(new);
                        tracing::info!("keeping the display on while a Silicon uses this Mac");
                    } else {
                        tracing::warn!("couldn't keep the display on (IOKit {r:#x})");
                    }
                }
                (false, Some(old)) => {
                    // SAFETY: releasing the assertion this keeper created.
                    unsafe { IOPMAssertionRelease(old) };
                    *id = None;
                    tracing::info!("the display may turn off on its own again");
                }
                _ => {}
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::Arc;
    use std::sync::mpsc::{Sender, channel};

    use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState};

    use super::DisplayKeeper;

    pub fn keeper() -> Arc<dyn DisplayKeeper> {
        let (tx, rx) = channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("extend-display".into())
            .spawn(move || {
                let mut held = false;
                while let Ok(on) = rx.recv() {
                    if on == held {
                        continue;
                    }
                    // SAFETY: sets this thread's own execution state.
                    let prev = unsafe {
                        if on {
                            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED)
                        } else {
                            SetThreadExecutionState(ES_CONTINUOUS)
                        }
                    };
                    if prev.0 == 0 {
                        tracing::warn!("Windows didn't take the display setting");
                    } else {
                        held = on;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("couldn't start the display thread: {e}");
        }
        Arc::new(Thread(std::sync::Mutex::new(tx)))
    }

    struct Thread(std::sync::Mutex<Sender<bool>>);

    impl DisplayKeeper for Thread {
        fn hold(&self, on: bool) {
            let _ = self.0.lock().unwrap().send(on);
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Arc;
    use std::sync::mpsc::{Sender, channel};

    use super::DisplayKeeper;

    pub fn keeper() -> Arc<dyn DisplayKeeper> {
        let (tx, rx) = channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("extend-display".into())
            .spawn(move || {
                let mut inhibit: Option<imp::Inhibit> = None;
                while let Ok(on) = rx.recv() {
                    match (on, inhibit.is_some()) {
                        (true, false) => inhibit = imp::inhibit(),
                        (false, true) => {
                            if let Some(i) = inhibit.take() {
                                i.release();
                            }
                        }
                        _ => {}
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("couldn't start the display thread: {e}");
        }
        Arc::new(Thread(std::sync::Mutex::new(tx)))
    }

    struct Thread(std::sync::Mutex<Sender<bool>>);

    impl DisplayKeeper for Thread {
        fn hold(&self, on: bool) {
            let _ = self.0.lock().unwrap().send(on);
        }
    }

    #[cfg(feature = "tray")]
    mod imp {
        use std::collections::HashMap;
        use std::time::Duration;

        use dbus::Path;
        use dbus::arg::{RefArg, Variant};
        use dbus::blocking::Connection;

        const APP: &str = "Silicon Extend";
        const WHY: &str = "A Silicon is using this computer";
        const TIMEOUT: Duration = Duration::from_secs(3);

        /// A held inhibit: the connection stays open until it is released.
        pub struct Inhibit {
            conn: Connection,
            how: How,
        }

        enum How {
            ScreenSaver { path: &'static str, cookie: u32 },
            Portal(Path<'static>),
        }

        pub fn inhibit() -> Option<Inhibit> {
            let conn = match Connection::new_session() {
                Ok(c) => c,
                Err(e) => {
                    tracing::info!("no D-Bus session ({e}); the display may turn off during a session");
                    return None;
                }
            };
            for path in ["/org/freedesktop/ScreenSaver", "/ScreenSaver"] {
                let proxy = conn.with_proxy("org.freedesktop.ScreenSaver", path, TIMEOUT);
                let r: Result<(u32,), _> = proxy.method_call("org.freedesktop.ScreenSaver", "Inhibit", (APP, WHY));
                if let Ok((cookie,)) = r {
                    tracing::info!("keeping the display on while a Silicon uses this computer");
                    return Some(Inhibit {
                        conn,
                        how: How::ScreenSaver { path, cookie },
                    });
                }
            }
            // The desktop portal (Flatpak-era desktops, some Wayland compositors): flags 8 = idle.
            let proxy = conn.with_proxy(
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                TIMEOUT,
            );
            let mut options: HashMap<&str, Variant<Box<dyn RefArg>>> = HashMap::new();
            options.insert("reason", Variant(Box::new(WHY.to_owned())));
            let r: Result<(Path<'static>,), _> =
                proxy.method_call("org.freedesktop.portal.Inhibit", "Inhibit", ("", 8u32, options));
            match r {
                Ok((handle,)) => {
                    tracing::info!("keeping the display on (desktop portal) while a Silicon uses this computer");
                    Some(Inhibit {
                        conn,
                        how: How::Portal(handle),
                    })
                }
                Err(e) => {
                    tracing::info!("this desktop offers no way to keep the display on ({e})");
                    None
                }
            }
        }

        impl Inhibit {
            pub fn release(self) {
                match &self.how {
                    How::ScreenSaver { path, cookie } => {
                        let proxy = self.conn.with_proxy("org.freedesktop.ScreenSaver", *path, TIMEOUT);
                        let _: Result<(), _> =
                            proxy.method_call("org.freedesktop.ScreenSaver", "UnInhibit", (*cookie,));
                    }
                    How::Portal(handle) => {
                        let proxy = self
                            .conn
                            .with_proxy("org.freedesktop.portal.Desktop", handle.clone(), TIMEOUT);
                        let _: Result<(), _> = proxy.method_call("org.freedesktop.portal.Request", "Close", ());
                    }
                }
                tracing::info!("the display may turn off on its own again");
                // Dropping the connection ends the inhibit even when the call above failed.
            }
        }
    }

    #[cfg(not(feature = "tray"))]
    mod imp {
        pub struct Inhibit;
        pub fn inhibit() -> Option<Inhibit> {
            None
        }
        impl Inhibit {
            pub fn release(self) {}
        }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    pub fn keeper() -> std::sync::Arc<dyn super::DisplayKeeper> {
        std::sync::Arc::new(super::NoKeeper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_only_for_an_own_awake_session_without_a_takeover() {
        assert!(wanted(true, false, true));
        assert!(!wanted(false, false, true));
        assert!(!wanted(true, true, true));
        assert!(!wanted(true, false, false));
    }

    #[test]
    fn the_recorder_notes_changes_only() {
        let r = Recorder::default();
        r.hold(true);
        r.hold(true);
        r.hold(false);
        r.hold(false);
        assert_eq!(*r.changes.lock().unwrap(), vec![true, false]);
    }
}
