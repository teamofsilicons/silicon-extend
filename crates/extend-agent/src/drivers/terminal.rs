//! `terminal run <command> [--cwd <dir>] [--env KEY=VALUE]...`, Extend's addition for computers.
//!
//! Runs the command through the signed-in user's shell (`$SHELL -l -c` on Mac and Linux,
//! `cmd.exe /D /S /C` on Windows) with plain pipes, so stdout and stderr stay apart. The command
//! runs in its own process group, and the whole group is killed on timeout or cancel.
//!
//! Tokens: `--cwd` and `--env` are the terminal's own flags; every other token is the command.
//! One token is used as-is (`terminal run "ls -la | wc -l"`); several are joined with spaces
//! (`terminal run ls -la`). Tokens after `--` are always the command.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use extend_driver::cancel::CancelToken;
use extend_driver::{Invocation, LocalFile, Output};
use extend_protocol::model::{CommandError, FileKind};
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// Bytes of each stream returned inline; the full stream is uploaded as a file past this.
pub const INLINE_LIMIT: usize = 256 * 1024;

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
    execute(&req, &cwd, inv.workdir, inv.timeout, &inv.cancel).await
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

pub async fn execute(
    req: &TerminalRequest,
    cwd: &Path,
    workdir: &Path,
    timeout: Duration,
    cancel: &CancelToken,
) -> Output {
    let argv = shell_argv();
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    #[cfg(windows)]
    {
        // cmd.exe parses its own command line; hand it over untouched.
        cmd.raw_arg(format!("\"{}\"", req.command));
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
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
