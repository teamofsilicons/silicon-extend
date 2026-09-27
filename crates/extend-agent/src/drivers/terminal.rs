//! `terminal run <command> [--cwd <dir>] [--env KEY=VALUE]...`, Extend's addition for computers.
//!
//! Runs the command through the signed-in user's shell (`$SHELL -l -c` on Mac and Linux,
//! `cmd.exe /D /S /C` on Windows) with plain pipes, so stdout and stderr stay apart. The command
//! runs in its own process group, and the whole group is killed on timeout or cancel.
//!
//! Tokens: `--cwd` and `--env` are the terminal's own flags; every other token is the command.
//! One token is used as-is (`terminal run "ls -la | wc -l"`); several are joined with spaces
//! (`terminal run ls -la`). Tokens after `--` are always the command.
//!
//! Nothing a session's terminal started outlives the session ([`end_session`]): on a computer
//! several Carbons paired, a process left running would keep what one Silicon started running
//! into the next Silicon's session.
//!
//! * Mac and Linux: every command of a session runs with `EXTEND_SESSION_MARK=<random hex>` (one
//!   per session), which its children inherit. At the session's end every process of this
//!   account whose environment carries the mark (read from `/proc/<pid>/environ` on Linux,
//!   `sysctl KERN_PROCARGS2` on a Mac) is killed with its process group, so `setsid` and `&`
//!   don't escape it.
//! * Mac: macOS doesn't show the environment of Apple's own programs (`sleep`, `perl`, the
//!   shells), even to the same account, so the mark finds only other programs there. Two more
//!   ways cover the rest: each command's process group (a non-interactive shell's `&` jobs stay in
//!   it after the shell exits) is killed at the session's end, unless its id has since gone to an
//!   unrelated process; and the session's processes are followed by descent, through the parent
//!   unique id the kernel keeps for each process while its parent lives (the process table is
//!   read every half second while the session runs, and once more at its end). An Apple program
//!   that leaves its process group (`setsid`) the instant its parent exits can still escape; one
//!   started by anything that stays running, or any other program, can't.
//! * Windows: every command of a session goes into one Job Object that kills everything in it
//!   when it closes and allows no breakaway. The command starts suspended, joins the job, and only
//!   then runs, so nothing it starts is ever outside; the job is closed at the session's end.
//!
//! What the OS itself starts on a command's behalf (launchd, `schtasks`, `systemd-run`, `cron`,
//! `at`) isn't a child of the command and isn't contained; `docs/device-protocol.md` says so.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use extend_driver::cancel::CancelToken;
use extend_driver::{Invocation, LocalFile, Output};
use extend_protocol::model::{CommandError, FileKind};
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// Bytes of each stream returned inline; the full stream is uploaded as a file past this.
pub const INLINE_LIMIT: usize = 256 * 1024;

/// The environment variable that marks a session's processes.
pub const SESSION_MARK_VAR: &str = "EXTEND_SESSION_MARK";

/// What a live session's terminal started: its mark, and on Windows its job.
struct SessionProcesses {
    mark: String,
    #[cfg(windows)]
    job: Option<job::Job>,
    /// Mac: the unique ids of the session's processes seen so far, and the watcher's stop flag.
    #[cfg(target_os = "macos")]
    lineage: std::sync::Arc<lineage::Watch>,
    /// Mac: each command's process group, with its shell's unique id (to tell a reused id).
    #[cfg(target_os = "macos")]
    groups: Vec<(i32, u64)>,
}

fn sessions() -> &'static Mutex<HashMap<String, SessionProcesses>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, SessionProcesses>>> = OnceLock::new();
    SESSIONS.get_or_init(Default::default)
}

/// The mark for `session_id`'s processes: 128 random bits, new for each session.
pub fn session_mark(session_id: &str) -> String {
    let mut all = sessions().lock().unwrap();
    all.entry(session_id.to_owned())
        .or_insert_with(|| SessionProcesses {
            mark: extend_protocol::ids::hex_lower(&rand::random::<[u8; 16]>()),
            #[cfg(windows)]
            job: None,
            #[cfg(target_os = "macos")]
            lineage: lineage::Watch::start(),
            #[cfg(target_os = "macos")]
            groups: vec![],
        })
        .mark
        .clone()
}

/// Mac: notes a command's shell as a root of `session_id`'s processes.
#[cfg(target_os = "macos")]
fn note_root(session_id: &str, pid: Option<u32>) {
    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    let Some((unique, _)) = lineage::ids(pid) else { return };
    let mut all = sessions().lock().unwrap();
    if let Some(s) = all.get_mut(session_id) {
        s.lineage.known.lock().unwrap().insert(unique);
        // The shell leads its own process group (`process_group(0)`).
        s.groups.push((pid, unique));
    }
}

/// Ends every process `session_id`'s terminal started, and whatever they started in turn.
/// Returns how many were ended (Mac and Linux; Windows closes the job and says 0).
pub fn end_session(session_id: &str) -> usize {
    let Some(ended) = sessions().lock().unwrap().remove(session_id) else {
        return 0;
    };
    #[cfg(windows)]
    {
        // Closing the job's last handle kills everything in it.
        drop(ended.job);
        0
    }
    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        let extra = ended.lineage.stop_and_collect();
        #[cfg(not(target_os = "macos"))]
        let extra: Vec<i32> = vec![];
        #[cfg(target_os = "macos")]
        for (group, shell) in &ended.groups {
            // A group whose id now names a process that isn't the command's shell was reused:
            // leave it. Otherwise it is the command's (its shell, or its orphaned `&` jobs).
            if lineage::ids(*group).is_none_or(|(unique, _)| unique == *shell) {
                // SAFETY: signalling a process group of this account's command.
                unsafe {
                    libc::kill(-*group, libc::SIGKILL);
                }
            }
        }
        let killed = containment::kill_marked(&ended.mark, &extra);
        if killed > 0 {
            tracing::info!("ended {killed} process(es) session {session_id}'s terminal left running");
        }
        killed
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalRequest {
    pub command: String,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

/// Parses the tokens after `terminal`.
pub fn parse(args: &[String]) -> Result<TerminalRequest, String> {
    let usage = "Usage: terminal run <command> [--cwd <dir>] [--env KEY=VALUE]...";
    let Some((first, rest)) = args.split_first() else {
        return Err(format!("Nothing to run. {usage}"));
    };
    if first != "run" {
        return Err(format!("Unknown terminal action {first:?}. {usage}"));
    }
    let mut cwd = None;
    let mut env = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i].as_str();
        i += 1;
        if t == "--" {
            words.extend(rest[i..].iter().map(String::as_str));
            break;
        }
        let (name, inline) = match t.split_once('=') {
            Some((n, v)) if n == "--cwd" || n == "--env" => (n, Some(v)),
            _ => (t, None),
        };
        match name {
            "--cwd" | "--env" => {
                let value = match inline {
                    Some(v) => v.to_owned(),
                    None => {
                        let v = rest.get(i).ok_or_else(|| format!("{name} needs a value. {usage}"))?;
                        i += 1;
                        v.clone()
                    }
                };
                if name == "--cwd" {
                    cwd = Some(value);
                } else {
                    let (k, v) = value
                        .split_once('=')
                        .ok_or_else(|| format!("--env takes KEY=VALUE, got {value:?}"))?;
                    if k.is_empty() || k.contains('\0') {
                        return Err(format!("--env takes KEY=VALUE, got {value:?}"));
                    }
                    env.push((k.to_owned(), v.to_owned()));
                }
            }
            _ => words.push(t),
        }
    }
    let command = words.join(" ");
    if command.trim().is_empty() {
        return Err(format!("Nothing to run. {usage}"));
    }
    Ok(TerminalRequest { command, cwd, env })
}

/// The shell that runs the command, as argv before the command string.
pub fn shell_argv() -> Vec<String> {
    if let Ok(s) = std::env::var("EXTEND_TERMINAL_SHELL")
        && !s.trim().is_empty()
    {
        return s.split_whitespace().map(str::to_owned).collect();
    }
    if cfg!(windows) {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        return vec![comspec, "/D".into(), "/S".into(), "/C".into()];
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into());
    let name = Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sh")
        .to_owned();
    // A login shell gives the Carbon's usual PATH even when the app was started at login.
    if matches!(name.as_str(), "bash" | "zsh" | "fish" | "ksh") {
        vec![shell, "-l".into(), "-c".into()]
    } else {
        vec![shell, "-c".into()]
    }
}

pub async fn run(inv: &Invocation<'_>) -> Output {
    let req = match parse(inv.args) {
        Ok(r) => r,
        Err(m) => return Output::fail("invalid_args", m),
    };
    let home = crate::config::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cwd = match &req.cwd {
        Some(dir) => {
            let p = expand_home(dir, &home);
            if !p.is_dir() {
                return Output::fail(
                    "invalid_args",
                    format!("--cwd {dir} isn't a directory on this computer"),
                );
            }
            p
        }
        None => home,
    };
    execute_in(&req, &cwd, inv.workdir, inv.timeout, &inv.cancel, Some(inv.session_id)).await
}

fn expand_home(dir: &str, home: &Path) -> PathBuf {
    if dir == "~" {
        home.to_path_buf()
    } else if let Some(rest) = dir.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(dir)
    }
}

/// Runs a command outside any session (nothing contains what it leaves running).
pub async fn execute(
    req: &TerminalRequest,
    cwd: &Path,
    workdir: &Path,
    timeout: Duration,
    cancel: &CancelToken,
) -> Output {
    execute_in(req, cwd, workdir, timeout, cancel, None).await
}

/// Runs a command in `session`: marked (and on Windows in the session's job), so it ends with the
/// session at the latest.
pub async fn execute_in(
    req: &TerminalRequest,
    cwd: &Path,
    workdir: &Path,
    timeout: Duration,
    cancel: &CancelToken,
    session: Option<&str>,
) -> Output {
    let argv = shell_argv();
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        cmd.env(SESSION_MARK_VAR, session_mark(session));
    }
    #[cfg(windows)]
    {
        // cmd.exe parses its own command line; hand it over untouched.
        cmd.raw_arg(format!("\"{}\"", req.command));
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        let suspended = if session.is_some_and(|s| !s.is_empty()) {
            CREATE_SUSPENDED
        } else {
            0
        };
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | suspended);
    }
    #[cfg(not(windows))]
    {
        cmd.arg(&req.command);
        cmd.process_group(0);
    }
    cmd.current_dir(cwd)
        .envs(req.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Output::fail("command_failed", format!("couldn't start {}: {e}", argv[0])),
    };
    #[cfg(target_os = "macos")]
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        note_root(session, child.id());
    }
    #[cfg(windows)]
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        // Into the session's job before it runs a single instruction, then resumed.
        if let Some(handle) = child.raw_handle()
            && let Err(e) = job::contain(session, handle)
        {
            tracing::warn!("couldn't put the command in its session's job: {e}");
        }
    }
    let pid = child.id();
    let out_task = tokio::spawn(capture(
        child.stdout.take().expect("stdout"),
        workdir.join("stdout.txt"),
    ));
    let err_task = tokio::spawn(capture(
        child.stderr.take().expect("stderr"),
        workdir.join("stderr.txt"),
    ));

    enum End {
        Exited(std::process::ExitStatus),
        Failed(std::io::Error),
        Timeout,
        Cancelled,
    }
    let end = tokio::select! {
        s = child.wait() => match s { Ok(s) => End::Exited(s), Err(e) => End::Failed(e) },
        _ = tokio::time::sleep(timeout) => End::Timeout,
        _ = cancel.cancelled() => End::Cancelled,
    };
    if matches!(end, End::Timeout | End::Cancelled) {
        kill_tree(pid);
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        let _ = child.start_kill();
    }
    let (stdout, stderr) =
        match tokio::time::timeout(Duration::from_secs(5), async { (out_task.await, err_task.await) }).await {
            Ok((Ok(o), Ok(e))) => (o, e),
            // A grandchild kept the pipes open past the kill; report what we have.
            _ => (Captured::default(), Captured::default()),
        };
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut files = Vec::new();
    for (c, name) in [(&stdout, "stdout.txt"), (&stderr, "stderr.txt")] {
        if c.truncated
            && let Some(path) = &c.full_path
        {
            files.push(LocalFile {
                path: path.clone(),
                name: name.into(),
                content_type: "text/plain".into(),
                kind: FileKind::Log,
            });
        }
    }
    let exit_code = match &end {
        End::Exited(s) => s.code(),
        _ => None,
    };
    let mut output = serde_json::json!({
        "stdout": stdout.text,
        "stderr": stderr.text,
        "exit_code": exit_code,
        "duration_ms": duration_ms,
    });
    if stdout.truncated || stderr.truncated {
        output["stdout_truncated"] = stdout.truncated.into();
        output["stderr_truncated"] = stderr.truncated.into();
    }
    let mut text = stdout.text.clone();
    if !stderr.text.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr.text);
    }
    let fail = |code: &str, message: String, details: serde_json::Value| Output {
        ok: false,
        output: output.clone(),
        text: Some(if text.is_empty() { message.clone() } else { text.clone() }),
        error: Some(CommandError {
            code: code.into(),
            message,
            details,
        }),
        files: files.clone(),
    };
    match end {
        End::Exited(status) if status.success() => Output {
            ok: true,
            output,
            text: Some(text),
            error: None,
            files,
        },
        End::Exited(status) => match status.code() {
            Some(code) => fail(
                "command_failed",
                format!("The command exited with code {code}."),
                serde_json::json!({ "exit_code": code }),
            ),
            None => {
                let signal = exit_signal(&status);
                fail(
                    "command_failed",
                    format!(
                        "The command was ended by signal {}.",
                        signal.map_or("?".into(), |s| s.to_string())
                    ),
                    serde_json::json!({ "exit_code": null, "signal": signal }),
                )
            }
        },
        End::Failed(e) => fail(
            "command_failed",
            format!("couldn't wait for the command: {e}"),
            serde_json::Value::Null,
        ),
        End::Timeout => fail(
            "command_timeout",
            format!(
                "The command didn't finish within {} ms and was stopped.",
                timeout.as_millis()
            ),
            serde_json::json!({ "exit_code": null }),
        ),
        End::Cancelled => fail(
            "cancelled",
            "Extend cancelled this command.".into(),
            serde_json::json!({ "exit_code": null }),
        ),
    }
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Kills the command and everything it started.
#[cfg(unix)]
pub fn kill_tree(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    // SAFETY: signalling our own child's process group (created with process_group(0)).
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        // SAFETY: as above; the group may already be gone, which is fine.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    });
}

#[cfg(windows)]
pub fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
}

/// Mac and Linux: finding and ending the processes that carry a session's mark.
#[cfg(unix)]
pub mod containment {
    use super::SESSION_MARK_VAR;

    /// Whether a process environment block (`NAME=value` entries separated by NULs) carries
    /// exactly `SESSION_MARK_VAR=mark`.
    pub fn environ_has_mark(environ: &[u8], mark: &str) -> bool {
        let want = format!("{SESSION_MARK_VAR}={mark}");
        environ.split(|b| *b == 0).any(|entry| entry == want.as_bytes())
    }

    /// The environment in a `KERN_PROCARGS2` buffer: argc (4 bytes), the executable path, NUL
    /// padding, argc arguments, then the environment entries up to an empty one.
    pub fn procargs2_environ(buf: &[u8]) -> Option<Vec<&[u8]>> {
        let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
        let mut rest = &buf[4..];
        // The executable path, then its NUL padding.
        let end = rest.iter().position(|b| *b == 0)?;
        rest = &rest[end..];
        let start = rest.iter().position(|b| *b != 0)?;
        rest = &rest[start..];
        let mut parts = rest.split(|b| *b == 0);
        for _ in 0..argc.max(0) {
            parts.next()?;
        }
        Some(parts.take_while(|e| !e.is_empty()).collect())
    }

    /// Kills (SIGKILL) every process of this account carrying `mark`, and the `also` ones (found
    /// by descent), each with its process group.
    pub fn kill_marked(mark: &str, also: &[i32]) -> usize {
        let me = std::process::id() as i32;
        let mut killed = 0;
        let mut pids = marked(mark);
        for pid in also {
            if !pids.contains(pid) {
                pids.push(*pid);
            }
        }
        for pid in pids {
            if pid == me || pid <= 1 {
                continue;
            }
            // SAFETY: signals to processes of this account; a gone process is fine.
            unsafe {
                let group = libc::getpgid(pid);
                if group > 1 && group != libc::getpgid(me) {
                    libc::kill(-group, libc::SIGKILL);
                }
                if libc::kill(pid, libc::SIGKILL) == 0 {
                    killed += 1;
                }
            }
        }
        killed
    }

    #[cfg(target_os = "linux")]
    fn marked(mark: &str) -> Vec<i32> {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return vec![];
        };
        dir.filter_map(Result::ok)
            .filter_map(|e| {
                let pid: i32 = e.file_name().to_str()?.parse().ok()?;
                (e.metadata().ok()?.uid() == uid).then_some(pid)
            })
            .filter(|pid| std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| environ_has_mark(&env, mark)))
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn marked(mark: &str) -> Vec<i32> {
        let mut pids = vec![0i32; 8192];
        // SAFETY: the buffer is as big as its size says.
        let n = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as libc::c_int,
            )
        };
        if n <= 0 {
            return vec![];
        }
        pids.truncate(n as usize);
        let mut argmax: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: reads one int.
        let ok = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&mut argmax as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == 0;
        if !ok || argmax <= 0 {
            return vec![];
        }
        let mut buf = vec![0u8; argmax as usize];
        pids.into_iter()
            .filter(|pid| {
                let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, *pid];
                let mut len = buf.len();
                // SAFETY: fills `buf` up to `len`; fails for other accounts' processes.
                let r = unsafe {
                    libc::sysctl(
                        mib.as_mut_ptr(),
                        3,
                        buf.as_mut_ptr().cast(),
                        &mut len,
                        std::ptr::null_mut(),
                        0,
                    )
                };
                r == 0
                    && procargs2_environ(&buf[..len]).is_some_and(|env| env.iter().any(|e| environ_has_mark(e, mark)))
            })
            .collect()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn marked(_mark: &str) -> Vec<i32> {
        vec![]
    }
}

/// Mac: a session's processes by descent, through the parent unique id the kernel keeps from
/// each process's fork (it survives the parent's exit, unlike the parent pid).
#[cfg(any(target_os = "macos", all(test, unix)))]
pub mod lineage {
    use std::collections::HashSet;
    #[cfg(target_os = "macos")]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(target_os = "macos")]
    use std::sync::{Arc, Mutex};

    /// One process: pid, its unique id, and its parent's unique id at fork.
    pub type Row = (i32, u64, u64);

    /// Adds to `known` every process in `table` forked from one already known, until nothing
    /// more is added; returns the live pids that are known.
    pub fn expand(known: &mut HashSet<u64>, table: &[Row]) -> Vec<i32> {
        loop {
            let before = known.len();
            for (_, unique, parent) in table {
                if known.contains(parent) {
                    known.insert(*unique);
                }
            }
            if known.len() == before {
                break;
            }
        }
        table
            .iter()
            .filter(|(_, unique, _)| known.contains(unique))
            .map(|(pid, _, _)| *pid)
            .collect()
    }

    /// The session's processes seen so far, and the watcher that keeps adding to them.
    #[cfg(target_os = "macos")]
    pub struct Watch {
        pub known: Mutex<HashSet<u64>>,
        stop: AtomicBool,
    }

    #[cfg(target_os = "macos")]
    impl Watch {
        pub fn start() -> Arc<Watch> {
            let w = Arc::new(Watch {
                known: Mutex::new(HashSet::new()),
                stop: AtomicBool::new(false),
            });
            let watcher = w.clone();
            let _ = std::thread::Builder::new()
                .name("extend-session-watch".into())
                .spawn(move || {
                    while !watcher.stop.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        if watcher.known.lock().unwrap().is_empty() {
                            continue;
                        }
                        let t = table();
                        expand(&mut watcher.known.lock().unwrap(), &t);
                    }
                });
            w
        }

        /// Stops watching and returns the live pids of the session's processes.
        pub fn stop_and_collect(&self) -> Vec<i32> {
            self.stop.store(true, Ordering::SeqCst);
            let t = table();
            let mut known = self.known.lock().unwrap();
            if known.is_empty() {
                return vec![];
            }
            expand(&mut known, &t)
        }
    }

    /// `proc_pidinfo(PROC_PIDUNIQIDENTIFIERINFO)`: (unique id, parent's unique id).
    #[cfg(target_os = "macos")]
    pub fn ids(pid: i32) -> Option<(u64, u64)> {
        const PROC_PIDUNIQIDENTIFIERINFO: libc::c_int = 17;
        // struct proc_uniqidentifierinfo: p_uuid[16], p_uniqueid, p_puniqueid, … (56 bytes today).
        let mut buf = [0u8; 128];
        // SAFETY: the buffer is as big as its size says.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDUNIQIDENTIFIERINFO,
                0,
                buf.as_mut_ptr().cast(),
                buf.len() as libc::c_int,
            )
        };
        if n < 32 {
            return None;
        }
        let unique = u64::from_ne_bytes(buf[16..24].try_into().ok()?);
        let parent = u64::from_ne_bytes(buf[24..32].try_into().ok()?);
        Some((unique, parent))
    }

    /// Every process this account can ask about.
    #[cfg(target_os = "macos")]
    pub fn table() -> Vec<Row> {
        let mut pids = vec![0i32; 8192];
        // SAFETY: the buffer is as big as its size says.
        let n = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as libc::c_int,
            )
        };
        if n <= 0 {
            return vec![];
        }
        pids.truncate(n as usize);
        pids.into_iter()
            .filter_map(|pid| ids(pid).map(|(u, p)| (pid, u, p)))
            .collect()
    }
}

/// Windows: one Job Object per session, closed (killing everything in it) at the session's end.
#[cfg(windows)]
mod job {
    use std::os::windows::io::RawHandle;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    /// A job handle; dropping it closes the job.
    pub struct Job(HANDLE);

    // SAFETY: a kernel handle, used from any thread.
    unsafe impl Send for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closing the handle this job owns.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    fn new_job() -> windows::core::Result<Job> {
        // SAFETY: plain job-object calls on a handle this function owns.
        unsafe {
            let handle = CreateJobObjectW(None, windows::core::PCWSTR::null())?;
            let job = Job(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // Kill on close, and no JOB_OBJECT_LIMIT_BREAKAWAY_OK: nothing leaves the job.
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            Ok(job)
        }
    }

    /// Puts the suspended process into `session`'s job, then lets it run.
    pub fn contain(session: &str, process: RawHandle) -> Result<(), String> {
        let process = HANDLE(process);
        let result = (|| {
            let mut all = super::sessions().lock().unwrap();
            let entry = all.get_mut(session).ok_or("the session has no mark")?;
            if entry.job.is_none() {
                entry.job = Some(new_job().map_err(|e| e.to_string())?);
            }
            let job = entry.job.as_ref().expect("just made");
            // SAFETY: both handles are live.
            unsafe { AssignProcessToJobObject(job.0, process) }.map_err(|e| e.to_string())
        })();
        // Resumed whatever happened: a command that couldn't join still runs (and still ends
        // with its own process tree on timeout).
        // SAFETY: the process this function was handed, created suspended.
        unsafe {
            NtResumeProcess(process);
        }
        result
    }
}

#[derive(Debug, Default, Clone)]
struct Captured {
    text: String,
    truncated: bool,
    full_path: Option<PathBuf>,
}

/// Reads a stream: the first [`INLINE_LIMIT`] bytes inline, everything to `spill` once past it.
async fn capture(mut reader: impl AsyncRead + Unpin, spill: PathBuf) -> Captured {
    use tokio::io::AsyncWriteExt as _;
    let mut inline = Vec::new();
    let mut file: Option<tokio::fs::File> = None;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if file.is_none()
            && inline.len() + n > INLINE_LIMIT
            && let Ok(mut f) = tokio::fs::File::create(&spill).await
        {
            let _ = f.write_all(&inline).await;
            file = Some(f);
        }
        if let Some(f) = file.as_mut() {
            let _ = f.write_all(&buf[..n]).await;
        }
        let room = INLINE_LIMIT.saturating_sub(inline.len());
        inline.extend_from_slice(&buf[..n.min(room)]);
    }
    let truncated = file.is_some();
    if let Some(mut f) = file {
        let _ = f.flush().await;
    }
    Captured {
        text: String::from_utf8_lossy(&inline).into_owned(),
        truncated,
        full_path: truncated.then_some(spill),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_run_with_flags() {
        let r = parse(&s(&[
            "run",
            "ls -la | wc -l",
            "--cwd",
            "/tmp",
            "--env",
            "A=1",
            "--env=B=x=y",
        ]))
        .unwrap();
        assert_eq!(r.command, "ls -la | wc -l");
        assert_eq!(r.cwd.as_deref(), Some("/tmp"));
        assert_eq!(r.env, vec![("A".into(), "1".into()), ("B".into(), "x=y".into())]);
        let r = parse(&s(&["run", "ls", "-la"])).unwrap();
        assert_eq!(r.command, "ls -la");
        let r = parse(&s(&["run", "--", "echo", "--cwd"])).unwrap();
        assert_eq!(r.command, "echo --cwd");
        assert!(parse(&s(&["run"])).is_err());
        assert!(parse(&s(&["exec", "ls"])).is_err());
        assert!(parse(&s(&["run", "ls", "--env", "NOEQUALS"])).is_err());
        assert!(parse(&s(&["run", "ls", "--cwd"])).is_err());
    }

    #[cfg(unix)]
    mod unix {
        use super::super::*;

        async fn go(cmd: &str, timeout: Duration) -> Output {
            let dir = tempfile::tempdir().unwrap();
            let req = TerminalRequest {
                command: cmd.into(),
                cwd: None,
                env: vec![("EXTEND_T".into(), "hello".into())],
            };
            let out = execute(&req, dir.path(), dir.path(), timeout, &CancelToken::new()).await;
            // Files must be read before the temp dir goes away.
            for f in &out.files {
                assert!(f.path.exists());
            }
            out
        }

        #[tokio::test]
        async fn captures_stdout_stderr_and_exit_code() {
            let out = go("echo out-$EXTEND_T; echo err >&2", Duration::from_secs(20)).await;
            assert!(out.ok, "{out:?}");
            assert_eq!(out.output["stdout"], "out-hello\n");
            assert_eq!(out.output["stderr"], "err\n");
            assert_eq!(out.output["exit_code"], 0);
            assert!(out.output["duration_ms"].as_u64().is_some());
        }

        #[tokio::test]
        async fn non_zero_exit_is_command_failed() {
            let out = go("echo nope >&2; exit 3", Duration::from_secs(20)).await;
            assert!(!out.ok);
            let e = out.error.unwrap();
            assert_eq!(e.code, "command_failed");
            assert_eq!(e.details["exit_code"], 3);
            assert_eq!(out.output["exit_code"], 3);
            assert_eq!(out.output["stderr"], "nope\n");
        }

        #[tokio::test]
        async fn timeout_kills_the_whole_group() {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("survived");
            let started = Instant::now();
            // The background sleep is a grandchild: it must die with the group.
            let cmd = format!("(sleep 3; touch {}) & sleep 30", marker.display());
            let out = go(&cmd, Duration::from_millis(500)).await;
            assert!(started.elapsed() < Duration::from_secs(10));
            assert_eq!(out.error.unwrap().code, "command_timeout");
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert!(!marker.exists(), "a grandchild outlived the timeout");
        }

        #[tokio::test]
        async fn cancel_stops_it() {
            let dir = tempfile::tempdir().unwrap();
            let token = CancelToken::new();
            let t2 = token.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                t2.cancel();
            });
            let req = TerminalRequest {
                command: "sleep 30".into(),
                cwd: None,
                env: vec![],
            };
            let out = execute(&req, dir.path(), dir.path(), Duration::from_secs(60), &token).await;
            assert_eq!(out.error.unwrap().code, "cancelled");
        }

        /// Stands in for a long-running program the Silicon started (`--ignored` runs it). On a
        /// Mac this test binary isn't one of Apple's, so its environment can be read.
        #[test]
        #[ignore = "a sleeper for what_a_session_left_running_ends_with_it"]
        fn sleeper() {
            std::thread::sleep(Duration::from_secs(600));
        }

        /// A process started in a new session and process group (`setsid`, backgrounded) and a
        /// plain `&` job are gone once the session ends; another session's are left alone.
        #[tokio::test]
        async fn what_a_session_left_running_ends_with_it() {
            let dir = tempfile::tempdir().unwrap();
            // perl's setsid() runs everywhere a shell does (macOS has no setsid command).
            let sleeper = if cfg!(target_os = "macos") {
                let me = std::env::current_exe().unwrap();
                format!(
                    "'{}' --ignored --exact drivers::terminal::tests::unix::sleeper --nocapture",
                    me.display()
                )
            } else {
                "sleep 600".to_owned()
            };
            let command = format!(
                "perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV' -- {sleeper} >/dev/null 2>&1 & echo $!; sleep 600 >/dev/null 2>&1 & echo $!"
            );
            let start = |session: &'static str| {
                let dir = dir.path().to_path_buf();
                let command = command.clone();
                async move {
                    let req = TerminalRequest {
                        command,
                        cwd: None,
                        env: vec![],
                    };
                    let out = execute_in(
                        &req,
                        &dir,
                        &dir,
                        Duration::from_secs(20),
                        &CancelToken::new(),
                        Some(session),
                    )
                    .await;
                    assert!(out.ok, "{out:?}");
                    let pids: Vec<i32> = out.output["stdout"]
                        .as_str()
                        .unwrap()
                        .split_whitespace()
                        .map(|p| p.parse().unwrap())
                        .collect();
                    assert_eq!(pids.len(), 2, "{out:?}");
                    pids
                }
            };
            let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
            let left = start("c01").await;
            let other = start("c02").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            for pid in left.iter().chain(&other) {
                assert!(alive(*pid), "the command's child {pid} is running");
            }
            end_session("c01");
            for _ in 0..50 {
                if !left.iter().any(|p| alive(*p)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            for pid in &left {
                assert!(!alive(*pid), "{pid} outlived its session");
            }
            for pid in &other {
                assert!(alive(*pid), "another session's process {pid} was killed");
            }
            // A session ends once; the other one cleans up too.
            assert_eq!(end_session("c01"), 0);
            end_session("c02");
            for _ in 0..50 {
                if !other.iter().any(|p| alive(*p)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            for pid in &other {
                assert!(!alive(*pid), "{pid} outlived its session");
            }
        }

        #[test]
        fn descent_follows_forks_through_known_processes() {
            use std::collections::HashSet;
            // The shell (id 10) forked 11, which forked 12; 12 forked 13 after 11 exited; 20 is
            // someone else's.
            let table = [(101, 11, 10), (102, 12, 11), (103, 13, 12), (200, 20, 1)];
            let mut known: HashSet<u64> = [10].into();
            let mut found = super::super::lineage::expand(&mut known, &table);
            found.sort();
            assert_eq!(found, vec![101, 102, 103]);
            // A process forked from one never seen isn't the session's.
            let mut known: HashSet<u64> = [10].into();
            assert!(super::super::lineage::expand(&mut known, &[(300, 30, 29)]).is_empty());
        }

        #[test]
        fn marks_are_matched_exactly() {
            let env = b"A=1\0EXTEND_SESSION_MARK=abc\0B=2\0";
            assert!(containment::environ_has_mark(env, "abc"));
            assert!(!containment::environ_has_mark(env, "ab"));
            assert!(!containment::environ_has_mark(b"EXTEND_SESSION_MARK=abcd\0", "abc"));
            // A KERN_PROCARGS2 buffer: argc 2, the path, padding, two arguments, the environment.
            let mut buf = 2i32.to_ne_bytes().to_vec();
            buf.extend_from_slice(b"/bin/sh\0\0\0sh\0-c\0HOME=/x\0EXTEND_SESSION_MARK=abc\0\0junk");
            let env = containment::procargs2_environ(&buf).unwrap();
            assert_eq!(env, vec![&b"HOME=/x"[..], &b"EXTEND_SESSION_MARK=abc"[..]]);
            // Each session has its own mark, kept until the session ends.
            let a = session_mark("m01");
            assert_eq!(a, session_mark("m01"));
            assert_ne!(a, session_mark("m02"));
            assert_eq!(a.len(), 32);
            end_session("m01");
            end_session("m02");
            assert_ne!(session_mark("m01"), a);
            end_session("m01");
        }

        #[tokio::test]
        async fn large_output_is_truncated_and_kept_as_a_file() {
            let dir = tempfile::tempdir().unwrap();
            let req = TerminalRequest {
                command: "head -c 600000 /dev/zero | tr '\\0' a".into(),
                cwd: None,
                env: vec![],
            };
            let out = execute(
                &req,
                dir.path(),
                dir.path(),
                Duration::from_secs(20),
                &CancelToken::new(),
            )
            .await;
            assert!(out.ok);
            assert_eq!(out.output["stdout"].as_str().unwrap().len(), INLINE_LIMIT);
            assert_eq!(out.output["stdout_truncated"], true);
            assert_eq!(out.files.len(), 1);
            assert_eq!(std::fs::metadata(&out.files[0].path).unwrap().len(), 600_000);
        }
    }
}
