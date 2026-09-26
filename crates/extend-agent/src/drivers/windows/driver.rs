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
fn locked() -> bool {
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

fn os_version() -> Option<String> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION.get_or_init(super::shell::ver).clone()
}

#[async_trait]
impl Driver for WindowsDriver {
    async fn probe(&self) -> Probe {
        tokio::task::spawn_blocking(|| probe_from(locked(), os_version()))
            .await
            .unwrap_or_else(|_| probe_from(false, None))
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
