//! A scriptable fake Extend service, which is also the fake Silicon Accounts (one port answers both),
//! and a way to run the real `extend` binary against it in its own state directory.
//!
//! `EXTEND_TEST_BIN` runs the checks against another build of the CLI instead of this one.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    /// The JSON body, or `Null`.
    pub body: Value,
    /// The raw body (form bodies are read with [`Req::form`]).
    pub raw: String,
}

impl Req {
    pub fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or_default()
    }
    pub fn query(&self, key: &str) -> Option<String> {
        let q = self.path.split_once('?')?.1;
        url_decode_pairs(q).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// A form field of an `application/x-www-form-urlencoded` body.
    pub fn form(&self, key: &str) -> Option<String> {
        url_decode_pairs(&self.raw)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

fn url_decode_pairs(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub fn ok(kind: &str, data: Value) -> Resp {
    Resp {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: json!({"type": kind, "data": data}).to_string().into_bytes(),
    }
}

pub fn answer(status: u16, kind: &str, data: Value) -> Resp {
    Resp {
        status,
        ..ok(kind, data)
    }
}

pub fn err(status: u16, code: &str, message: &str) -> Resp {
    Resp {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: json!({"type": "error", "data": {"code": code, "message": message, "hint": format!("fake hint for {code}"), "request_id": "req-0192"}})
            .to_string()
            .into_bytes(),
    }
}

pub fn no_content() -> Resp {
    Resp {
        status: 204,
        headers: vec![],
        body: vec![],
    }
}

/// A plain JSON answer, the way Silicon Accounts' OAuth endpoints answer.
pub fn raw_json(status: u16, v: Value) -> Resp {
    Resp {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: v.to_string().into_bytes(),
    }
}

/// An RFC 6749 error from Silicon Accounts.
pub fn oauth_error(error: &str, description: &str) -> Resp {
    raw_json(400, json!({"error": error, "error_description": description}))
}

/// The permanent uuid the fake gives an account.
pub fn uuid_of(member: &str) -> String {
    match member {
        "c:alice" => "aLiCe".into(),
        "c:bob" => "bOb12".into(),
        "si:chef" => "cHeF1".into(),
        "si:sous" => "sOuS1".into(),
        "si:scout" => "sCoUt".into(),
        other => format!("u{}", other.len()),
    }
}

/// A Silicon Accounts token response for `member`, with these tokens.
pub fn tokens(member: &str, access: &str, refresh: &str) -> Value {
    let silicon = member.starts_with("si:");
    let mut account = json!({"uuid": uuid_of(member), "membership_id": format!("extend:{}", uuid_of(member)),
        "kind": if silicon { "silicon" } else { "carbon" }, "id": member, "display_name": "", "pfp_url": "", "version": 1});
    if silicon {
        account["custodian"] = json!({"uuid": "aLiCe", "id": "c:alice"});
    }
    json!({"access_token": access, "token_type": "Bearer", "expires_in": 1800, "refresh_token": refresh,
           "refresh_token_expires_at": "2029-03-25T02:31:52.745Z", "scope": "profile",
           "membership_id": format!("extend:{}", uuid_of(member)), "account": account})
}

type Handler = dyn Fn(&Req) -> Option<Resp> + Send + Sync;

/// A fake service: `routes` answers what it knows (after the version handshake and, unless a
/// route answers it, `GET /api/v2/accounts`, which names this fake as the Silicon Accounts it
/// trusts); everything it receives is recorded.
pub struct Fake {
    pub url: String,
    pub seen: Arc<Mutex<Vec<Req>>>,
}

impl Fake {
    pub fn start(routes: impl Fn(&Req) -> Option<Resp> + Send + Sync + 'static) -> Self {
        Self::start_with_version(
            json!({"api_version": 2, "supported": [1, 2], "service_version": "fake"}),
            routes,
        )
    }

    pub fn start_with_version(version: Value, routes: impl Fn(&Req) -> Option<Resp> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Handler> = Arc::new(routes);
        let (seen2, version, me) = (seen.clone(), Arc::new(version), url.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (routes, seen, version, me) = (routes.clone(), seen2.clone(), version.clone(), me.clone());
                std::thread::spawn(move || serve(stream, &*routes, &seen, &version, &me));
            }
        });
        Self { url, seen }
    }

    pub fn requests(&self, method: &str, path: &str) -> Vec<Req> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path_only() == path)
            .cloned()
            .collect()
    }
}

fn serve(stream: TcpStream, routes: &Handler, seen: &Mutex<Vec<Req>>, version: &Value, me: &str) {
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
        raw: String::from_utf8_lossy(&body).into_owned(),
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
            if req.method == "GET" && req.path_only() == "/api/v2/accounts" {
                ok(
                    "accounts",
                    json!({"app_id": "extend", "accounts_url": me, "api_base_url": me, "website_url": "https://w",
                           "docs_url": "https://d", "repository_url": "https://r", "ting_enabled": true}),
                )
            } else {
                err(
                    404,
                    "unknown_command",
                    &format!("the fake has no {} {}", req.method, req.path),
                )
            }
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

pub fn bin() -> String {
    std::env::var("EXTEND_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_extend").to_owned())
}

/// One state directory, and a way to run the CLI in it.
pub struct Cli {
    pub home: PathBuf,
    pub url: String,
    pub accounts: String,
    pub env: Vec<(String, String)>,
}

impl Cli {
    pub fn new(test: &str, url: &str) -> Self {
        let home = std::env::temp_dir().join(format!("extend-cli-behaviour-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".extend")).unwrap();
        Self {
            home,
            url: url.to_owned(),
            accounts: url.to_owned(),
            env: Vec::new(),
        }
    }

    pub fn state(&self) -> PathBuf {
        self.home.join(".extend")
    }

    pub fn auth(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.state().join("auth.json")).unwrap()).unwrap()
    }

    /// Signed in as `member` (no service call): a sign-in saved the way `extend login` saves one.
    pub fn signed_in(self, member: &str) -> Self {
        self.signed_in_with(member, "test-access", "sar_test-refresh", 4_102_444_800)
    }

    pub fn signed_in_with(self, member: &str, access: &str, refresh: &str, expires_at: i64) -> Self {
        let silicon = member.starts_with("si:");
        let mut auth = json!({
            "format": 4, "accounts_url": self.accounts, "api_url": self.url, "app_id": "extend",
            "access_token": access, "refresh_token": refresh, "expires_at": expires_at,
            "refresh_expires_at": 1_900_000_000i64, "uuid": uuid_of(member), "id": member,
            "kind": if silicon { "silicon" } else { "carbon" }, "method": if silicon { "slt" } else { "device" },
            "signed_in_at": 1_700_000_000i64,
        });
        if silicon {
            auth["custodian"] = json!({"uuid": "aLiCe", "id": "c:alice"});
        }
        std::fs::write(self.state().join("auth.json"), auth.to_string()).unwrap();
        self
    }

    /// Where the signed-in account's device sessions are cached.
    pub fn session_dir(&self) -> PathBuf {
        let uuid = self.auth()["uuid"].as_str().unwrap().to_owned();
        let hex: String = uuid.bytes().map(|b| format!("{b:02x}")).collect();
        self.state().join("sessions").join(format!("acct-{hex}"))
    }

    pub fn connected(self, sid: &str, commands: &[&str]) -> Self {
        let cache = json!({
            "session_id": sid, "device_id": "7c1e09ab", "device_name": "CLI box", "os": "linux",
            "capabilities": [], "commands": commands,
        });
        let dir = self.session_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{sid}.json")), cache.to_string()).unwrap();
        std::fs::write(dir.join("current"), sid).unwrap();
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn command(&self, args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(cwd)
            .env("SILICON_HOME", &self.home)
            .env("HOME", &self.home)
            .env("EXTEND_API_URL", &self.url)
            .env("ACCOUNTS_URL", &self.accounts)
            .env("EXTEND_TELEMETRY", "off")
            .env_remove("EXTEND_SESSION")
            .env_remove("EXTEND_TEST_SECRET")
            .env_remove("NO_COLOR");
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.run_in(args, &self.home)
    }

    pub fn run_in(&self, args: &[&str], cwd: &Path) -> Output {
        self.command(args, cwd).stdin(Stdio::null()).output().unwrap()
    }

    /// Runs with `input` on stdin.
    pub fn run_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(args, &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Runs with one of stdout/stderr going to a reader that has already gone (the pipe's read end
    /// is closed before the CLI writes); returns the exit status and what the other stream got.
    pub fn run_with_reader_gone(&self, args: &[&str], gone_stdout: bool) -> (std::process::ExitStatus, String) {
        let mut c = self.command(args, &self.home);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
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

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
pub fn json_out(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}\nstderr: {}", stdout(o), stderr(o)))
}
/// The `{"error": …}` document on stderr.
pub fn json_err(o: &Output) -> Value {
    serde_json::from_str(stderr(o).lines().next().unwrap_or_default())
        .unwrap_or_else(|e| panic!("stderr is not a JSON error ({e}): {}", stderr(o)))
}

/// `GET /api/v2/me` for `member`.
pub fn me(member: &str) -> Value {
    let silicon = member.starts_with("si:");
    let mut m = json!({"uuid": uuid_of(member), "id": member, "type": if silicon { "silicon" } else { "carbon" }});
    if silicon {
        m["custodian"] = json!({"uuid": "aLiCe", "id": "c:alice", "type": "carbon"});
    }
    m
}

pub fn device(id: &str, name: &str, online: bool) -> Value {
    json!({"device_id": id, "name": name, "os": "linux", "kind": "computer", "owner": {"type": "carbon", "id": "c:alice"},
           "visibility": "personal", "state": "ready", "online": online})
}

pub fn session(sid: &str, state: &str, commands: &[&str]) -> Value {
    json!({"session_id": sid, "device_id": "7c1e09ab", "silicon_id": "si:chef", "state": state,
           "started_at": "2026-09-27T00:00:00Z", "last_command_at": null, "idle_ends_at": null, "ended_at": null,
           "end_reason": if state == "ended" { json!("stopped_by_carbon") } else { Value::Null }, "command_count": 1,
           "device": {"device_id": "7c1e09ab", "name": "CLI box", "os": "linux", "kind": "computer",
                      "owner": {"type": "carbon", "id": "c:alice"}, "visibility": "personal", "state": "ready", "online": true,
                      "missing": [{"capability": "input.remote", "reason": "Only TVs have a remote."}]},
           "capabilities": [], "commands": commands})
}

pub const FILE_ID: &str = "0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70";

pub fn file_info(url: &str) -> Value {
    json!({"file_id": FILE_ID, "name": "shot.png", "kind": "screenshot", "content_type": "image/png", "size_bytes": 11,
           "url": url, "self_destruct_at": null, "permanent": false, "created_by": "si:chef"})
}

pub fn command_result(files: Value, warnings: Value) -> Value {
    json!({"command_id": FILE_ID, "session_id": "a3f", "command": "screenshot", "ok": true, "output": null, "text": "took a screenshot",
           "files": files, "error": null, "started_at": "2026-09-27T00:00:00Z", "duration_ms": 3, "idle_ends_at": null, "warnings": warnings})
}

/// `device(...)` with 1.1 fields added.
pub fn device_with(id: &str, name: &str, extra: Value) -> Value {
    let mut d = device(id, name, true);
    d.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    d
}

pub fn wake(asks: i64, extra: Value) -> Value {
    let mut w = json!({
        "wake_id": "0192f3a4-0000-7000-8000-000000000001", "device_id": "0d44e1f2", "from": "si:chef",
        "to": "c:alice", "reason": "Need the TV on", "created_at": "2026-09-27T10:00:00Z", "last_asked_at": "2026-09-27T10:06:00Z",
        "asks": asks, "expires_at": "2026-09-27T10:36:00Z", "state": "open", "wake_detectable": true, "device_notice": "sent",
        "ting": "delivered"
    });
    w.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    w
}
