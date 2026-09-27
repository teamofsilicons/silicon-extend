//! Runs the real `extend` binary against a scriptable fake Extend service: the JSON convention,
//! test-environment selection, paging, help while connected, grammar and settings, moving the
//! state directory, file downloads through Extend, and the compatibility check.
//!
//! `EXTEND_TEST_BIN` runs the checks against another build of the CLI instead of this one.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

impl Req {
    fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or_default()
    }
    fn query(&self, key: &str) -> Option<String> {
        let q = self.path.split_once('?')?.1;
        q.split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_owned())
    }
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn ok(kind: &str, data: Value) -> Resp {
    Resp {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: json!({"type": kind, "data": data}).to_string().into_bytes(),
    }
}

fn err(status: u16, code: &str, message: &str) -> Resp {
    Resp {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: json!({"type": "error", "data": {"code": code, "message": message, "hint": format!("fake hint for {code}"), "request_id": "req-0192"}})
            .to_string()
            .into_bytes(),
    }
}

type Handler = dyn Fn(&Req) -> Option<Resp> + Send + Sync;

/// A fake service: `routes` answers what it knows (after the version handshake); everything it
/// receives is recorded.
struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Req>>>,
}

impl Fake {
    fn start(routes: impl Fn(&Req) -> Option<Resp> + Send + Sync + 'static) -> Self {
        Self::start_with_version(
            json!({"api_version": 1, "supported": [1], "service_version": "fake"}),
            routes,
        )
    }

    fn start_with_version(version: Value, routes: impl Fn(&Req) -> Option<Resp> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Handler> = Arc::new(routes);
        let (seen2, version) = (seen.clone(), Arc::new(version));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (routes, seen, version) = (routes.clone(), seen2.clone(), version.clone());
                std::thread::spawn(move || serve(stream, &*routes, &seen, &version));
            }
        });
        Self { url, seen }
    }

    fn requests(&self, method: &str, path: &str) -> Vec<Req> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path_only() == path)
            .cloned()
            .collect()
    }
}

fn serve(stream: TcpStream, routes: &Handler, seen: &Mutex<Vec<Req>>, version: &Value) {
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
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let req = Req {
        method,
        path,
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    seen.lock().unwrap().push(req.clone());
    let resp = if req.method == "GET" && req.path_only() == "/api/version" {
        if version["code"].is_string() {
            Resp {
                status: 410,
                headers: vec![],
                body: json!({"type": "error", "data": version}).to_string().into_bytes(),
            }
        } else {
            ok("version", version.clone())
        }
    } else {
        routes(&req).unwrap_or_else(|| {
            err(
                404,
                "unknown_command",
                &format!("the fake has no {} {}", req.method, req.path),
            )
        })
    };
    let mut out = stream;
    let mut head = format!(
        "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\nx-request-id: req-0192\r\n",
        resp.status,
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&resp.body);
}

fn bin() -> String {
    std::env::var("EXTEND_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_extend").to_owned())
}

/// One state directory, and a way to run the CLI in it.
struct Cli {
    home: PathBuf,
    url: String,
    env: Vec<(String, String)>,
}

impl Cli {
    fn new(test: &str, url: &str) -> Self {
        let home = std::env::temp_dir().join(format!("extend-cli-behaviour-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".extend")).unwrap();
        Self {
            home,
            url: url.to_owned(),
            env: Vec::new(),
        }
    }

    fn state(&self) -> PathBuf {
        self.home.join(".extend")
    }

    /// Signs in as `member` (no service call; the fake accepts any token).
    fn signed_in(self, member: &str) -> Self {
        let auth = json!({
            "api_url": self.url, "access_token": "test-access", "refresh_token": "test-refresh", "expires_at": 4_102_444_800i64,
            "member_id": member, "member_kind": if member.starts_with("si:") { "silicon" } else { "carbon" }, "teams": ["acme"], "team": "acme",
        });
        std::fs::write(self.state().join("auth.json"), auth.to_string()).unwrap();
        self
    }

    fn connected(self, sid: &str, commands: &[&str]) -> Self {
        let cache = json!({
            "session_id": sid, "device_id": "7c1e09ab", "device_name": "CLI box", "os": "linux",
            "capabilities": [], "commands": commands, "test_id": null,
        });
        let dir = self.state().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{sid}.json")), cache.to_string()).unwrap();
        std::fs::write(dir.join("current"), sid).unwrap();
        self
    }

    fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(args, &self.home)
    }

    fn run_in(&self, args: &[&str], cwd: &Path) -> Output {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(cwd)
            .env("SILICON_HOME", &self.home)
            .env("EXTEND_API_URL", &self.url)
            .env("EXTEND_TELEMETRY", "off")
            .env_remove("EXTEND_SESSION")
            .env_remove("EXTEND_TEST_SECRET")
            .env_remove("NO_COLOR");
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// Runs with one of stdout/stderr going to a reader that has already gone (the pipe's read end
    /// is closed before the CLI writes); returns the exit status and what the other stream got.
    fn run_with_reader_gone(&self, args: &[&str], gone_stdout: bool) -> (std::process::ExitStatus, String) {
        use std::process::Stdio;
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(&self.home)
            .env("SILICON_HOME", &self.home)
            .env("EXTEND_API_URL", &self.url)
            .env("EXTEND_TELEMETRY", "off")
            .env_remove("EXTEND_SESSION")
            .env_remove("EXTEND_TEST_SECRET")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let (gone, mut kept): (Box<dyn std::any::Any>, Box<dyn std::io::Read>) = if gone_stdout {
            (Box::new(child.stdout.take()), Box::new(child.stderr.take().unwrap()))
        } else {
            (Box::new(child.stderr.take()), Box::new(child.stdout.take().unwrap()))
        };
        drop(gone);
        let mut text = String::new();
        kept.read_to_string(&mut text).unwrap();
        (child.wait().unwrap(), text)
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn json_out(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}\nstderr: {}", stdout(o), stderr(o)))
}

fn me(member: &str) -> Value {
    json!({"authenticated": true, "member": {"type": if member.starts_with("si:") { "silicon" } else { "carbon" }, "id": member}, "teams": ["acme"], "team": "acme", "team_role": null})
}

fn device(id: &str, name: &str, online: bool) -> Value {
    json!({"device_id": id, "name": name, "os": "linux", "kind": "computer", "owner": {"type": "carbon", "id": "c:alice"},
           "visibility": "team", "state": "ready", "online": online})
}

fn session(sid: &str, state: &str, commands: &[&str]) -> Value {
    json!({"session_id": sid, "device_id": "7c1e09ab", "silicon_id": "si:chef", "state": state,
           "started_at": "2026-09-27T00:00:00Z", "last_command_at": null, "idle_ends_at": null, "ended_at": null,
           "end_reason": if state == "ended" { json!("stopped_by_carbon") } else { Value::Null }, "command_count": 1,
           "device": {"device_id": "7c1e09ab", "name": "CLI box", "os": "linux", "kind": "computer",
                      "owner": {"type": "carbon", "id": "c:alice"}, "visibility": "team", "state": "ready", "online": true,
                      "missing": [{"capability": "input.remote", "reason": "Only TVs have a remote."}]},
           "capabilities": [], "commands": commands})
}

const FILE_ID: &str = "0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70";

fn file_info(url: &str) -> Value {
    json!({"file_id": FILE_ID, "name": "shot.png", "kind": "screenshot", "content_type": "image/png", "size_bytes": 11,
           "url": url, "self_destruct_at": null, "permanent": false})
}

fn command_result(files: Value, warnings: Value) -> Value {
    json!({"command_id": FILE_ID, "session_id": "a3f", "command": "screenshot", "ok": true, "output": null, "text": "took a screenshot",
           "files": files, "error": null, "started_at": "2026-09-27T00:00:00Z", "duration_ms": 3, "idle_ends_at": null, "warnings": warnings})
}

// ───────────────────────────── The JSON convention ─────────────────────────────

#[test]
fn json_prints_the_data_itself_and_errors_on_stderr() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/iam") => Some(ok(
            "iam",
            json!({"app_id": "extend", "iam_base_url": "https://iam.example", "api_base_url": "http://x", "website_url": "https://w", "docs_url": "https://d", "repository_url": "https://r"}),
        )),
        ("GET", "/api/v1/auth/me") => Some(ok("me", me("si:chef"))),
        ("GET", "/api/v1/devices/7c1e09ab") => {
            Some(err(404, "device_not_found", "No device 7c1e09ab is visible to you."))
        }
        _ => None,
    });
    let cli = Cli::new("json", &fake.url);
    let o = cli.run(&["iam", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(json_out(&o)["app_id"], "extend", "app_id at the top level");
    assert!(json_out(&o).get("ok").is_none() && json_out(&o).get("data").is_none());

    // Not signed in: the check worked, so exit 0 in both modes.
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(json_out(&o)["authenticated"], false);
    assert!(json_out(&o)["reason"].as_str().unwrap().contains("No saved login"));
    let o = cli.run(&["login", "status"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).starts_with("Not signed in"), "{}", stdout(&o));

    let cli = cli.signed_in("si:chef");
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(json_out(&o)["authenticated"], true);
    assert_eq!(json_out(&o)["member"]["id"], "si:chef");

    // A failure: nothing on stdout, one {"error": {...}} document on stderr.
    let o = cli.run(&["--json", "device", "show", "7c1e09ab"]);
    assert_eq!(o.status.code(), Some(5));
    assert!(o.stdout.is_empty(), "stdout: {}", stdout(&o));
    let e: Value = serde_json::from_str(stderr(&o).lines().next().unwrap()).unwrap();
    assert_eq!(e["error"]["code"], "device_not_found");
    assert_eq!(e["error"]["exit_code"], 5);
    assert_eq!(e["error"]["request_id"], "req-0192");
    assert_eq!(e["error"]["hint"], "fake hint for device_not_found");
    assert!(e["error"]["message"].as_str().unwrap().contains("No device 7c1e09ab"));
    // Usage errors too.
    let o = cli.run(&["--json", "device", "ls", "--onlinee"]);
    let e: Value = serde_json::from_str(stderr(&o).lines().next().unwrap()).unwrap();
    assert_eq!(
        (e["error"]["code"].as_str(), e["error"]["exit_code"].as_i64()),
        (Some("invalid_input"), Some(2))
    );
}

#[test]
fn an_expired_login_reads_as_not_authenticated() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/auth/me") => Some(err(401, "token_expired", "The access token expired.")),
        ("POST", "/api/v1/auth/refresh") => Some(err(401, "token_expired", "The IAM session ended.")),
        _ => None,
    });
    let cli = Cli::new("expired", &fake.url).signed_in("si:chef");
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(json_out(&o)["authenticated"], false);
    assert_eq!(json_out(&o)["code"], "token_expired");
}

// ───────────────────────────── Test environments ─────────────────────────────

const ENV_A: &str = "9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c";
const ENV_B: &str = "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
const SECRET: &str = "ask_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";

fn testing_env(id: &str) -> Value {
    json!({"environment_id": id, "name": "checkout-e2e", "state": "ready", "paired_devices": 0, "device_limit": 5})
}

#[test]
fn the_test_environment_is_named_even_when_nothing_ran() {
    let cli = Cli::new("trailer", "http://127.0.0.1:9");
    for args in [
        &["--test", ENV_A, "device", "ls"][..],
        &["--json", "--test", ENV_A, "device", "ls"],
        &["--test", ENV_A, "device", "ls", "--timeout", "5"],
    ] {
        let o = cli.run(args);
        let err = stderr(&o);
        assert_ne!(o.status.code(), Some(0), "{args:?}");
        assert_eq!(
            err.lines().last().unwrap(),
            format!("[test environment: unknown ({ENV_A}) as not signed in; nothing ran]"),
            "{args:?}: {err}"
        );
    }
    let o = cli.run(&["--test", ENV_A, "device", "ls"]);
    assert_eq!(o.status.code(), Some(11));
    assert!(
        stderr(&o).contains("EXTEND_TEST_SECRET=<secret> extend --test"),
        "{}",
        stderr(&o)
    );
    let o = cli.run(&["--test", "not-a-uuid", "device", "ls"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o)
            .lines()
            .last()
            .unwrap()
            .starts_with("[test environment: unknown (not-a-uuid)")
    );
}

#[test]
fn a_test_secret_must_belong_to_the_test_id() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/testing-environment") => Some(ok("testing_environment", testing_env(ENV_A))),
        ("POST", "/api/v1/auth/login") => Some(ok(
            "auth",
            json!({"access_token": "a", "refresh_token": "r", "token_type": "Bearer", "expires_in": 900,
                   "member": {"type": "silicon", "id": "si:chef"}, "teams": ["acme"], "testing_environment": testing_env(ENV_A)}),
        )),
        ("GET", "/api/v1/auth/me") => Some(ok("me", me("si:chef"))),
        _ => None,
    });
    // `config test add` refuses a secret of another environment, and saves nothing.
    let cli = Cli::new("secret-id", &fake.url);
    let mut child = Command::new(bin())
        .args(["config", "test", "add", ENV_B])
        .env("SILICON_HOME", &cli.home)
        .env("EXTEND_API_URL", &fake.url)
        .env("EXTEND_TELEMETRY", "off")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(SECRET.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(11), "{}", stderr(&o));
    assert!(
        stderr(&o).contains(&format!(
            "That secret belongs to test environment \"checkout-e2e\" ({ENV_A}), not {ENV_B}"
        )),
        "{}",
        stderr(&o)
    );
    assert!(!cli.state().join(format!("test/{ENV_B}.json")).exists());

    // EXTEND_TEST_SECRET works without `config test add`, once it is checked against the id.
    let wrong = Cli::new("secret-env-wrong", &fake.url).env("EXTEND_TEST_SECRET", SECRET);
    let o = wrong.run(&["--test", ENV_B, "login", "si:chef"]);
    assert_eq!(o.status.code(), Some(11), "{}", stderr(&o));
    assert!(stderr(&o).contains(&format!(
        "EXTEND_TEST_SECRET belongs to test environment \"checkout-e2e\" ({ENV_A}), not {ENV_B}"
    )));
    assert!(fake.requests("POST", "/api/v1/auth/login").is_empty(), "nothing ran");

    // Set without --test, it would reach production instead: refused, and nothing is sent.
    let before = fake.seen.lock().unwrap().len();
    let o = wrong.run(&["device", "ls"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("EXTEND_TEST_SECRET is set, but this command has no --test"),
        "{}",
        stderr(&o)
    );
    assert_eq!(fake.seen.lock().unwrap().len(), before, "nothing reached Extend");

    let right = Cli::new("secret-env-right", &fake.url).env("EXTEND_TEST_SECRET", SECRET);
    let o = right.run(&["--test", ENV_A, "login", "si:chef"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        stderr(&o).lines().last().unwrap(),
        format!("[test environment: checkout-e2e ({ENV_A}) as si:chef]")
    );
    let login = fake.requests("POST", "/api/v1/auth/login");
    assert_eq!(login[0].headers["x-testing-application-secret"], SECRET);
    let saved = std::fs::read_to_string(right.state().join(format!("test/{ENV_A}.json"))).unwrap();
    assert!(
        !saved.contains(SECRET),
        "the secret from the environment is never written: {saved}"
    );
    // The next command doesn't check again, and the login is kept.
    let checks = fake.requests("GET", "/api/v1/testing-environment").len();
    let o = right.run(&["--test", ENV_A, "--json", "login", "status"]);
    assert_eq!(json_out(&o)["authenticated"], true, "{}", stderr(&o));
    assert_eq!(fake.requests("GET", "/api/v1/testing-environment").len(), checks);
}

// ───────────────────────────── device ls ─────────────────────────────

#[test]
fn device_ls_reads_every_page() {
    let fake = Fake::start(|r| {
        if r.path_only() != "/api/v1/devices" {
            return None;
        }
        let online_only = r.query("online").as_deref() == Some("true");
        let page = match r.query("cursor").as_deref() {
            None => {
                json!({"items": [device("00000001", "one", true), device("00000002", "two", false)], "next_cursor": "00000002"})
            }
            Some("00000002") => json!({"items": [device("00000003", "three", true)], "next_cursor": null}),
            _ => return None,
        };
        let mut page = page;
        if online_only {
            // An older service filtered after paging, leaving offline devices out of a full page.
            page["items"] = json!(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|d| d["online"] == true)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        Some(ok("devices", page))
    });
    let cli = Cli::new("device-ls", &fake.url).signed_in("si:chef");
    let o = cli.run(&["device", "ls"]);
    assert!(o.status.success(), "{}", stderr(&o));
    for name in ["one", "two", "three"] {
        assert!(stdout(&o).contains(name), "{}", stdout(&o));
    }
    let o = cli.run(&["device", "ls", "--online", "--json"]);
    let names: Vec<String> = json_out(&o)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["one", "three"]);
    assert_eq!(json_out(&o)["next_cursor"], Value::Null);
    let asked = fake.requests("GET", "/api/v1/devices");
    assert!(asked.iter().all(|r| r.query("limit").as_deref() == Some("100")));
    assert!(asked.iter().any(|r| r.query("online").as_deref() == Some("true")));
}

#[test]
fn device_ls_says_when_it_stops_early() {
    let fake = Fake::start(|r| {
        (r.path_only() == "/api/v1/devices").then(|| {
            ok(
                "devices",
                json!({"items": [device("00000001", "loop", true)], "next_cursor": "00000001"}),
            )
        })
    });
    let cli = Cli::new("device-ls-loop", &fake.url).signed_in("si:chef");
    let o = cli.run(&["device", "ls"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("This list is incomplete"), "{}", stdout(&o));
    let o = cli.run(&["device", "ls", "--json"]);
    assert_eq!(json_out(&o)["next_cursor"], "00000001");
}

// ───────────────────────────── Help while connected ─────────────────────────────

#[test]
fn help_follows_the_connected_device() {
    let commands = Arc::new(Mutex::new(vec!["snapshot"]));
    let state = Arc::new(Mutex::new("active"));
    let (c2, s2) = (commands.clone(), state.clone());
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/sessions/a3f") => {
            Some(ok("session", session("a3f", *s2.lock().unwrap(), &c2.lock().unwrap())))
        }
        ("POST", "/api/v1/sessions/a3f/commands") => {
            let mut result = command_result(json!([]), json!([]));
            if *s2.lock().unwrap() == "ended" {
                result["ok"] = json!(false);
                result["error"] =
                    json!({"code": "session_ended", "message": "Session a3f ended: the device's Carbon stopped it."});
            }
            Some(ok("command_result", result))
        }
        _ => None,
    });
    let cli = Cli::new("help-connected", &fake.url).signed_in("si:chef");
    let o = cli.run(&["session", "connect", "a3f"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let help = stdout(&cli.run(&["--help"]));
    assert!(help.contains("connected to session a3f on CLI box (linux)"), "{help}");
    assert!(!help.contains("tv-remote"), "{help}");
    let tv = stdout(&cli.run(&["tv-remote", "--help"]));
    assert!(
        tv.contains("Not available on CLI box (linux) in session a3f: it needs input.remote")
            && tv.contains("Only TVs have a remote."),
        "{tv}"
    );

    // `session status` refreshes the list.
    commands.lock().unwrap().push("terminal");
    assert!(cli.run(&["session", "status"]).status.success());
    assert!(stdout(&cli.run(&["--help"])).contains("  terminal "));
    // So does each device command.
    commands.lock().unwrap().push("clipboard");
    assert!(cli.run(&["snapshot"]).status.success());
    assert!(stdout(&cli.run(&["--help"])).contains("  clipboard "));

    // A result saying the session ended disconnects.
    *state.lock().unwrap() = "ended";
    let o = cli.run(&["snapshot"]);
    assert_eq!(o.status.code(), Some(1));
    let help = stdout(&cli.run(&["--help"]));
    assert!(!help.contains("connected to session"), "{help}");
    assert!(!cli.state().join("sessions/current").exists());
}

#[test]
fn a_session_ended_error_disconnects() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v1/sessions/b4c/commands") => Some(err(
            409,
            "session_ended",
            "Session b4c ended while `snapshot` was running.",
        )),
        _ => None,
    });
    let cli = Cli::new("ended-error", &fake.url)
        .signed_in("si:chef")
        .connected("b4c", &["snapshot"]);
    let o = cli.run(&["snapshot"]);
    assert_eq!(o.status.code(), Some(6), "{}", stderr(&o));
    assert!(!stdout(&cli.run(&["--help"])).contains("connected to session"));
}

// ───────────────────────────── Grammar, settings, colour, -v ─────────────────────────────

#[test]
fn unknown_flags_and_settings_are_refused_with_the_choices() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v1/devices/7c1e09ab/requests") => Some(ok(
            "request",
            json!({"request_id": FILE_ID, "device_id": "7c1e09ab", "from": "si:sous", "to": "si:chef", "reason": r.body["data"]["reason"],
                   "created_at": "2026-09-27T00:00:00Z", "delivery": "delivered"}),
        )),
        _ => None,
    });
    let cli = Cli::new("grammar", &fake.url).signed_in("si:sous");
    let o = cli.run(&["device", "ls", "--onlinee"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("`extend device ls` has no flag --onlinee"),
        "{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains("Did you mean --online?"), "{}", stderr(&o));
    let o = cli.run(&["login", "status", "--bogus"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("takes no flags of its own"), "{}", stderr(&o));
    let o = cli.run(&["--bogus", "device", "ls"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("--bogus is not a global flag"), "{}", stderr(&o));

    // A flag's value is the value, even when it looks like a global flag.
    let o = cli.run(&["request", "send", "7c1e09ab", "--reason", "-h"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        fake.requests("POST", "/api/v1/devices/7c1e09ab/requests")[0].body["data"]["reason"],
        "-h"
    );

    for (args, says) in [
        (
            &["config", "set", "color", "purple"][..],
            "color can't be \"purple\": it is one of auto, always, never",
        ),
        (
            &["config", "set", "screenshot_scale", "7"],
            "it is a number from 0.01 to 1",
        ),
        (&["config", "set", "api_url", "ftp://x"], "it must be an https URL"),
        (
            &["config", "set", "self_destruct", "45d"],
            "outside 1 minute to 30 days",
        ),
        (&["config", "get", "bogus"], "unknown setting \"bogus\""),
        (&["config", "unset", "bogus"], "unknown setting \"bogus\""),
    ] {
        let o = cli.run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(stderr(&o).contains(says), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("  hint: "),
            "{args:?}: every usage error says what to do"
        );
    }
    let o = cli.run(&["config", "get", "telemetry"]);
    assert_eq!(stdout(&o).trim(), "on (default)");
}

#[test]
fn color_follows_the_setting_and_no_color() {
    let cli = Cli::new("color", "http://127.0.0.1:9");
    let o = cli.run(&["frobnicate"]);
    assert!(!stderr(&o).contains('\x1b'), "not a terminal: no colour by default");
    assert!(cli.run(&["config", "set", "color", "always"]).status.success());
    let o = cli.run(&["frobnicate"]);
    assert!(stderr(&o).contains("\x1b[1;31merror:\x1b[0m"), "{:?}", stderr(&o));
    // `always` is the user's own choice, so it wins over NO_COLOR.
    let cli = cli.env("NO_COLOR", "1");
    assert!(stderr(&cli.run(&["frobnicate"])).contains('\x1b'));
    assert!(cli.run(&["config", "set", "color", "never"]).status.success());
    assert!(!stderr(&cli.run(&["frobnicate"])).contains('\x1b'));
    // --json is never coloured.
    assert!(cli.run(&["config", "set", "color", "always"]).status.success());
    assert!(!stderr(&cli.run(&["--json", "frobnicate"])).contains('\x1b'));
}

#[test]
fn verbose_shows_each_call_its_time_and_request_id() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/devices/7c1e09ab") => Some(ok("device", device("7c1e09ab", "box", true))),
        ("GET", "/api/v1/devices/0000dead") => Some(err(404, "device_not_found", "No device 0000dead.")),
        _ => None,
    });
    let cli = Cli::new("verbose", &fake.url).signed_in("c:alice");
    let o = cli.run(&["-v", "device", "show", "7c1e09ab"]);
    let err = stderr(&o);
    assert!(
        err.contains("[extend] GET ") && err.contains("/api/version: API v1 in "),
        "{err}"
    );
    assert!(err.contains("[extend] GET /api/v1/devices/7c1e09ab: ok in "), "{err}");
    assert!(err.contains("[extend] finished in "), "{err}");
    let o = cli.run(&["--verbose", "device", "show", "0000dead"]);
    assert!(
        stderr(&o).contains("[extend] GET /api/v1/devices/0000dead: 404 device_not_found in ")
            && stderr(&o).contains(", request req-0192"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&cli.run(&["device", "show", "7c1e09ab"])).contains("[extend]"));
}

/// `extend -v … 2>&1 | grep -q …` and `extend … | head -1`: once the reader has what it wanted it
/// goes away, and the CLI's later writes hit a broken pipe. That must not turn into a panic (exit
/// 101), which also made e2e/cli-e2e.sh's `-v` check fail under `set -o pipefail`.
#[test]
fn a_reader_that_goes_away_does_not_crash_the_command() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v1/devices/7c1e09ab") => Some(ok("device", device("7c1e09ab", "Lab box", true))),
        _ => None,
    });
    let cli = Cli::new("reader-gone", &fake.url).signed_in("c:alice");
    let (status, out) = cli.run_with_reader_gone(&["-v", "device", "show", "7c1e09ab"], false);
    assert_eq!(status.code(), Some(0), "stderr gone; stdout: {out}");
    assert!(out.contains("Lab box"), "{out}");
    let (status, err) = cli.run_with_reader_gone(&["-v", "device", "show", "7c1e09ab"], true);
    assert_eq!(status.code(), Some(0), "stdout gone; stderr: {err}");
    assert!(!err.contains("panicked"), "{err}");
    assert!(
        err.contains("[extend] finished in ") && err.contains("with exit code 0"),
        "{err}"
    );
    let (status, err) = cli.run_with_reader_gone(&["--help"], true);
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

// ───────────────────────────── config home ─────────────────────────────

#[test]
fn config_home_moves_the_login_and_settings() {
    let fake = Fake::start(|r| (r.path_only() == "/api/v1/auth/me").then(|| ok("me", me("si:chef"))));
    let cli = Cli::new("home-move", &fake.url).signed_in("si:chef");
    assert!(cli.run(&["config", "set", "telemetry", "off"]).status.success());
    let target = cli.home.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    let o = cli.run(&["config", "home", target.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("Moved the login for si:chef, 1 setting(s)"),
        "{}",
        stdout(&o)
    );

    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(
        json_out(&o)["authenticated"],
        true,
        "the login came along: {}",
        stderr(&o)
    );
    assert_eq!(stdout(&cli.run(&["config", "get", "telemetry"])).trim(), "off");
    let new_root = target.canonicalize().unwrap().join(".extend");
    assert!(new_root.join("auth.json").exists() && new_root.join("config.toml").exists());
    assert!(!cli.state().join("auth.json").exists(), "the old copy is gone");
    assert!(stdout(&cli.run(&["docs"])).contains(&new_root.display().to_string()));

    let o = cli.run(&["config", "home", "/definitely/not/here"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("not a directory: /definitely/not/here") && stderr(&o).contains("hint: "));

    // Back home: refused while the default still holds state, unless asked to switch.
    std::fs::write(cli.state().join("config.toml"), "output = \"json\"\n").unwrap();
    let o = cli.run(&["config", "home", cli.home.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("already holds Extend state (config.toml)") && stderr(&o).contains("--use-existing"),
        "{}",
        stderr(&o)
    );
    let o = cli.run(&["config", "home", cli.home.to_str().unwrap(), "--use-existing"]);
    assert!(o.status.success(), "{}", stderr(&o));
    // Now the default's settings apply, and the moved login stays where it went.
    let o = cli.run(&["login", "status"]);
    assert_eq!(
        json_out(&o)["authenticated"],
        false,
        "output=json from the default state"
    );
}

// ───────────────────────────── Files ─────────────────────────────

fn file_fake(content: Resp) -> Fake {
    let content = Arc::new(Mutex::new(Some(content)));
    Fake::start(move |r| {
        let url = "https://briefcase.example/f/shot";
        match (r.method.as_str(), r.path_only()) {
            ("GET", p) if p == format!("/api/v1/files/{FILE_ID}") => Some(ok("file", file_info(url))),
            ("GET", p) if p == format!("/api/v1/files/{FILE_ID}/content") => {
                let c = content.lock().unwrap();
                let c = c.as_ref().unwrap();
                Some(Resp {
                    status: c.status,
                    headers: c.headers.clone(),
                    body: c.body.clone(),
                })
            }
            ("POST", "/api/v1/sessions/a3f/commands") => Some(ok(
                "command_result",
                command_result(
                    json!([file_info(url)]),
                    json!([
                        "shot.png is stored, but not shared with c:alice, so they can't open it in Briefcase yet: fake."
                    ]),
                ),
            )),
            _ => None,
        }
    })
}

fn png() -> Resp {
    Resp {
        status: 200,
        headers: vec![
            ("content-type".into(), "image/png".into()),
            ("content-disposition".into(), "attachment; filename=\"shot.png\"".into()),
        ],
        body: b"PNG-BYTES!!".to_vec(),
    }
}

#[test]
fn files_download_through_extend_after_their_links() {
    let fake = file_fake(png());
    let cli = Cli::new("files", &fake.url).signed_in("si:chef");
    let o = cli.run(&["file", "get", FILE_ID]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    let link = out
        .find("https://briefcase.example/f/shot")
        .expect("the link is printed");
    assert!(link < out.find("Saved to").unwrap(), "{out}");
    assert_eq!(std::fs::read(cli.home.join("shot.png")).unwrap(), b"PNG-BYTES!!");
    let asked = fake.requests("GET", &format!("/api/v1/files/{FILE_ID}/content"));
    assert_eq!(
        asked[0].headers["authorization"], "Bearer test-access",
        "Extend's token goes to Extend"
    );

    let o = cli.run(&["file", "get", FILE_ID, "--out", "copy.png", "--json"]);
    assert_eq!(json_out(&o)["bytes"], 11);
    assert_eq!(std::fs::read(cli.home.join("copy.png")).unwrap(), b"PNG-BYTES!!");

    // A device command: links and warnings first, then the download.
    let cli = cli.connected("a3f", &["screenshot"]);
    let o = cli.run(&["screenshot", "--out", "shots/"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("took a screenshot\nscreenshot shot.png"),
        "{}",
        stdout(&o)
    );
    assert!(
        stdout(&o).contains("Saved shot.png to shots/shot.png"),
        "{}",
        stdout(&o)
    );
    assert!(
        stderr(&o).contains("warning: shot.png is stored, but not shared with c:alice"),
        "{}",
        stderr(&o)
    );
    assert_eq!(std::fs::read(cli.home.join("shots/shot.png")).unwrap(), b"PNG-BYTES!!");
    let o = cli.run(&["screenshot", "--json", "--out", "j.png"]);
    let doc = json_out(&o);
    assert_eq!(
        doc["warnings"][0].as_str().unwrap().split(',').next(),
        Some("shot.png is stored")
    );
    assert_eq!(doc["saved_to"][0], "j.png");
    assert_eq!(doc["command"], "screenshot");
}

#[test]
fn a_failed_download_still_leaves_the_link() {
    let fake = file_fake(err(403, "no_access", "Briefcase refused to read shot.png for si:chef."));
    let cli = Cli::new("files-refused", &fake.url)
        .signed_in("si:chef")
        .connected("a3f", &["screenshot"]);
    let o = cli.run(&["screenshot", "--out", "x.png"]);
    assert_eq!(o.status.code(), Some(4), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("https://briefcase.example/f/shot"),
        "the link was printed first: {}",
        stdout(&o)
    );
    assert!(
        stderr(&o).contains(&format!("Could not download shot.png ({FILE_ID}): Briefcase refused")),
        "{}",
        stderr(&o)
    );
    assert!(
        stderr(&o).contains("The file is still in Briefcase: https://briefcase.example/f/shot"),
        "{}",
        stderr(&o)
    );
    assert!(!cli.home.join("x.png").exists());

    let o = cli.run(&["--json", "screenshot", "--out", "x.png"]);
    let e: Value = serde_json::from_str(stderr(&o).lines().next().unwrap()).unwrap();
    assert_eq!(
        e["error"]["details"]["result"]["files"][0]["url"],
        "https://briefcase.example/f/shot"
    );
}

// ───────────────────────────── extend version ─────────────────────────────

fn matrix(state: &str) -> Value {
    json!({"service_version": "1.2.0", "current": 1, "supported": [1], "sunset_rule": "Sunset after 7 consecutive days with zero requests",
           "versions": [{"api_version": 1, "state": state, "deprecated_at": if state == "current" { Value::Null } else { json!("2026-09-01T00:00:00Z") },
                         "sunset_at": null, "sunset_earliest_at": if state == "deprecated" { json!("2026-10-04T00:00:00Z") } else { Value::Null },
                         "sunset_rule": "Sunset after 7 consecutive days with zero requests",
                         "compatible": {"client_crate": ">=1.0.0, <2.0.0", "cli": ">=1.0.0, <2.0.0", "device_app_min": "1.0.0"}}]})
}

#[test]
fn version_reads_the_compatibility_matrix() {
    for (state, says) in [
        (
            "current",
            "Status: current. API v1 is current, and it works with CLI >=1.0.0, <2.0.0",
        ),
        (
            "deprecated",
            "Status: deprecated. API v1 is deprecated since 2026-09-01; Extend retires it after 7 consecutive days with zero requests, no earlier than 2026-10-04.",
        ),
    ] {
        let fake = Fake::start(move |r| (r.path_only() == "/api/v1/contracts").then(|| ok("contracts", matrix(state))));
        let cli = Cli::new(&format!("version-{state}"), &fake.url);
        let o = cli.run(&["version"]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert!(stdout(&o).contains(says), "{}", stdout(&o));
        assert!(
            stdout(&o).contains("API v1 at") && stdout(&o).contains("(service 1.2.0)"),
            "{}",
            stdout(&o)
        );
        let o = cli.run(&["-V", "--json"]);
        assert_eq!(json_out(&o)["status"], state);
        assert_eq!(json_out(&o)["cli_range"], ">=1.0.0, <2.0.0");
    }

    let fake = Fake::start_with_version(
        json!({"code": "api_version_sunset", "message": "API version 1 was retired on 2026-09-20.", "hint": "Update: `honeycomb install 'extend'`."}),
        |_| None,
    );
    let o = Cli::new("version-sunset", &fake.url).run(&["version", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(json_out(&o)["status"], "sunset");
    assert!(
        json_out(&o)["message"]
            .as_str()
            .unwrap()
            .contains("retired on 2026-09-20")
    );

    let o = Cli::new("version-down", "http://127.0.0.1:9").run(&["version"]);
    assert!(o.status.success());
    assert!(
        stdout(&o).contains("API unreachable") && stdout(&o).contains("Status: unknown."),
        "{}",
        stdout(&o)
    );
}
