//! Runs the real `extend` binary against a small fake Extend service and checks what a device
//! command would send to the device: `command`, `args`, and Extend's own settings.
//!
//! `EXTEND_TEST_BIN` runs the checks against another build of the CLI instead of this one.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

/// A fake service: answers the version handshake and device commands, and reports each command
/// request it receives.
struct FakeService {
    url: String,
    requests: mpsc::Receiver<Value>,
}

impl FakeService {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (tx, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || serve(stream, &tx));
            }
        });
        Self { url, requests }
    }

    /// The next command request the CLI sent, if it sent one.
    fn next_command(&self) -> Option<Value> {
        self.requests.recv_timeout(Duration::from_millis(200)).ok()
    }
}

fn serve(stream: TcpStream, tx: &mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default().to_owned(),
        parts.next().unwrap_or_default().to_owned(),
    );
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let (status, reply) = match (method.as_str(), path.as_str()) {
        ("GET", "/api/version") => (
            200,
            json!({"type": "version", "data": {"api_version": 2, "supported": [1, 2], "service_version": "fake"}}),
        ),
        ("POST", p) if p.starts_with("/api/v2/sessions/") && p.ends_with("/commands") => {
            let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let data = request["data"].clone();
            let _ = tx.send(data.clone());
            (
                200,
                json!({"type": "command_result", "data": {
                    "command_id": "0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70", "session_id": "a3f", "command": data["command"], "ok": true,
                    "output": {"args": data["args"]}, "text": "ran on the fake device", "files": [],
                    "started_at": "2026-09-26T00:00:00Z", "duration_ms": 1, "idle_ends_at": null,
                }}),
            )
        }
        _ => (
            404,
            json!({"type": "error", "data": {"code": "internal", "message": format!("the fake service has no {method} {path}")}}),
        ),
    };
    let body = reply.to_string();
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// A signed-in Silicon's state directory, and a way to run the CLI in it.
struct Cli {
    home: PathBuf,
    service: FakeService,
}

impl Cli {
    fn new(test: &str) -> Self {
        let home = std::env::temp_dir().join(format!("extend-cli-it-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".extend")).unwrap();
        let service = FakeService::start();
        let auth = json!({
            "format": 4, "accounts_url": service.url, "api_url": service.url, "app_id": "extend",
            "access_token": "test-access", "refresh_token": "sar_test-refresh", "expires_at": 4_102_444_800i64,
            "uuid": "cHeF1", "id": "si:chef", "kind": "silicon", "method": "slt", "signed_in_at": 1,
        });
        std::fs::write(home.join(".extend/auth.json"), auth.to_string()).unwrap();
        Self { home, service }
    }

    fn run(&self, args: &[&str]) -> Output {
        let bin = std::env::var("EXTEND_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_extend").to_owned());
        Command::new(bin)
            .args(args)
            .current_dir(&self.home)
            .env("SILICON_HOME", &self.home)
            .env("EXTEND_API_URL", &self.service.url)
            .env("ACCOUNTS_URL", &self.service.url)
            .env("EXTEND_SESSION", "a3f")
            .env("EXTEND_TELEMETRY", "off")
            .output()
            .unwrap()
    }

    /// Runs the CLI and returns the command request the device would receive.
    fn sent(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        let request = self.service.next_command();
        assert!(
            out.status.success() && request.is_some(),
            "`extend {}` sent nothing to the device (exit {:?})\nstdout: {}\nstderr: {}",
            args.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        request.unwrap()
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn args_of(request: &Value) -> Vec<String> {
    request["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn adb_arguments_reach_the_device_exactly_as_typed() {
    let cli = Cli::new("adb-verbatim");
    for device_args in [
        &["shell", "grep", "-v", "error", "/sdcard/app.log"][..],
        &["shell", "df", "-h"],
        &["logcat", "-d", "-v", "threadtime"],
        &["shell", "dumpsys", "--help"],
        &["shell", "cmd", "stats", "--json"],
        &[
            "shell",
            "perfetto",
            "--out",
            "/data/misc/perfetto-traces/t",
            "--timeout",
            "5000",
            "--keep",
        ],
        &["shell", "--", "ls"],
    ] {
        let argv: Vec<&str> = ["adb"].iter().chain(device_args).copied().collect();
        let request = cli.sent(&argv);
        assert_eq!(request["command"], "adb");
        assert_eq!(args_of(&request), device_args, "extend {}", argv.join(" "));
        assert!(
            request.get("timeout_ms").is_none() && request.get("permanent").is_none(),
            "{request}"
        );
    }
}

#[test]
fn extend_flags_go_before_the_first_adb_argument() {
    let cli = Cli::new("adb-leading");
    let out = cli.run(&["--json", "adb", "--timeout", "60000", "--keep", "shell", "df", "-h"]);
    let request = cli.service.next_command().expect("sent to the device");
    assert_eq!(args_of(&request), ["shell", "df", "-h"]);
    assert_eq!(request["timeout_ms"], 60000);
    assert_eq!(request["permanent"], true);
    let printed: Value = serde_json::from_slice(&out.stdout).expect("--json before the adb arguments prints JSON");
    assert_eq!(printed["ok"], true);

    // `--` right after `adb` ends Extend's flags and is not sent.
    let request = cli.sent(&["adb", "--", "shell", "grep", "-v", "x", "/sdcard/app.log"]);
    assert_eq!(args_of(&request), ["shell", "grep", "-v", "x", "/sdcard/app.log"]);
    let request = cli.sent(&["adb", "--", "--help"]);
    assert_eq!(args_of(&request), ["--help"]);

    // `-h` before the adb arguments is Extend's help, and nothing is sent.
    let out = cli.run(&["adb", "-h"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Everything after the first adb argument goes to the device")
    );
    assert!(cli.service.next_command().is_none());
}

#[test]
fn adb_pull_local_path_is_not_sent() {
    let cli = Cli::new("adb-pull");
    for argv in [
        &["adb", "pull", "/sdcard/x.bin", "--out", "x.bin"][..],
        &["adb", "pull", "/sdcard/x.bin", "x.bin"],
        &["adb", "--out", "x.bin", "pull", "/sdcard/x.bin"],
        &["adb", "pull", "--out", "x.bin", "/sdcard/x.bin"],
    ] {
        assert_eq!(
            args_of(&cli.sent(argv)),
            ["pull", "/sdcard/x.bin"],
            "extend {}",
            argv.join(" ")
        );
    }
}

#[test]
fn double_dash_escapes_other_device_commands() {
    let cli = Cli::new("double-dash");
    // The device's own parser reads `--` too, so it is forwarded.
    let request = cli.sent(&["type", "--", "-v", "--json"]);
    assert_eq!(args_of(&request), ["--", "-v", "--json"]);
    let request = cli.sent(&["terminal", "run", "--", "grep", "-v", "x", "--out", "y"]);
    assert_eq!(args_of(&request), ["run", "--", "grep", "-v", "x", "--out", "y"]);
    // Before `--`, global flags still work anywhere.
    let out = cli.run(&["snapshot", "-i", "--json"]);
    assert_eq!(args_of(&cli.service.next_command().unwrap()), ["-i"]);
    assert!(serde_json::from_slice::<Value>(&out.stdout).is_ok());
}

#[test]
fn adb_shell_paths_are_not_uploaded() {
    let cli = Cli::new("adb-no-upload");
    std::fs::write(cli.home.join("a.png"), b"not for the device").unwrap();
    let request = cli.sent(&["adb", "shell", "some-tool", "--image", "a.png"]);
    assert_eq!(args_of(&request), ["shell", "some-tool", "--image", "a.png"]);
    assert!(request.get("attachments").is_none(), "{request}");

    let request = cli.sent(&["adb", "push", "a.png", "/sdcard/a.png"]);
    assert_eq!(args_of(&request), ["push", "attachment:a.png", "/sdcard/a.png"]);
    assert_eq!(request["attachments"][0]["name"], "a.png");
}

#[test]
fn oversized_push_is_refused_with_a_way_forward() {
    let cli = Cli::new("adb-oversized");
    let big = cli.home.join("system.img");
    std::fs::File::create(&big).unwrap().set_len(4 << 30).unwrap(); // sparse
    let out = cli.run(&["adb", "push", "system.img", "/sdcard/system.img"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("system.img is 4.0 GiB; a command can carry at most 8 MiB (8388608 bytes) of local files."),
        "{stderr}"
    );
    assert!(stderr.contains("split -b 8m"), "{stderr}");
    assert!(cli.service.next_command().is_none());
}

#[test]
fn install_and_push_refuse_inputs_that_are_not_local_files() {
    let cli = Cli::new("not-local");
    for (argv, says) in [
        (
            &["install", "com.example.app", "0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70"][..],
            // Suggested to a Silicon, so it names the Team.
            "extend file get 0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70 --out ./app.apk",
        ),
        (
            &["install", "com.example.app", "https://briefcase.example/f/abc"],
            "got a link, https://briefcase.example/f/abc",
        ),
        (
            &["install", "com.example.app", "./does-not-exist.apk"],
            "There is no file at ./does-not-exist.apk on this computer",
        ),
        (
            &["reinstall", "com.example.app", "./does-not-exist.apk"],
            "`extend reinstall` sends the APK from this computer",
        ),
        (
            &["adb", "install", "./does-not-exist.apk"],
            "`extend adb install` sends the APK from this computer",
        ),
        (
            &["adb", "push", "./does-not-exist.bin", "/sdcard/x"],
            "`extend adb push` sends a file from this computer",
        ),
    ] {
        let out = cli.run(argv);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "extend {}: {stderr}", argv.join(" "));
        assert!(stderr.contains(says), "extend {}: {stderr}", argv.join(" "));
        assert!(
            cli.service.next_command().is_none(),
            "extend {} was sent to the device",
            argv.join(" ")
        );
    }

    // A local APK is still sent, and the package name is never read as a file.
    std::fs::write(cli.home.join("app.apk"), b"PK").unwrap();
    std::fs::write(cli.home.join("com.example.app"), b"not the APK").unwrap();
    let request = cli.sent(&["install", "com.example.app", "app.apk"]);
    assert_eq!(args_of(&request), ["com.example.app", "attachment:app.apk"]);
    assert_eq!(request["attachments"].as_array().unwrap().len(), 1);
}

#[test]
fn install_help_offers_only_a_local_apk() {
    let cli = Cli::new("install-help");
    let out = cli.run(&["install", "--help"]);
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        help.contains("Usage:\n  extend install <package> <path.apk>\n"),
        "{help}"
    );
    assert!(!help.contains("file_id|path"), "{help}");
}
