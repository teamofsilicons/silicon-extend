//! Opt-in tests for a disposable Windows CI runner. No service, account or existing app is used.
//! Run through apps/desktop/windows/verify-native.ps1; fixtures are child test processes we own.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use extend_agent::drivers::{terminal, windows::WindowsDriver};
use extend_driver::{Driver, Invocation, Output, cancel::CancelToken};
use serde_json::{Value, json};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WAIT_TIMEOUT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColorBrush};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEINPUT, SendInput, VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

fn opt_in() -> PathBuf {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("EXTEND_WINDOWS_NATIVE").as_deref(),
        Ok("disposable-runner")
    );
    let out = PathBuf::from(std::env::var_os("EXTEND_WINDOWS_EVIDENCE").expect("evidence directory"));
    assert!(out.is_absolute());
    std::fs::create_dir_all(&out).unwrap();
    out
}

fn save(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn fixture_command(test: &str, dir: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored", "--exact", test, "--nocapture"]);
    command.env("EXTEND_WINDOWS_FIXTURE", dir);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("fixture.log"))
        .unwrap();
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    command
}

async fn wait_json(path: &Path) -> Value {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(value) = serde_json::from_slice(&bytes)
        {
            return value;
        }
        assert!(
            Instant::now() < until,
            "fixture did not report ready: {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct OwnedWindow {
    process: Child,
    ready: Value,
    evidence: PathBuf,
}

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        // The Child handle belongs to this test; no process-name or desktop-wide cleanup.
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl OwnedWindow {
    async fn start(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let mut own = Self {
            process: fixture_command("fixture_window", dir)
                .env("EXTEND_WINDOWS_PARENT", std::process::id().to_string())
                .spawn()
                .unwrap(),
            ready: Value::Null,
            evidence: dir.to_path_buf(),
        };
        own.ready = wait_json(&dir.join("ready.json")).await;
        assert_eq!(own.ready["pid"].as_u64(), Some(u64::from(own.process.id())));
        own
    }

    fn handle(&self, key: &str) -> HWND {
        let hwnd = HWND(self.ready[key].as_u64().unwrap() as usize as *mut _);
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        assert_eq!(pid, self.process.id(), "the HWND must still belong to our fixture");
        hwnd
    }

    fn foreground(&self) {
        let hwnd = self.handle("window");
        unsafe {
            let requested = SetForegroundWindow(hwnd).as_bool();
            // Some runner images deny programmatic activation. Bootstrap through an actual
            // click on our own title bar, never by changing Windows' global focus policy.
            let bootstrap = (!requested && GetForegroundWindow() != hwnd).then(|| self.click_title_bar());
            // Cross-process activation is asynchronous. WM_NULL acknowledges the fixture's
            // message queue before we inspect focus; it never changes another window.
            // https://devblogs.microsoft.com/oldnewthing/20161118-00/?p=94745
            let mut unused = 0;
            let acknowledged = SendMessageTimeoutW(
                hwnd,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                5_000,
                Some(&mut unused),
            )
            .0 != 0;
            let foreground = GetForegroundWindow();
            let mut foreground_pid = 0;
            GetWindowThreadProcessId(foreground, Some(&mut foreground_pid));
            save(
                self.evidence.join(format!("foreground-{}.json", uuid::Uuid::new_v4())),
                &json!({
                    "requested": requested, "acknowledged": acknowledged,
                    "expected_window": hwnd.0 as usize, "expected_pid": self.process.id(),
                    "foreground_window": foreground.0 as usize, "foreground_pid": foreground_pid,
                    "title_bar_bootstrap": bootstrap,
                }),
            );
            assert!(
                acknowledged,
                "owned fixture must process its activation within five seconds"
            );
            assert_eq!(self.handle("window"), hwnd, "fixture ownership must remain unchanged");
            assert_eq!(
                foreground, hwnd,
                "owned fixture must have input focus (request accepted: {requested})"
            );
        }
    }

    fn click_title_bar(&self) -> Value {
        let hwnd = self.handle("window");
        let log = self
            .evidence
            .join(format!("focus-bootstrap-{}.json", uuid::Uuid::new_v4()));
        // This guard restores only our window's topmost flag and the pointer even if a
        // fixture assertion panics. The owning Child guard subsequently closes the window.
        struct Restore<'a> {
            hwnd: HWND,
            cursor: POINT,
            cursor_safe: &'a std::cell::Cell<bool>,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(HWND_NOTOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    if self.cursor_safe.get() {
                        let _ = SetCursorPos(self.cursor.x, self.cursor.y);
                    }
                }
            }
        }
        struct ReleaseClick {
            pending: bool,
            log: PathBuf,
        }
        impl Drop for ReleaseClick {
            fn drop(&mut self) {
                if self.pending {
                    // A partial SendInput result can have inserted only LEFTDOWN. This guard
                    // drops before Restore, so release it while the pointer is still over the
                    // owned title bar. Do not panic again if failure evidence cannot be saved.
                    let sent =
                        unsafe { SendInput(&[mouse_input(MOUSEEVENTF_LEFTUP)], std::mem::size_of::<INPUT>() as i32) };
                    let evidence = json!({"cleanup_sent_mouse_up": sent, "accepted": sent == 1,
                        "cursor_restored": false, "reason": "leave cursor in place on an incomplete click; fixture teardown follows"});
                    let _ = std::fs::write(&self.log, evidence.to_string());
                    eprintln!("owned title-bar mouse-up cleanup: {evidence}");
                }
            }
        }
        unsafe {
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor).unwrap();
            let cursor_safe = std::cell::Cell::new(true);
            let _restore = Restore {
                hwnd,
                cursor,
                cursor_safe: &cursor_safe,
            };
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
            .unwrap();
            let mut title = TITLEBARINFO {
                cbSize: std::mem::size_of::<TITLEBARINFO>() as u32,
                ..Default::default()
            };
            GetTitleBarInfo(hwnd, &mut title).unwrap();
            let rect = title.rcTitleBar;
            assert!(
                rect.right > rect.left && rect.bottom > rect.top,
                "owned title bar must be visible"
            );
            let point = POINT {
                x: rect.left + (rect.right - rect.left) / 2,
                y: rect.top + (rect.bottom - rect.top) / 2,
            };
            let hit = WindowFromPoint(point);
            let mut hit_pid = 0;
            GetWindowThreadProcessId(hit, Some(&mut hit_pid));
            let packed = LPARAM(((point.x as u32 & 0xffff) | ((point.y as u32 & 0xffff) << 16)) as isize);
            let title_bar = message(hwnd, WM_NCHITTEST, WPARAM(0), packed);
            let mut evidence = json!({"expected_window": hwnd.0 as usize, "expected_pid": self.process.id(),
                "hit_window": hit.0 as usize, "hit_pid": hit_pid, "hit_test": title_bar,
                "x": point.x, "y": point.y, "sent_inputs": 0});
            save(&log, &evidence);
            assert_eq!(
                hit, hwnd,
                "only our exact top-level window may receive the bootstrap click"
            );
            assert_eq!(
                hit_pid,
                self.process.id(),
                "bootstrap point must belong to our fixture process"
            );
            assert_eq!(title_bar, HTCAPTION as usize, "bootstrap point must be the title bar");
            assert_eq!(self.handle("window"), hwnd, "fixture ownership must remain unchanged");
            SetCursorPos(point.x, point.y).unwrap();
            let mut actual = POINT::default();
            GetCursorPos(&mut actual).unwrap();
            assert_eq!(
                (actual.x, actual.y),
                (point.x, point.y),
                "pointer must reach the verified point"
            );
            assert_eq!(
                WindowFromPoint(actual),
                hwnd,
                "recheck the hit immediately before input"
            );
            let mut release = ReleaseClick {
                pending: false,
                log: log.with_extension("cleanup.json"),
            };
            let sent = SendInput(
                &[mouse_input(MOUSEEVENTF_LEFTDOWN), mouse_input(MOUSEEVENTF_LEFTUP)],
                std::mem::size_of::<INPUT>() as i32,
            );
            release.pending = sent > 0;
            cursor_safe.set(sent == 0);
            evidence["sent_inputs"] = json!(sent);
            save(&log, &evidence);
            assert_eq!(sent, 2, "Windows must accept both owned-title-bar input events");
            let until = Instant::now() + Duration::from_secs(5);
            let mouse_down = || GetAsyncKeyState(i32::from(VK_LBUTTON.0)) as u16 & 0x8000 != 0;
            while (GetForegroundWindow() != hwnd || mouse_down()) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            evidence["foreground_window"] = json!(GetForegroundWindow().0 as usize);
            evidence["mouse_button_released"] = json!(!mouse_down());
            save(&log, &evidence);
            assert_eq!(
                GetForegroundWindow(),
                hwnd,
                "owned title-bar click must activate the fixture"
            );
            assert!(
                !mouse_down(),
                "bootstrap mouse-up must be processed before restoring the pointer"
            );
            release.pending = false;
            cursor_safe.set(true);
            evidence
        }
    }

    fn checked(&self) -> usize {
        message(self.handle("check"), BM_GETCHECK, WPARAM(0), LPARAM(0))
    }

    fn text(&self) -> String {
        let mut text = [0u16; 512];
        let n = message(
            self.handle("edit"),
            WM_GETTEXT,
            WPARAM(text.len()),
            LPARAM(text.as_mut_ptr() as isize),
        );
        String::from_utf16_lossy(&text[..n.min(text.len())])
    }
}

fn message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> usize {
    let mut result = 0;
    let sent = unsafe { SendMessageTimeoutW(hwnd, msg, wp, lp, SMTO_ABORTIFHUNG, 2_000, Some(&mut result)) };
    assert_ne!(sent.0, 0, "owned fixture did not answer message {msg}");
    result
}

unsafe extern "system" fn fixture_window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

struct FixtureClass {
    name: PCWSTR,
    instance: HINSTANCE,
    window: Option<HWND>,
}

impl Drop for FixtureClass {
    fn drop(&mut self) {
        unsafe {
            if let Some(hwnd) = self.window
                && IsWindow(Some(hwnd)).as_bool()
            {
                let _ = DestroyWindow(hwnd);
            }
            let _ = UnregisterClassW(self.name, Some(self.instance));
        }
    }
}

#[test]
#[ignore = "child fixture, launched only by the opt-in owned-window test"]
fn fixture_window() {
    opt_in();
    let dir = PathBuf::from(std::env::var_os("EXTEND_WINDOWS_FIXTURE").unwrap());
    let title = format!("Extend native fixture {}", uuid::Uuid::new_v4());
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    // Standard Win32 controls expose real UI Automation providers. A separate process is used
    // because the production driver deliberately excludes its own windows from app snapshots.
    unsafe {
        // A STATIC top-level control returns HTTRANSPARENT, so Windows deliberately skips it
        // during mouse hit testing. Use a real application class with DefWindowProc's caption
        // handling; the child EDIT and BUTTON keep their standard accessibility providers.
        // https://learn.microsoft.com/en-us/windows/win32/controls/about-static-controls
        let class_name = w!("ExtendNativeFixtureWindow");
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(fixture_window_proc),
            hInstance: instance,
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        let class_atom = RegisterClassW(&class);
        assert_ne!(class_atom, 0, "register the owned fixture window class");
        // The name is a static UTF-16 literal and the delegate is a function in this process;
        // both outlive the window. On forced Child cleanup Windows reclaims this local class.
        let mut registered = FixtureClass {
            name: class_name,
            instance,
            window: None,
        };
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            PCWSTR(wide.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            80,
            80,
            560,
            300,
            None,
            None,
            Some(instance),
            None,
        )
        .unwrap();
        registered.window = Some(window);
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!("initial"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            30,
            40,
            440,
            34,
            Some(window),
            None,
            None,
            None,
        )
        .unwrap();
        let check = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Owned checkbox"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            30,
            110,
            240,
            36,
            Some(window),
            None,
            None,
            None,
        )
        .unwrap();
        let _ = ShowWindow(window, SW_SHOW);
        let foreground_requested = SetForegroundWindow(window).as_bool();
        let parent = std::env::var("EXTEND_WINDOWS_PARENT").unwrap().parse().unwrap();
        let parent_allowed = AllowSetForegroundWindow(parent).is_ok();
        save(
            dir.join("ready.json"),
            &json!({
                "pid": std::process::id(), "title": title,
                "window": window.0 as usize, "edit": edit.0 as usize, "check": check.0 as usize,
                "window_class": "ExtendNativeFixtureWindow", "class_atom": class_atom,
                "foreground_requested": foreground_requested, "parent_foreground_allowed": parent_allowed,
            }),
        );
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

async fn run(driver: &WindowsDriver, session: &str, dir: &Path, command: &str, args: &[&str]) -> Output {
    let args: Vec<_> = args.iter().map(|s| (*s).to_owned()).collect();
    let result = driver
        .run(Invocation {
            id: uuid::Uuid::new_v4(),
            session_id: session,
            command,
            args: &args,
            attachments: &[],
            workdir: dir,
            timeout: Duration::from_secs(15),
            cancel: CancelToken::new(),
        })
        .await;
    assert!(result.ok, "{command}: {result:?}");
    result
}

#[tokio::test]
#[ignore = "requires explicit disposable Windows runner opt-in and a graphical desktop"]
async fn owned_window_snapshot_click_type_and_capture() {
    let out = opt_in().join(format!("window-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&out).unwrap();
    // Match the driver's coordinate space before fixture hit testing. This affects this
    // disposable test process only, never Windows' global display or focus settings.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let sentinel = OwnedWindow::start(&out.join("sentinel")).await;
    let target = OwnedWindow::start(&out.join("target")).await;
    target.foreground();
    let driver = WindowsDriver::new(out.join("state"));
    let probe = driver.probe().await;
    assert!(
        probe.capabilities.contains(&extend_protocol::Capability::ScreenRead),
        "graphical desktop unavailable: {probe:?}"
    );
    let session = format!("native-{}", uuid::Uuid::new_v4());
    driver.session_started(&session).await;
    let snapshot = run(&driver, &session, &out, "snapshot", &[]).await;
    save(out.join("snapshot-before.json"), &snapshot.output);
    let serialized = snapshot.output.to_string();
    assert!(serialized.contains(target.ready["title"].as_str().unwrap()));
    assert!(serialized.contains("Owned checkbox"));
    assert!(!serialized.contains(sentinel.ready["title"].as_str().unwrap()));
    assert_eq!(target.checked(), 0);
    target.foreground();
    run(&driver, &session, &out, "find", &["text", "Owned checkbox", "click"]).await;
    let until = Instant::now() + Duration::from_secs(5);
    while target.checked() != 1 && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        target.checked(),
        1,
        "SendInput click must toggle the real owned control"
    );
    target.foreground();
    run(&driver, &session, &out, "find", &["role", "text-field", "focus"]).await;
    target.foreground();
    run(&driver, &session, &out, "type", &["native-text"]).await;
    let until = Instant::now() + Duration::from_secs(5);
    while !target.text().contains("native-text") && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        target.text().contains("native-text"),
        "SendInput text must reach the real edit control"
    );
    target.foreground();
    // FrontmostApp captures the full virtual screen; this is disposable-runner desktop
    // evidence, not proof of an isolated application-window crop.
    let capture = run(&driver, &session, &out, "screenshot", &["runner-desktop.png"]).await;
    assert_eq!(capture.files.len(), 1);
    let bytes = std::fs::read(&capture.files[0].path).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(std::io::Cursor::new(bytes)))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert!(
        info.width >= 400 && info.height >= 200,
        "runner desktop capture must have usable dimensions"
    );
    let colors: std::collections::HashSet<_> = pixels[..info.buffer_size()].as_chunks::<4>().0.iter().collect();
    assert!(colors.len() > 4, "capture must not be a blank or unavailable display");
    target.foreground();
    let after = run(&driver, &session, &out, "snapshot", &[]).await;
    save(out.join("snapshot-after.json"), &after.output);
    assert_eq!(sentinel.text(), "initial");
    assert_eq!(sentinel.checked(), 0, "other owned process must remain untouched");
    driver.session_ended(&session).await;
    save(
        out.join("result.json"),
        &json!({"passed": true, "target": target.ready, "sentinel": sentinel.ready,
        "text": target.text(), "checked": target.checked(), "capture_width": info.width, "capture_height": info.height,
        "capture_scope": "full disposable runner virtual desktop, not an isolated window crop",
        "limits": "Disposable runner desktop only; no lock, UAC, wake, multi-monitor or physical Windows proof."}),
    );
}

struct OwnedSession(String);
impl OwnedSession {
    fn new() -> Self {
        Self(format!("native-{}", uuid::Uuid::new_v4()))
    }
}
impl Drop for OwnedSession {
    fn drop(&mut self) {
        terminal::end_session(&self.0);
    }
}

struct OwnedProcess {
    pid: u32,
    handle: HANDLE,
}

impl OwnedProcess {
    fn open(pid: u32) -> Self {
        // A PID reported by the fixture we just started. Retain its kernel handle, so even
        // failure cleanup cannot act on a different process after PID reuse.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }.unwrap();
        Self { pid, handle }
    }

    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        unsafe {
            if self.alive() {
                let _ = TerminateProcess(self.handle, 1);
                let _ = WaitForSingleObject(self.handle, 5_000);
            }
            let _ = CloseHandle(self.handle);
        }
    }
}

async fn gone(process: &OwnedProcess) {
    let until = Instant::now() + Duration::from_secs(10);
    while process.alive() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !process.alive(),
        "owned child {} outlived its command/session",
        process.pid
    );
}

#[test]
#[ignore = "child fixture, launched only by the opt-in owned-terminal test"]
#[allow(
    clippy::zombie_processes,
    reason = "Windows fixture deliberately leaves a child for session cleanup to prove containment"
)]
fn fixture_terminal() {
    use std::os::windows::process::CommandExt;
    opt_in();
    let dir = PathBuf::from(std::env::var_os("EXTEND_WINDOWS_FIXTURE").unwrap());
    if std::env::var("EXTEND_WINDOWS_CHILD").is_ok() {
        save(dir.join("child.json"), &json!({"pid": std::process::id()}));
        std::thread::sleep(Duration::from_secs(90));
        return;
    }
    let child = fixture_command("fixture_terminal", &dir)
        .env("EXTEND_WINDOWS_CHILD", "1")
        .creation_flags(0x0000_0200 | 0x0800_0000) // new process group, no console; job is inherited
        .spawn()
        .unwrap();
    save(
        dir.join("parent.json"),
        &json!({"pid": std::process::id(), "child_pid": child.id()}),
    );
    if std::env::var("EXTEND_WINDOWS_WAIT").is_ok() {
        std::thread::sleep(Duration::from_secs(90));
    }
}

fn terminal_request(dir: &Path, wait: bool) -> terminal::TerminalRequest {
    let exe = std::env::current_exe().unwrap();
    let mut env = vec![("EXTEND_WINDOWS_FIXTURE".into(), dir.display().to_string())];
    if wait {
        env.push(("EXTEND_WINDOWS_WAIT".into(), "1".into()));
    }
    terminal::TerminalRequest {
        command: format!("\"{}\" --ignored --exact fixture_terminal --nocapture", exe.display()),
        cwd: None,
        env,
    }
}

#[tokio::test]
#[ignore = "requires explicit disposable Windows runner opt-in"]
async fn owned_terminal_children_end_with_their_session_timeout_and_cancel() {
    let out = opt_in().join(format!("terminal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&out).unwrap();
    let mut retained = Vec::new();
    for name in ["a", "b"] {
        let dir = out.join(name);
        std::fs::create_dir(&dir).unwrap();
        let session = OwnedSession::new();
        let result = terminal::execute_in(
            &terminal_request(&dir, false),
            &dir,
            &dir,
            Duration::from_secs(20),
            &CancelToken::new(),
            Some(&session.0),
        )
        .await;
        assert!(result.ok, "{result:?}");
        let child = OwnedProcess::open(wait_json(&dir.join("child.json")).await["pid"].as_u64().unwrap() as u32);
        let parent = wait_json(&dir.join("parent.json")).await;
        assert_eq!(parent["child_pid"].as_u64(), Some(u64::from(child.pid)));
        assert!(
            child.alive(),
            "background child must first outlive its successful shell"
        );
        retained.push((session, child));
    }
    terminal::end_session(&retained[0].0.0);
    gone(&retained[0].1).await;
    assert!(retained[1].1.alive(), "another owned session must remain alive");
    terminal::end_session(&retained[1].0.0);
    gone(&retained[1].1).await;
    for (name, timeout) in [
        ("timeout", Duration::from_secs(10)),
        ("cancel", Duration::from_secs(30)),
    ] {
        let dir = out.join(name);
        std::fs::create_dir(&dir).unwrap();
        let session = OwnedSession::new();
        let cancel = CancelToken::new();
        let request = terminal_request(&dir, true);
        let operation = terminal::execute_in(&request, &dir, &dir, timeout, &cancel, Some(&session.0));
        tokio::pin!(operation);
        let ready = dir.join("child.json");
        let child = tokio::select! {
            value = wait_json(&ready) => OwnedProcess::open(value["pid"].as_u64().unwrap() as u32),
            result = &mut operation => panic!("terminal ended before its owned child started: {result:?}"),
        };
        let parent_ready = wait_json(&dir.join("parent.json")).await;
        assert_eq!(parent_ready["child_pid"].as_u64(), Some(u64::from(child.pid)));
        let parent = OwnedProcess::open(parent_ready["pid"].as_u64().unwrap() as u32);
        if name == "cancel" {
            cancel.cancel();
        }
        let result = operation.await;
        let expected = if name == "cancel" {
            "cancelled"
        } else {
            "command_timeout"
        };
        assert_eq!(result.error.unwrap().code, expected);
        // Assert before end_session: timeout/cancel themselves must contain the child tree.
        gone(&child).await;
        gone(&parent).await;
    }
    save(
        out.join("result.json"),
        &json!({"passed": true, "session_children": [retained[0].1.pid, retained[1].1.pid],
        "checks": ["background child retained until session end", "other session survives", "timeout kills tree", "cancel kills tree"]}),
    );
}
