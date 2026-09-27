//! [`WindowsDriver`]: the `extend_driver::Driver` for a Windows computer.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use extend_driver::{Driver, Invocation, Output, Probe};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP, OpenInputDesktop, SwitchDesktop,
};

use super::commands;
use super::probe_logic::probe_from;
use super::worker::{self, Job, Request};

pub struct WindowsDriver {
    worker: Mutex<Sender<Request>>,
    #[allow(dead_code)]
    state_dir: PathBuf,
}

impl WindowsDriver {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            worker: Mutex::new(worker::spawn()),
            state_dir,
        }
    }

    fn send(&self, request: Request) -> bool {
        let tx = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if tx.send(request).is_ok() {
            return true;
        }
        false
    }
}

/// True when the input desktop isn't the user's (locked, or the secure desktop of an admin prompt).
pub(crate) fn input_desktop_foreign() -> bool {
    unsafe {
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) {
            Ok(desk) => {
                let usable = SwitchDesktop(desk).is_ok();
                let _ = CloseDesktop(desk);
                !usable
            }
            Err(_) => true,
        }
    }
}

/// This session's state: on screen or not, locked or not (`WTSQuerySessionInformationW`), and
/// whether the input desktop is the user's.
pub(crate) fn session_state() -> crate::drivers::screen_lock::WindowsSession {
    use windows::Win32::System::RemoteDesktop::{
        WTS_CONNECTSTATE_CLASS, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSActive, WTSConnectState,
        WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
    use windows::core::PWSTR;

    /// SAFETY: reads a buffer WTSQuerySessionInformationW allocated for `class`, then frees it.
    unsafe fn query<T: Copy>(class: windows::Win32::System::RemoteDesktop::WTS_INFO_CLASS) -> Option<T> {
        let mut buf = PWSTR::null();
        let mut len = 0u32;
        unsafe {
            WTSQuerySessionInformationW(
                Some(WTS_CURRENT_SERVER_HANDLE),
                WTS_CURRENT_SESSION,
                class,
                &mut buf,
                &mut len,
            )
            .ok()?;
            let value = (len as usize >= std::mem::size_of::<T>() && !buf.is_null())
                .then(|| std::ptr::read_unaligned(buf.0.cast::<T>()));
            WTSFreeMemory(buf.0.cast());
            value
        }
    }
    // WTS_SESSIONSTATE_LOCK / _UNLOCK in WTSINFOEX_LEVEL1_W.SessionFlags (Windows 8 and later).
    const LOCK: i32 = 0;
    const UNLOCK: i32 = 1;
    // SAFETY: plain queries of this process's own session.
    let (connect, info) = unsafe {
        (
            query::<WTS_CONNECTSTATE_CLASS>(WTSConnectState),
            query::<WTSINFOEXW>(WTSSessionInfoEx),
        )
    };
    let locked = info.filter(|i| i.Level == 1).and_then(|i| {
        // SAFETY: Level 1 means the WTSInfoExLevel1 member is the one filled in.
        match unsafe { i.Data.WTSInfoExLevel1.SessionFlags } {
            LOCK => Some(true),
            UNLOCK => Some(false),
            _ => None,
        }
    });
    crate::drivers::screen_lock::WindowsSession {
        active: connect.map(|c| c == WTSActive),
        locked,
        input_desktop_foreign: input_desktop_foreign(),
    }
}

/// Time since the last keyboard or mouse input in this session (`GetLastInputInfo`).
pub(crate) fn input_idle() -> Option<std::time::Duration> {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: fills a correctly sized struct.
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return None;
        }
        Some(std::time::Duration::from_millis(u64::from(
            GetTickCount().wrapping_sub(info.dwTime),
        )))
    }
}

fn os_version() -> Option<String> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION.get_or_init(super::shell::ver).clone()
}

#[async_trait]
impl Driver for WindowsDriver {
    async fn probe(&self) -> Probe {
        tokio::task::spawn_blocking(|| {
            probe_from(
                crate::drivers::screen_lock::windows_block(session_state()),
                os_version(),
            )
        })
        .await
        .unwrap_or_else(|_| probe_from(None, None))
    }

    async fn run(&self, inv: Invocation<'_>) -> Output {
        let action = match commands::parse(inv.command, inv.args) {
            Ok(a) => a,
            Err(r) => return Output::fail(r.code, r.message),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (reply, rx) = tokio::sync::oneshot::channel();
        let job = Job {
            session: inv.session_id.to_owned(),
            action,
            workdir: inv.workdir.to_path_buf(),
            cancel: cancel.clone(),
            deadline: Instant::now() + inv.timeout,
            reply,
        };
        if !self.send(Request::Run(job)) {
            return Output::fail("internal", "The Windows driver stopped. Restart Silicon Extend.");
        }
        tokio::select! {
            out = rx => out.unwrap_or_else(|_| Output::fail("internal", "The Windows driver stopped while running the command.")),
            _ = tokio::time::sleep(inv.timeout) => {
                cancel.store(true, Ordering::Relaxed);
                Output::fail("command_timeout", format!("{} didn't finish within {} ms.", inv.command, inv.timeout.as_millis()))
            }
            _ = inv.cancel.cancelled() => {
                cancel.store(true, Ordering::Relaxed);
                Output::fail("cancelled", format!("{} was cancelled.", inv.command))
            }
        }
    }

    async fn session_started(&self, _session_id: &str) {}

    async fn session_ended(&self, session_id: &str) {
        self.send(Request::EndSession(session_id.to_owned()));
    }
}
