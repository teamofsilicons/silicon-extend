//! `bridge` — find and use the devices a Silicon has access to; pair and manage them as a Carbon.
//!
//! Built only on `silicon-bridge-client`. Stateful on disk (see `store.rs`); never asks for a
//! password. Every failure says what went wrong, why, and what to run next, and exits with a code
//! from `understanding/cli.yaml`.

mod help;
mod store;

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use bridge_protocol::capability::{self, COMMANDS};
use bridge_protocol::model::*;
use bridge_protocol::{ApiError, DeviceId, ErrorCode};
use serde_json::{Value, json};
use silicon_bridge_client::{ActivityQuery, Client, DeviceQuery, ListQuery};
use store::{Auth, Plane};

const DEFAULT_API: &str = "https://backend.bridge.teamofsilicons.com";

// ───────────────────────────── Errors ─────────────────────────────

#[derive(Debug)]
struct CliError {
    code: ErrorCode,
    message: String,
    hint: Option<String>,
    request_id: Option<String>,
    details: Value,
}

impl CliError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), hint: None, request_id: None, details: Value::Null }
    }
    fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
    fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }
    fn exit(&self) -> i32 {
        self.code.exit_code()
    }
}

impl From<silicon_bridge_client::Error> for CliError {
    fn from(e: silicon_bridge_client::Error) -> Self {
        match e {
            silicon_bridge_client::Error::Api { error, .. } => {
                let ApiError { code, message, hint, request_id, details, .. } = *error;
                Self { code, message, hint, request_id: Some(request_id).filter(|r| !r.is_empty()), details }
            }
            silicon_bridge_client::Error::Transport { url, source } => Self::new(ErrorCode::ServiceUnavailable, format!("Could not reach Silicon Bridge at {url}: {source}"))
                .hint("Check your network, or the api_url setting (`bridge config get api_url`)."),
            silicon_bridge_client::Error::Decode { status, detail } => Self::new(ErrorCode::Internal, format!("Bridge answered {status} with something this CLI can't read: {detail}"))
                .hint("Update the CLI with `honeycomb install 'bridge'`; if it persists, `bridge report` it."),
            silicon_bridge_client::Error::Invalid(m) => Self::usage(m),
        }
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(ErrorCode::InvalidInput, format!("{e:#}"))
    }
}

type R<T> = Result<T, CliError>;

// ───────────────────────────── Arguments ─────────────────────────────

#[derive(Debug, Default, Clone)]
struct Globals {
    json: bool,
    test: Option<String>,
    session: Option<String>,
    team: Option<String>,
    timeout: Option<u64>,
    help: bool,
    version: bool,
    verbose: bool,
}

fn parse_globals(argv: Vec<String>) -> R<(Globals, Vec<String>)> {
    let mut g = Globals::default();
    let mut rest = Vec::new();
    let mut it = argv.into_iter();
    let mut passthrough = false;
    while let Some(a) = it.next() {
        if passthrough {
            rest.push(a);
            continue;
        }
        let mut value = |name: &str| it.next().ok_or_else(|| CliError::usage(format!("{name} needs a value")));
        match a.as_str() {
            "--" => {
                passthrough = true;
                rest.push(a);
            }
            "--json" => g.json = true,
            "-h" | "--help" => g.help = true,
            "-V" | "--version" => g.version = true,
            "-v" | "--verbose" => g.verbose = true,
            "--test" => g.test = Some(value("--test")?),
            "--session" => g.session = Some(value("--session")?),
            "--team" => g.team = Some(value("--team")?),
            "--timeout" => {
                let v = value("--timeout")?;
                g.timeout = Some(v.parse().map_err(|_| CliError::usage(format!("--timeout takes milliseconds, got {v:?}")))?);
            }
            _ if a.starts_with("--test=") => g.test = Some(a["--test=".len()..].to_owned()),
            _ if a.starts_with("--session=") => g.session = Some(a["--session=".len()..].to_owned()),
            _ if a.starts_with("--team=") => g.team = Some(a["--team=".len()..].to_owned()),
            _ => rest.push(a),
        }
    }
    Ok((g, rest))
}

/// Pulls `--flag value` / `--flag` out of a command's own arguments.
struct Args {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(argv: &[String], takes_value: &[&str]) -> Self {
        let mut pos = Vec::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some((k, v)) = a.split_once('=').filter(|_| a.starts_with("--")) {
                flags.push((k.to_owned(), Some(v.to_owned())));
            } else if a.starts_with("--") && a.len() > 2 {
                if takes_value.contains(&a.as_str()) && i + 1 < argv.len() {
                    flags.push((a.clone(), Some(argv[i + 1].clone())));
                    i += 1;
                } else {
                    flags.push((a.clone(), None));
                }
            } else {
                pos.push(a.clone());
            }
            i += 1;
        }
        Self { pos, flags }
    }
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }
    fn value(&self, name: &str) -> Option<String> {
        self.flags.iter().rev().find(|(k, _)| k == name).and_then(|(_, v)| v.clone())
    }
    fn values(&self, name: &str) -> Vec<String> {
        self.flags.iter().filter(|(k, _)| k == name).filter_map(|(_, v)| v.clone()).collect()
    }
    fn req(&self, i: usize, what: &str, usage: &str) -> R<String> {
        self.pos.get(i).cloned().ok_or_else(|| CliError::usage(format!("missing {what}")).hint(format!("Usage: {usage}")))
    }
}

// ───────────────────────────── Context ─────────────────────────────

struct Ctx {
    g: Globals,
    plane: Plane,
    cfg: std::collections::BTreeMap<String, String>,
    client: Option<Client>,
    auth: Option<Auth>,
    test_name: Option<String>,
}

impl Ctx {
    fn api_url(&self) -> String {
        std::env::var("BRIDGE_API_URL").ok().filter(|s| !s.is_empty()).or_else(|| self.cfg.get("api_url").cloned()).unwrap_or_else(|| DEFAULT_API.into())
    }

    fn telemetry_on(&self) -> bool {
        let v = std::env::var("BRIDGE_TELEMETRY").ok().or_else(|| self.cfg.get("telemetry").cloned()).unwrap_or_else(|| "on".into());
        v != "off"
    }

    async fn client(&mut self) -> R<Client> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        let mut b = Client::builder(self.api_url()).user_agent(concat!("bridge-cli/", env!("CARGO_PKG_VERSION"))).telemetry(self.telemetry_on()).isi(std::env::var("ISI").ok());
        if let Plane::Test { secret, .. } = &self.plane {
            b = b.testing_secret(secret.clone());
        }
        let c = b.connect().await?;
        self.client = Some(c.clone());
        Ok(c)
    }

    fn team(&self) -> Option<String> {
        self.g.team.clone().or_else(|| self.auth.as_ref().and_then(|a| a.team.clone())).or_else(|| self.cfg.get("team").cloned())
    }

    fn require_auth(&self) -> R<Auth> {
        self.auth.clone().ok_or_else(|| {
            let mut e = CliError::new(ErrorCode::NotSignedIn, "You are not signed in to Silicon Bridge");
            e.message.push_str(if self.plane.is_test() { " in this test environment." } else { "." });
            e.hint("Get a short-lived token from Silicon IAM and run `bridge login <slt>`.")
        })
    }

    /// Runs an authenticated call, refreshing the access token once if it expired.
    async fn call<T, F, Fut>(&mut self, f: F) -> R<T>
    where
        F: Fn(Client, String, Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<T, silicon_bridge_client::Error>>,
    {
        let client = self.client().await?;
        let auth = self.require_auth()?;
        let team = self.team();
        match f(client.clone(), auth.access_token.clone(), team.clone()).await {
            Err(e) if silicon_bridge_client::needs_refresh(&e) => {
                let fresh = self.refresh(&client).await?;
                Ok(f(client, fresh.access_token, team).await?)
            }
            other => Ok(other?),
        }
    }

    async fn refresh(&mut self, client: &Client) -> R<Auth> {
        let _lock = store::Lock::acquire("refresh");
        // Another process may have refreshed while we waited.
        if let Some(disk) = store::load_auth(&self.plane)
            && self.auth.as_ref().is_some_and(|a| a.access_token != disk.access_token) {
                self.auth = Some(disk.clone());
                return Ok(disk);
            }
        let auth = self.require_auth()?;
        let key = format!("refresh-{}", &bridge_protocol::ids::secret_digest(&auth.refresh_token)[..32]);
        let s = client.refresh(&auth.refresh_token, &key).await.map_err(|e| {
            let mut c = CliError::from(e);
            c.code = ErrorCode::TokenExpired;
            c.hint = Some("Your login ended. Get a new short-lived token and run `bridge login <slt>`.".into());
            c
        })?;
        let fresh = Auth {
            access_token: s.access_token,
            refresh_token: s.refresh_token,
            expires_at: now_s() + s.expires_in,
            teams: if s.teams.is_empty() { auth.teams.clone() } else { s.teams },
            ..auth
        };
        store::save_auth(&self.plane, Some(&fresh))?;
        self.auth = Some(fresh.clone());
        Ok(fresh)
    }

    fn session_id(&self) -> R<String> {
        self.g
            .session
            .clone()
            .or_else(|| std::env::var("BRIDGE_SESSION").ok().filter(|s| !s.is_empty()))
            .or_else(|| store::current_session(&self.plane))
            .ok_or_else(|| {
                CliError::new(ErrorCode::NoSession, "No session selected.")
                    .hint("Run `bridge session new <device_id> --connect`, or pass --session <session_id>.")
            })
    }
}

fn now_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

// ───────────────────────────── Output ─────────────────────────────

fn emit(ctx: &Ctx, data: Value, text: impl FnOnce() -> String) {
    if ctx.g.json || ctx.cfg.get("output").is_some_and(|o| o == "json") {
        println!("{}", serde_json::to_string(&json!({"ok": true, "data": data})).unwrap_or_default());
    } else {
        let t = text();
        if !t.is_empty() {
            println!("{}", t.trim_end());
        }
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn fmt_time(t: &time::OffsetDateTime) -> String {
    let local = *t;
    local.format(&time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]Z")).unwrap_or_default()
}

fn ago(t: &time::OffsetDateTime) -> String {
    let s = (time::OffsetDateTime::now_utc() - *t).whole_seconds().max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn table(rows: Vec<Vec<String>>) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows[0].len();
    let widths: Vec<usize> = (0..cols).map(|c| rows.iter().map(|r| r.get(c).map_or(0, |x| x.chars().count())).max().unwrap_or(0)).collect();
    rows.iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, x)| if i + 1 == cols { x.clone() } else { format!("{x:<w$}", w = widths[i]) })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ───────────────────────────── Entry ─────────────────────────────

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = run(argv).await;
    std::process::exit(code);
}

async fn run(argv: Vec<String>) -> i32 {
    let (g, rest) = match parse_globals(argv) {
        Ok(v) => v,
        Err(e) => return fail(false, &e, None),
    };
    let json = g.json;
    let mut plane = Plane::Production;
    let mut test_name = None;
    if let Some(id) = &g.test {
        match store::load_test(id) {
            Ok(env) => {
                test_name = env.name.clone();
                plane = Plane::Test { id: id.clone(), secret: env.secret.clone() };
            }
            Err(e) => {
                let err = CliError::new(ErrorCode::TestingSecretInvalid, format!("{e:#}")).hint(format!("printf %s \"$TEST_APP_SECRET\" | bridge config test add {id}"));
                return fail(json, &err, Some(id));
            }
        }
    }
    let auth = store::load_auth(&plane);
    let mut ctx = Ctx { g, plane, cfg: store::load_config(), client: None, auth, test_name };
    let started = std::time::Instant::now();
    let result = dispatch(&mut ctx, rest.clone()).await;
    let code = match &result {
        Ok(c) => *c,
        Err(e) => fail(ctx.g.json, e, None),
    };
    telemetry(&mut ctx, &rest, &result, started.elapsed()).await;
    if let Plane::Test { id, .. } = &ctx.plane {
        let who = ctx.auth.as_ref().map(|a| a.member_id.clone()).unwrap_or_else(|| "not signed in".into());
        let name = ctx.test_name.clone().unwrap_or_else(|| id.clone());
        eprintln!("[test environment: {name} ({id}) as {who}]");
    }
    code
}

fn fail(json: bool, e: &CliError, _test: Option<&str>) -> i32 {
    if json {
        let err = json!({"code": e.code.as_str(), "message": e.message, "hint": e.hint, "request_id": e.request_id, "details": e.details, "exit_code": e.exit()});
        println!("{}", json!({"ok": false, "error": err}));
    } else {
        let mut err = std::io::stderr();
        let _ = writeln!(err, "error: {}", e.message);
        if let Some(h) = &e.hint {
            let _ = writeln!(err, "  hint: {h}");
        }
        let _ = writeln!(err, "  code: {} (exit {}){}", e.code.as_str(), e.exit(), e.request_id.as_ref().map(|r| format!(", request {r}")).unwrap_or_default());
    }
    e.exit()
}

async fn telemetry(ctx: &mut Ctx, rest: &[String], result: &R<i32>, took: Duration) {
    if !ctx.telemetry_on() || ctx.auth.is_none() || ctx.client.is_none() {
        return;
    }
    let command = rest.first().cloned().unwrap_or_default();
    let event = json!({
        "source": "cli",
        "event": "command",
        "step": format!("cli.{}", rest.iter().take(2).filter(|s| !s.starts_with('-')).cloned().collect::<Vec<_>>().join(".")),
        "success": matches!(result, Ok(0)),
        "duration_ms": took.as_millis() as u64,
        "error_code": result.as_ref().err().map(|e| e.code.as_str()),
        "command": command,
        "client_version": env!("CARGO_PKG_VERSION"),
    });
    let (Some(client), Some(auth)) = (ctx.client.clone(), ctx.auth.clone()) else { return };
    let team = ctx.team();
    let _ = tokio::time::timeout(Duration::from_millis(800), async move { client.authed(&auth.access_token, team.as_deref()).telemetry(event).await }).await;
}

async fn dispatch(ctx: &mut Ctx, rest: Vec<String>) -> R<i32> {
    if ctx.g.version && rest.is_empty() {
        return version(ctx).await;
    }
    let Some(cmd) = rest.first().cloned() else {
        print!("{}", top_help(ctx));
        return Ok(0);
    };
    let args = rest[1..].to_vec();
    let sub = args.first().cloned().unwrap_or_default();
    if ctx.g.help {
        let two = format!("{cmd} {sub}");
        if let Some(n) = help::find(&two).or_else(|| help::find(&cmd)) {
            print!("{}", help::render_node(n));
        } else if let Some(t) = help::render_device_command(&cmd) {
            print!("{t}");
        } else {
            print!("{}", top_help(ctx));
        }
        return Ok(0);
    }
    match cmd.as_str() {
        "help" => {
            print!("{}", top_help(ctx));
            Ok(0)
        }
        "login" if sub == "status" => login_status(ctx).await,
        "login" => login(ctx, &args).await,
        "logout" => logout(ctx).await,
        "iam" => iam(ctx).await,
        "team" => team(ctx, &args).await,
        "config" => config(ctx, &args).await,
        "device" => device(ctx, &args).await,
        "session" => session(ctx, &args).await,
        "takeover" => takeover(ctx, &args).await,
        "request" => request(ctx, &args).await,
        "file" => file(ctx, &args).await,
        "report" => report(ctx, &args).await,
        "env" => env_cmd(ctx, &args).await,
        "version" => version(ctx).await,
        "docs" => {
            emit(ctx, json!({"repository": help::REPO, "docs": help::DOCS, "website": help::WEBSITE, "crate": help::CRATE, "state": store::root()}), || {
                format!(
                    "Docs      {}\nSource    {}\nWebsite   {}\nRust      {}\nState     {}\nInstall   honeycomb install 'bridge'",
                    help::DOCS,
                    help::REPO,
                    help::WEBSITE,
                    help::CRATE,
                    store::root().display()
                )
            });
            Ok(0)
        }
        other if capability::command(other).is_some() => device_command(ctx, other, args).await,
        other => {
            if let Some(repl) = capability::not_exposed(other) {
                let mut e = CliError::new(ErrorCode::UnknownCommand, format!("`{other}` is an agent-device command Bridge doesn't relay."));
                e = match repl {
                    Some(r) => e.hint(format!("Use `{r}` instead.")),
                    None => e.hint("Bridge leaves out agent-device's tools for app developers (simulators, emulators, React Native, web)."),
                };
                return Err(e);
            }
            let near: Vec<&str> = help::NODES
                .iter()
                .map(|n| n.path)
                .chain(COMMANDS.iter().map(|c| c.name))
                .filter(|n| !n.contains(' ') && (n.starts_with(&other[..1.min(other.len())]) || strsim(n, other)))
                .take(4)
                .collect();
            Err(CliError::new(ErrorCode::UnknownCommand, format!("`{other}` is not a bridge command."))
                .hint(if near.is_empty() { "Run `bridge --help` to see the tree.".into() } else { format!("Did you mean: {}? Run `bridge --help` to see the tree.", near.join(", ")) }))
        }
    }
}

fn strsim(a: &str, b: &str) -> bool {
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    common >= 3
}

fn top_help(ctx: &Ctx) -> String {
    let sid = ctx.g.session.clone().or_else(|| store::current_session(&ctx.plane));
    let cache = sid.and_then(|s| store::load_session_cache(&ctx.plane, &s));
    match &cache {
        Some(c) => help::render_top(Some((&c.session_id, &c.device_name, &c.os, &c.commands))),
        None => help::render_top(None),
    }
}

// ───────────────────────────── Getting started ─────────────────────────────

async fn login(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let slt = args.first().cloned().filter(|s| !s.is_empty()).ok_or_else(|| {
        CliError::usage("missing the short-lived token").hint("Usage: bridge login <slt>. Generate one with the IAM CLI for app_id `bridge` (see `bridge iam`).")
    })?;
    let client = ctx.client().await?;
    let s = client.login(&slt).await?;
    let team = ctx.cfg.get("team").cloned().filter(|t| s.teams.contains(t)).or_else(|| s.teams.first().cloned());
    let auth = Auth {
        api_url: ctx.api_url(),
        access_token: s.access_token.clone(),
        refresh_token: s.refresh_token.clone(),
        expires_at: now_s() + s.expires_in,
        member_id: s.member.id.clone(),
        member_kind: if s.member.kind == MemberKind::Silicon { "silicon".into() } else { "carbon".into() },
        teams: s.teams.clone(),
        team: team.clone(),
    };
    store::save_auth(&ctx.plane, Some(&auth))?;
    ctx.auth = Some(auth);
    if let (Plane::Test { id, .. }, Some(env)) = (&ctx.plane, &s.testing_environment) {
        let mut t = store::load_test(id)?;
        t.name = Some(env.name.clone());
        store::save_test(id, &t)?;
        ctx.test_name = Some(env.name.clone());
    }
    let kind = if s.member.kind == MemberKind::Silicon { "Silicon" } else { "Carbon" };
    emit(ctx, json!({"authenticated": true, "member": s.member, "teams": s.teams, "team": team}), || {
        format!("Signed in as {} ({kind}) in teams: {}. Default team: {}.", s.member.id, s.teams.join(", "), team.clone().unwrap_or_default())
    });
    Ok(0)
}

async fn login_status(ctx: &mut Ctx) -> R<i32> {
    if ctx.auth.is_none() {
        let reason = "not signed in";
        if ctx.g.json {
            println!("{}", json!({"ok": true, "data": {"authenticated": false, "reason": reason}}));
        } else {
            println!("Not signed in. Run `bridge login <slt>` with a short-lived token from Silicon IAM.");
        }
        return Ok(3);
    }
    match ctx.call(|c, t, team| async move { c.authed(&t, team.as_deref()).me().await }).await {
        Ok(me) => {
            let kind = if me.member.kind == MemberKind::Silicon { "Silicon" } else { "Carbon" };
            emit(ctx, to_json(&me), || {
                format!("Authenticated as {} ({kind}) in team {}. Teams: {}.", me.member.id, me.team.clone().unwrap_or_else(|| "(none selected)".into()), me.teams.join(", "))
            });
            Ok(0)
        }
        Err(e) if matches!(e.code, ErrorCode::TokenExpired | ErrorCode::NotSignedIn | ErrorCode::Unauthorized) => {
            if ctx.g.json {
                println!("{}", json!({"ok": true, "data": {"authenticated": false, "reason": e.message}}));
            } else {
                println!("Not signed in: {} Run `bridge login <slt>`.", e.message);
            }
            Ok(3)
        }
        Err(e) => Err(e),
    }
}

async fn logout(ctx: &mut Ctx) -> R<i32> {
    let auth = ctx.require_auth()?;
    let client = ctx.client().await?;
    let _ = client.logout(&auth.refresh_token, Some(&auth.access_token)).await;
    store::save_auth(&ctx.plane, None)?;
    store::set_current_session(&ctx.plane, None)?;
    ctx.auth = None;
    emit(ctx, json!({"signed_out": auth.member_id}), || format!("Signed out {}.", auth.member_id));
    Ok(0)
}

async fn iam(ctx: &mut Ctx) -> R<i32> {
    let info = ctx.client().await?.iam().await?;
    emit(ctx, to_json(&info), || format!("app_id   {}\nIAM      {}\nAPI      {}\nWebsite  {}\nDocs     {}", info.app_id, info.iam_base_url, info.api_base_url, info.website_url, info.docs_url));
    Ok(0)
}

async fn version(ctx: &mut Ctx) -> R<i32> {
    let api = match ctx.client().await {
        Ok(c) => Some(c.api_version()),
        Err(_) => None,
    };
    emit(ctx, json!({"cli": env!("CARGO_PKG_VERSION"), "client_crate": env!("CARGO_PKG_VERSION"), "api_version": api, "api_url": ctx.api_url()}), || {
        format!(
            "bridge {} (silicon-bridge-client {}), API {} at {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION"),
            api.map_or("unreachable".to_owned(), |v| format!("v{v}")),
            ctx.api_url()
        )
    });
    Ok(0)
}

async fn team(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let mut auth = ctx.require_auth()?;
    match args.first().map(String::as_str) {
        None | Some("ls") => {
            let me = ctx.call(|c, t, _| async move { c.authed(&t, None).me().await }).await?;
            let default = ctx.team();
            emit(ctx, json!({"teams": me.teams, "default": default}), || {
                me.teams.iter().map(|t| if Some(t) == default.as_ref() { format!("{t} (default)") } else { t.clone() }).collect::<Vec<_>>().join("\n")
            });
        }
        Some("silicons") => {
            let list = ctx.call(|c, t, team| async move { c.authed(&t, team.as_deref()).team_silicons().await }).await?;
            emit(ctx, json!({"items": list}), || {
                if list.is_empty() {
                    "No Silicons in this team.".into()
                } else {
                    list.iter().map(|s| match &s.display_name { Some(n) => format!("{}  {n}", s.id), None => s.id.clone() }).collect::<Vec<_>>().join("\n")
                }
            });
        }
        Some("use") => {
            let t = args.get(1).cloned().ok_or_else(|| CliError::usage("missing team handle").hint("Usage: bridge team use <handle>"))?;
            if !auth.teams.contains(&t) {
                return Err(CliError::new(ErrorCode::NotATeamMember, format!("This login doesn't reach team {t:?}.")).hint(format!("Teams: {}", auth.teams.join(", "))));
            }
            auth.team = Some(t.clone());
            store::save_auth(&ctx.plane, Some(&auth))?;
            ctx.auth = Some(auth);
            emit(ctx, json!({"default": t}), || format!("Default team is now {t}."));
        }
        Some(other) => return Err(CliError::usage(format!("unknown `bridge team {other}`")).hint("Usage: bridge team ls | bridge team silicons | bridge team use <handle>")),
    }
    Ok(0)
}

async fn config(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, &[]);
    match a.pos.first().map(String::as_str) {
        None | Some("ls") => {
            let cfg = ctx.cfg.clone();
            emit(ctx, json!({"settings": cfg, "state_dir": store::root(), "keys": store::CONFIG_KEYS.iter().map(|(k, d)| json!({"key": k, "about": d})).collect::<Vec<_>>()}), || {
                let mut s = format!("State: {}\n", store::root().display());
                for (k, d) in store::CONFIG_KEYS {
                    s.push_str(&format!("{k:<18} {:<30} {d}\n", cfg.get(*k).cloned().unwrap_or_else(|| "(default)".into())));
                }
                s
            });
        }
        Some("get") => {
            let k = a.req(1, "key", "bridge config get <key>")?;
            let v = ctx.cfg.get(&k).cloned();
            emit(ctx, json!({"key": k, "value": v}), || v.clone().unwrap_or_else(|| "(default)".into()));
        }
        Some("set") => {
            let k = a.req(1, "key", "bridge config set <key> <value>")?;
            let v = a.req(2, "value", "bridge config set <key> <value>")?;
            if !store::CONFIG_KEYS.iter().any(|(x, _)| *x == k) {
                return Err(CliError::usage(format!("unknown setting {k:?}")).hint(format!("Settings: {}", store::CONFIG_KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", "))));
            }
            match k.as_str() {
                "telemetry" if v != "on" && v != "off" => return Err(CliError::usage("telemetry is on or off")),
                "output" if v != "text" && v != "json" => return Err(CliError::usage("output is text or json")),
                "self_destruct" => {
                    parse_ttl(&v)?;
                }
                _ => {}
            }
            ctx.cfg.insert(k.clone(), v.clone());
            store::save_config(&ctx.cfg)?;
            emit(ctx, json!({"key": k, "value": v}), || format!("{k} = {v}"));
        }
        Some("unset") => {
            let k = a.req(1, "key", "bridge config unset <key>")?;
            ctx.cfg.remove(&k);
            store::save_config(&ctx.cfg)?;
            emit(ctx, json!({"key": k, "value": null}), || format!("{k} is back to its default."));
        }
        Some("home") => {
            let d = a.req(1, "directory", "bridge config home <dir>")?;
            let root = store::set_home(&PathBuf::from(&d)).map_err(|e| CliError::usage(e.to_string()))?;
            emit(ctx, json!({"state_dir": root}), || format!("Bridge state now lives in {}", root.display()));
        }
        Some("test") => match a.pos.get(1).map(String::as_str) {
            Some("add") => {
                let id = a.req(2, "test id", "bridge config test add <test_id>  (secret on stdin)")?;
                if uuid::Uuid::parse_str(&id).is_err() {
                    return Err(CliError::usage(format!("{id:?} is not a test id; test ids are the Honeycomb environment UUID.")));
                }
                let mut secret = String::new();
                if std::io::stdin().is_terminal() {
                    eprint!("Paste the test application secret (ask_...): ");
                }
                std::io::stdin().read_to_string(&mut secret).map_err(|e| CliError::usage(format!("reading stdin: {e}")))?;
                let secret = secret.trim().to_owned();
                if !bridge_protocol::ids::is_secret(bridge_protocol::ids::APP_SECRET_PREFIX, &secret) {
                    return Err(CliError::new(ErrorCode::TestingSecretInvalid, "That is not a test application secret: expected ask_ followed by 43 characters."));
                }
                let client = Client::builder(ctx.api_url()).testing_secret(secret.clone()).connect().await?;
                let env = client.testing_environment().await?;
                store::save_test(&id, &store::TestEnv { secret, name: Some(env.name.clone()), auth: None })?;
                emit(ctx, json!({"test_id": id, "environment": env}), || format!("Added test environment {:?} ({id}). Use: bridge --test {id} <command>", env.name));
            }
            Some("ls") | None => {
                let list = store::list_tests();
                emit(ctx, json!(list.iter().map(|(id, e)| json!({"test_id": id, "name": e.name, "signed_in_as": e.auth.as_ref().map(|a| a.member_id.clone())})).collect::<Vec<_>>()), || {
                    if list.is_empty() {
                        "No test environments added. Add one with `bridge config test add <test_id>` (secret on stdin).".into()
                    } else {
                        list.iter().map(|(id, e)| format!("{id}  {}  {}", e.name.clone().unwrap_or_default(), e.auth.as_ref().map(|a| a.member_id.clone()).unwrap_or_else(|| "not signed in".into()))).collect::<Vec<_>>().join("\n")
                    }
                });
            }
            Some("rm") => {
                let id = a.req(2, "test id", "bridge config test rm <test_id>")?;
                let _ = std::fs::remove_file(store::root().join("test").join(format!("{id}.json")));
                emit(ctx, json!({"removed": id}), || format!("Removed test environment {id} from this CLI."));
            }
            Some(o) => return Err(CliError::usage(format!("unknown `bridge config test {o}`")).hint("Usage: bridge config test add|ls|rm")),
        },
        Some(o) => return Err(CliError::usage(format!("unknown `bridge config {o}`")).hint("Usage: bridge config ls|get|set|unset|home|test")),
    }
    Ok(0)
}

async fn env_cmd(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    if !ctx.plane.is_test() {
        return Err(CliError::new(ErrorCode::TestOnly, "This action is only possible in a test environment.").hint("Add --test <test_id>, e.g. `bridge --test <test_id> env show`."));
    }
    if args.first().map(String::as_str).is_some_and(|s| s != "show") {
        return Err(CliError::usage("Usage: bridge --test <test_id> env show"));
    }
    let env = ctx.client().await?.testing_environment().await?;
    let who = ctx.auth.as_ref().map(|a| a.member_id.clone());
    emit(ctx, json!({"environment": env, "signed_in_as": who}), || {
        format!(
            "Test environment {} ({})\nState: {}\nSigned in as: {}\nPaired devices: {} of {}",
            env.name,
            env.environment_id,
            env.state,
            who.clone().unwrap_or_else(|| "not signed in".into()),
            env.paired_devices,
            env.device_limit
        )
    });
    Ok(0)
}

// ───────────────────────────── Devices ─────────────────────────────

fn device_line(d: &Device) -> Vec<String> {
    let in_use = d.in_use.as_ref().map_or("—".to_owned(), |u| format!("{} ({}, {})", u.silicon_id, u.session_id, ago(&u.since)));
    vec![
        d.device_id.to_string(),
        d.name.clone(),
        d.os.as_str().to_owned(),
        if d.online { "yes".into() } else { "no".into() },
        in_use,
        d.days_left.map_or("—".into(), |x| x.to_string()),
    ]
}

fn parse_device_id(s: &str) -> R<DeviceId> {
    s.parse().map_err(|_| CliError::usage(format!("{s:?} is not a device id; device ids are 8 lowercase hexadecimal characters, like 7c1e09ab.")).hint("List devices with `bridge device ls`."))
}

async fn device(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let sub = args.first().cloned().unwrap_or_else(|| "ls".into());
    let a = Args::parse(&args[args.len().min(1)..], &["--os", "--name", "--visibility", "--ttl-days", "--access", "--address", "--silicon", "--session", "--since", "--until", "--limit"]);
    match sub.as_str() {
        "ls" => {
            let scope = if a.flag("--team-visible") { Some("team".to_owned()) } else { None };
            let q = DeviceQuery { scope, online: a.flag("--online").then_some(true), os: a.value("--os"), limit: Some(100), cursor: None };
            let page = ctx.call(|c, t, team| { let q = q.clone(); async move { c.authed(&t, team.as_deref()).devices(q).await } }).await?;
            emit(ctx, to_json(&page), || {
                if page.items.is_empty() {
                    return "No devices. A Carbon pairs one at bridge.teamofsilicons.com or with `bridge device pair <code> --name <name>`; a Silicon needs its Carbon to grant access.".into();
                }
                let mut rows = vec![vec!["ID".into(), "NAME".into(), "OS".into(), "ONLINE".into(), "IN USE BY".into(), "DAYS LEFT".into()]];
                rows.extend(page.items.iter().map(device_line));
                table(rows)
            });
        }
        "show" => {
            let id = a.req(0, "device id", "bridge device show <device_id>")?;
            parse_device_id(&id)?;
            let d = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).device(&id).await } }).await?;
            emit(ctx, to_json(&d), || device_text(&d));
        }
        "pair" => {
            let code = a.req(0, "pairing code", "bridge device pair <pairing_code> --name <name>")?;
            let name = a.value("--name").ok_or_else(|| CliError::usage("--name is required").hint("bridge device pair <pairing_code> --name \"Saket's Pixel\""))?;
            let visibility = match a.value("--visibility").as_deref() {
                None => None,
                Some("team") => Some(Visibility::Team),
                Some("personal") => Some(Visibility::Personal),
                Some(o) => return Err(CliError::usage(format!("--visibility is team or personal, got {o:?}"))),
            };
            let ttl = a.value("--ttl-days").map(|v| v.parse::<i32>().map_err(|_| CliError::usage("--ttl-days takes a number of days, 1–30"))).transpose()?;
            let claim = PairingClaim { pairing_code: code, name, visibility, pair_ttl_days: ttl, silicon_ids: a.values("--access") };
            let d = ctx.call(|c, t, team| { let claim = claim.clone(); async move { c.authed(&t, team.as_deref()).pair(&claim).await } }).await?;
            emit(ctx, to_json(&d), || format!("Paired {} \"{}\" ({}). Next: finish the device's own setup — watch it with `bridge device setup {} --watch`.", d.device_id, d.name, d.os.as_str(), d.device_id));
        }
        "attach" => {
            let host = a.req(0, "host device id", "bridge device attach <host_device_id> --os ios --name <name>")?;
            let os_raw = a.value("--os").ok_or_else(|| CliError::usage("--os is required: ios, ipados, tvos, samsung_tv or lg_tv"))?;
            let os: bridge_protocol::DeviceOs = serde_json::from_value(json!(os_raw)).map_err(|_| CliError::usage(format!("unknown --os {os_raw:?}")))?;
            let name = a.value("--name").ok_or_else(|| CliError::usage("--name is required"))?;
            let input = AttachmentCreate { os, name, visibility: None, pair_ttl_days: None, address: a.value("--address") };
            let d = ctx.call(|c, t, team| { let (host, input) = (host.clone(), input.clone()); async move { c.authed(&t, team.as_deref()).attach(&host, &input).await } }).await?;
            emit(ctx, to_json(&d), || format!("Created {} \"{}\" through {host}. Follow setup with `bridge device setup {} --watch`.", d.device_id, d.name, d.device_id));
        }
        "setup" => {
            let id = a.req(0, "device id", "bridge device setup <device_id> [--watch]")?;
            loop {
                let s = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).setup(&id).await } }).await?;
                let done = s.state == SetupState::Complete;
                if !a.flag("--watch") || done || ctx.g.json {
                    emit(ctx, to_json(&s), || setup_text(&s));
                    break;
                }
                print!("\x1b[2J\x1b[H{}\n(watching; Ctrl-C to stop)\n", setup_text(&s));
                let _ = std::io::stdout().flush();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        "setup-code" => {
            let id = a.req(0, "device id", "bridge device setup-code <device_id> <code>")?;
            let code = a.req(1, "code", "bridge device setup-code <device_id> <code>")?;
            let s = ctx.call(|c, t, team| { let (id, code) = (id.clone(), code.clone()); async move { c.authed(&t, team.as_deref()).setup_code(&id, &code).await } }).await?;
            emit(ctx, to_json(&s), || format!("Code sent.\n{}", setup_text(&s)));
        }
        "rename" | "visibility" | "ttl" => {
            let id = a.req(0, "device id", &format!("bridge device {sub} <device_id> <value>"))?;
            let v = a.req(1, "value", &format!("bridge device {sub} <device_id> <value>"))?;
            let patch = match sub.as_str() {
                "rename" => DevicePatch { name: Some(v), ..Default::default() },
                "visibility" => DevicePatch {
                    visibility: Some(match v.as_str() {
                        "team" => Visibility::Team,
                        "personal" => Visibility::Personal,
                        o => return Err(CliError::usage(format!("visibility is team or personal, got {o:?}"))),
                    }),
                    ..Default::default()
                },
                _ => DevicePatch { pair_ttl_days: Some(v.parse().map_err(|_| CliError::usage("ttl takes a number of days, 1–30"))?), ..Default::default() },
            };
            let d = ctx.call(|c, t, team| { let (id, patch) = (id.clone(), patch.clone()); async move { c.authed(&t, team.as_deref()).update_device(&id, None, &patch).await } }).await?;
            emit(ctx, to_json(&d), || match sub.as_str() {
                "ttl" => format!(
                    "{} stays paired until {} unless used ({} days without activity).",
                    d.name,
                    d.pair_expires_at.map(|t| fmt_time(&t)).unwrap_or_default(),
                    d.pair_ttl_days.unwrap_or_default()
                ),
                "visibility" => format!("{} is now {}.", d.name, d.visibility.as_str()),
                _ => format!("Renamed to \"{}\".", d.name),
            });
        }
        "stop" => {
            let id = a.req(0, "device id", "bridge device stop <device_id>")?;
            let s = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).stop_device(&id).await } }).await?;
            emit(ctx, to_json(&s), || format!("Stopped {} (session {}).", s.silicon_id, s.session_id));
        }
        "rm" => {
            let id = a.req(0, "device id", "bridge device rm <device_id> --yes")?;
            let d = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).device(&id).await } }).await?;
            if !a.flag("--yes") {
                return Err(CliError::new(
                    ErrorCode::ConfirmationRequired,
                    format!(
                        "This removes \"{}\" ({id}): it ends {}, removes access for {} Silicon(s), and unpairs it.",
                        d.name,
                        d.in_use.as_ref().map_or("no running session".to_owned(), |u| format!("{}'s session {}", u.silicon_id, u.session_id)),
                        d.access_count.unwrap_or(0)
                    ),
                )
                .hint(format!("Run `bridge device rm {id} --yes` to confirm.")));
            }
            let version = d.version;
            ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).remove_device(&id, version).await } }).await?;
            emit(ctx, json!({"removed": id}), || format!("Removed {id} (\"{}\").", d.name));
        }
        "access" => {
            let op = a.pos.first().cloned().unwrap_or_else(|| "ls".into());
            let id = a.req(1, "device id", "bridge device access ls|grant|revoke <device_id> [<silicon_id>...]")?;
            match op.as_str() {
                "ls" => {
                    let list = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).access(&id).await } }).await?;
                    emit(ctx, json!({"items": list}), || {
                        if list.is_empty() {
                            return format!("No Silicon has access yet. Grant it with `bridge device access grant {id} <silicon_id>`.");
                        }
                        let mut rows = vec![vec!["SILICON".into(), "GRANTED BY".into(), "GRANTED".into(), "LAST USED".into()]];
                        rows.extend(list.iter().map(|g| vec![g.silicon_id.clone(), g.granted_by.clone(), fmt_time(&g.granted_at), g.last_used_at.map(|t| fmt_time(&t)).unwrap_or_else(|| "—".into())]));
                        table(rows)
                    });
                }
                "grant" | "revoke" => {
                    let silicons: Vec<String> = a.pos[2..].to_vec();
                    if silicons.is_empty() {
                        return Err(CliError::usage("name at least one Silicon").hint(format!("bridge device access {op} {id} si:chef")));
                    }
                    for s in &silicons {
                        if op == "grant" {
                            ctx.call(|c, t, team| { let (id, s) = (id.clone(), s.clone()); async move { c.authed(&t, team.as_deref()).grant(&id, &s).await } }).await?;
                        } else {
                            ctx.call(|c, t, team| { let (id, s) = (id.clone(), s.clone()); async move { c.authed(&t, team.as_deref()).revoke(&id, &s).await } }).await?;
                        }
                    }
                    emit(ctx, json!({"device_id": id, op.clone(): silicons}), || {
                        if op == "grant" { format!("Granted {} access to {id}.", silicons.join(", ")) } else { format!("Revoked access for {} on {id}; any running session of theirs there has ended.", silicons.join(", ")) }
                    });
                }
                o => return Err(CliError::usage(format!("unknown `bridge device access {o}`")).hint("ls, grant or revoke")),
            }
        }
        "activity" => {
            let id = a.req(0, "device id", "bridge device activity <device_id>")?;
            let q = ActivityQuery {
                silicon_id: a.value("--silicon"),
                session_id: a.value("--session"),
                since: a.value("--since").map(|s| relative_time(&s)).transpose()?,
                until: a.value("--until").map(|s| relative_time(&s)).transpose()?,
                limit: a.value("--limit").and_then(|l| l.parse().ok()).or(Some(50)),
                cursor: None,
            };
            let page = ctx.call(|c, t, team| { let (id, q) = (id.clone(), q.clone()); async move { c.authed(&t, team.as_deref()).activity(&id, q).await } }).await?;
            emit(ctx, to_json(&page), || {
                let mut rows = vec![vec!["TIME".into(), "WHO".into(), "SESSION".into(), "ACTION".into(), "OUTCOME".into(), "FILES".into()]];
                rows.extend(page.items.iter().map(|e| {
                    let action = match (&e.command, &e.args) {
                        (Some(c), Some(a)) => format!("{c} {}", a.join(" ")),
                        (Some(c), None) => c.clone(),
                        _ => e.action.clone(),
                    };
                    vec![fmt_time(&e.at), e.actor.id.clone(), e.session_id.as_ref().map_or("—".into(), ToString::to_string), action, e.outcome.clone().unwrap_or_default(), e.files.len().to_string()]
                }));
                table(rows)
            });
        }
        "requests" => {
            let id = a.req(0, "device id", "bridge device requests <device_id>")?;
            let page = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).device_requests(&id, ListQuery::default()).await } }).await?;
            emit(ctx, to_json(&page), || requests_table(&page.items));
        }
        o => {
            return Err(CliError::new(ErrorCode::UnknownCommand, format!("`bridge device {o}` is not a command."))
                .hint("Commands: ls, show, pair, attach, setup, setup-code, rename, visibility, ttl, stop, rm, access, activity, requests"));
        }
    }
    Ok(0)
}

fn relative_time(s: &str) -> R<String> {
    if let Some(n) = s.strip_suffix('h').or_else(|| s.strip_suffix('d')).or_else(|| s.strip_suffix('m')).and_then(|n| n.parse::<i64>().ok()) {
        let unit = s.chars().last().unwrap_or('h');
        let d = match unit {
            'm' => time::Duration::minutes(n),
            'd' => time::Duration::days(n),
            _ => time::Duration::hours(n),
        };
        return Ok((time::OffsetDateTime::now_utc() - d).format(&time::format_description::well_known::Rfc3339).unwrap_or_default());
    }
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).map_err(|_| CliError::usage(format!("{s:?} is not a time; use RFC 3339 or 30m, 2h, 3d")))?;
    Ok(s.to_owned())
}

fn device_text(d: &Device) -> String {
    let mut s = format!(
        "{} ({})\n  OS:        {}{}\n  Owner:     {}\n  Online:    {}\n  In use:    {}\n  Pairing:   {} day(s) left of {}\n",
        d.name,
        d.device_id,
        d.os.as_str(),
        d.os_version.as_ref().map(|v| format!(" {v}")).unwrap_or_default(),
        d.owner.id,
        if d.online { "yes" } else { "no" },
        d.in_use.as_ref().map_or("no".to_owned(), |u| format!("{} in session {} since {}{}", u.silicon_id, u.session_id, fmt_time(&u.since), if u.paused { " (paused for takeover)" } else { "" })),
        d.days_left.unwrap_or(0),
        d.pair_ttl_days.unwrap_or(0),
    );
    if let Some(h) = &d.host_device_id {
        s.push_str(&format!("  Through:   {h}\n"));
    }
    if let Some(cmds) = &d.commands {
        s.push_str("\nYou can:\n");
        for c in COMMANDS.iter().filter(|c| cmds.iter().any(|x| x == c.name)) {
            s.push_str(&format!("  {:<14} {}\n", c.name, c.summary));
        }
    }
    if let Some(m) = d.missing.as_ref().filter(|m| !m.is_empty()) {
        s.push_str("\nMissing:\n");
        for x in m {
            s.push_str(&format!("  {:<14} {}\n", x.capability.as_str(), x.reason));
        }
    }
    s
}

fn setup_text(s: &Setup) -> String {
    let mut out = format!(
        "Setup: {}\n",
        match s.state {
            SetupState::Complete => "complete",
            SetupState::NeedsCarbon => "needs you on the device",
            SetupState::InProgress => "in progress",
        }
    );
    if s.steps.is_empty() && s.state != SetupState::Complete {
        out.push_str("  Waiting for the device to connect and report its setup.\n");
    }
    for st in &s.steps {
        let mark = match st.status {
            StepStatus::Done => "✓",
            StepStatus::InProgress => "…",
            StepStatus::NeedsCarbon => "!",
            StepStatus::Failed => "✗",
            StepStatus::Todo => " ",
        };
        out.push_str(&format!("  {mark} {}", st.title));
        if st.status != StepStatus::Done {
            if let Some(h) = &st.help {
                out.push_str(&format!(" — {h}"));
            }
            if let Some(e) = &st.error {
                out.push_str(&format!(" ({e})"));
            }
        }
        out.push('\n');
    }
    out
}

fn requests_table(items: &[RequestInfo]) -> String {
    if items.is_empty() {
        return "No requests.".into();
    }
    let mut rows = vec![vec!["TIME".into(), "FROM".into(), "TO".into(), "DEVICE".into(), "DELIVERY".into(), "REASON".into()]];
    rows.extend(items.iter().map(|r| vec![fmt_time(&r.created_at), r.from.clone(), r.to.clone(), r.device_id.to_string(), format!("{:?}", r.delivery).to_lowercase(), r.reason.clone()]));
    table(rows)
}

// ───────────────────────────── Sessions ─────────────────────────────

async fn connect_session(ctx: &mut Ctx, id: &str) -> R<Session> {
    let s = ctx.call(|c, t, team| { let id = id.to_owned(); async move { c.authed(&t, team.as_deref()).session(&id).await } }).await?;
    if s.state == SessionState::Ended {
        return Err(CliError::new(ErrorCode::SessionEnded, format!("Session {id} has ended ({}).", s.end_reason.map_or("unknown reason", EndReason::explain)))
            .hint(format!("Start a new one: bridge session new {}", s.device_id)));
    }
    let d = s.device.clone();
    let cache = store::SessionCache {
        session_id: s.session_id.to_string(),
        device_id: s.device_id.to_string(),
        device_name: d.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
        os: d.as_ref().map(|d| d.os.as_str().to_owned()).unwrap_or_default(),
        capabilities: s.capabilities.clone().unwrap_or_default().iter().map(|c| c.as_str().to_owned()).collect(),
        commands: s.commands.clone().unwrap_or_default(),
        test_id: match &ctx.plane {
            Plane::Test { id, .. } => Some(id.clone()),
            Plane::Production => None,
        },
    };
    store::save_session_cache(&ctx.plane, &cache)?;
    store::set_current_session(&ctx.plane, Some(id))?;
    Ok(s)
}

async fn session(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let sub = args.first().cloned().unwrap_or_else(|| "status".into());
    let a = Args::parse(&args[args.len().min(1)..], &["--device", "--state"]);
    match sub.as_str() {
        "new" => {
            let id = a.req(0, "device id", "bridge session new <device_id> [--connect]")?;
            let did = parse_device_id(&id)?;
            let s = ctx.call(|c, t, team| { let did = did.clone(); async move { c.authed(&t, team.as_deref()).start_session(&did).await } }).await?;
            let sid = s.session_id.to_string();
            if a.flag("--connect") {
                connect_session(ctx, &sid).await?;
            }
            if ctx.g.json {
                emit(ctx, to_json(&s), String::new);
            } else {
                println!("{sid}");
                eprintln!(
                    "Started session {sid} on {id}.{} It ends after 5 minutes without a command; end it with `bridge session end {sid}`.",
                    if a.flag("--connect") { " Connected — try `bridge snapshot -i`.".to_owned() } else { format!(" Next: bridge session connect {sid}.") }
                );
            }
        }
        "connect" => {
            let id = a.req(0, "session id", "bridge session connect <session_id>")?;
            let s = connect_session(ctx, &id).await?;
            let d = s.device.clone();
            emit(ctx, to_json(&s), || {
                format!(
                    "Connected to {id} on {} ({}). `bridge --help` now lists only what works there. Try: bridge snapshot -i",
                    d.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
                    d.as_ref().map(|d| d.os.as_str()).unwrap_or_default()
                )
            });
        }
        "disconnect" => {
            store::set_current_session(&ctx.plane, None)?;
            emit(ctx, json!({"connected": null}), || "Disconnected. The session keeps running until it's ended or idle for 5 minutes.".into());
        }
        "status" => {
            let id = a.pos.first().cloned().map_or_else(|| ctx.session_id(), Ok)?;
            let s = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).session(&id).await } }).await?;
            emit(ctx, to_json(&s), || {
                let dev = s.device.as_ref().map(|d| format!("{} ({})", d.name, d.device_id)).unwrap_or_else(|| s.device_id.to_string());
                match s.state {
                    SessionState::Ended => format!("{} ended {} on {dev}: {}.", s.session_id, s.ended_at.map(|t| fmt_time(&t)).unwrap_or_default(), s.end_reason.map_or("unknown", EndReason::explain)),
                    st => format!(
                        "{} {} on {dev} since {}, {} command(s), ends if idle at {}",
                        s.session_id,
                        if st == SessionState::Paused { "paused (takeover)" } else { "active" },
                        fmt_time(&s.started_at),
                        s.command_count,
                        s.idle_ends_at.map(|t| fmt_time(&t)).unwrap_or_default()
                    ),
                }
            });
        }
        "ls" => {
            let q = ListQuery { device_id: a.value("--device"), state: a.value("--state"), limit: Some(50), ..Default::default() };
            let page = ctx.call(|c, t, team| { let q = q.clone(); async move { c.authed(&t, team.as_deref()).sessions(q).await } }).await?;
            emit(ctx, to_json(&page), || {
                if page.items.is_empty() {
                    return "No sessions.".into();
                }
                let mut rows = vec![vec!["SESSION".into(), "DEVICE".into(), "SILICON".into(), "STATE".into(), "STARTED".into(), "COMMANDS".into(), "ENDED BECAUSE".into()]];
                rows.extend(page.items.iter().map(|s| {
                    vec![
                        s.session_id.to_string(),
                        s.device_id.to_string(),
                        s.silicon_id.clone(),
                        format!("{:?}", s.state).to_lowercase(),
                        fmt_time(&s.started_at),
                        s.command_count.to_string(),
                        s.end_reason.map_or("—".into(), |r| r.as_str().to_owned()),
                    ]
                }));
                table(rows)
            });
        }
        "end" => {
            let id = a.pos.first().cloned().map_or_else(|| ctx.session_id(), Ok)?;
            let s = ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).end_session(&id).await } }).await?;
            if store::current_session(&ctx.plane).as_deref() == Some(id.as_str()) {
                store::set_current_session(&ctx.plane, None)?;
            }
            emit(ctx, to_json(&s), || format!("Ended {} after {} command(s). {} is free for other Silicons.", s.session_id, s.command_count, s.device_id));
        }
        o => return Err(CliError::new(ErrorCode::UnknownCommand, format!("`bridge session {o}` is not a command.")).hint("new, connect, disconnect, status, ls, end")),
    }
    Ok(0)
}

async fn takeover(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, &["--reason"]);
    let sid = ctx.session_id()?;
    match a.pos.first().map(String::as_str) {
        None => {
            let reason = a.value("--reason").ok_or_else(|| CliError::usage("--reason is required").hint("bridge takeover --reason \"Please approve the Face ID prompt\""))?;
            let t = ctx.call(|c, tok, team| { let (sid, reason) = (sid.clone(), reason.clone()); async move { c.authed(&tok, team.as_deref()).takeover(&sid, &reason).await } }).await?;
            emit(ctx, to_json(&t), || format!("Session {sid} is paused; the device shows your reason and a Done button. Commands wait until the Carbon taps Done (by {}).", fmt_time(&t.expires_at)));
        }
        Some("status") => {
            let t = ctx.call(|c, tok, team| { let sid = sid.clone(); async move { c.authed(&tok, team.as_deref()).takeover_status(&sid).await } }).await?;
            emit(ctx, to_json(&t), || match &t {
                Some(t) => format!("Paused since {}: {} (ends by {}).", fmt_time(&t.started_at), t.reason, fmt_time(&t.expires_at)),
                None => "No takeover in progress.".into(),
            });
        }
        Some("release") => {
            ctx.call(|c, tok, team| { let sid = sid.clone(); async move { c.authed(&tok, team.as_deref()).release_takeover(&sid).await } }).await?;
            emit(ctx, json!({"released": sid}), || format!("Session {sid} is active again."));
        }
        Some(o) => return Err(CliError::usage(format!("unknown `bridge takeover {o}`")).hint("bridge takeover --reason \"...\" | status | release")),
    }
    Ok(0)
}

async fn request(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let sub = args.first().cloned().unwrap_or_else(|| "ls".into());
    let a = Args::parse(&args[args.len().min(1)..], &["--reason", "--device"]);
    match sub.as_str() {
        "send" => {
            let id = a.req(0, "device id", "bridge request send <device_id> --reason \"...\"")?;
            parse_device_id(&id)?;
            let reason = a.value("--reason").ok_or_else(|| CliError::usage("--reason is required (1–300 characters)"))?;
            let n = reason.trim().chars().count();
            if n == 0 || n > 300 {
                return Err(CliError::usage(format!("--reason must be 1–300 characters; it is {n}.")));
            }
            let r = ctx.call(|c, t, team| { let (id, reason) = (id.clone(), reason.clone()); async move { c.authed(&t, team.as_deref()).send_request(&id, &reason).await } }).await?;
            emit(ctx, to_json(&r), || {
                format!(
                    "Sent to {} (using {}{}). Delivery: {}.",
                    r.to,
                    r.device_id,
                    r.session_id.as_ref().map(|s| format!(" in session {s}")).unwrap_or_default(),
                    format!("{:?}", r.delivery).to_lowercase()
                )
            });
        }
        "ls" => {
            let direction = if a.flag("--sent") { Some("sent".into()) } else if a.flag("--received") { Some("received".into()) } else { None };
            let q = ListQuery { direction, device_id: a.value("--device"), limit: Some(50), ..Default::default() };
            let page = ctx.call(|c, t, team| { let q = q.clone(); async move { c.authed(&t, team.as_deref()).requests(q).await } }).await?;
            emit(ctx, to_json(&page), || requests_table(&page.items));
        }
        o => return Err(CliError::usage(format!("unknown `bridge request {o}`")).hint("send or ls")),
    }
    Ok(0)
}

async fn file(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let sub = args.first().cloned().unwrap_or_else(|| "ls".into());
    let a = Args::parse(&args[args.len().min(1)..], &["--session", "--device", "--kind", "--out"]);
    match sub.as_str() {
        "ls" => {
            let q = ListQuery { session_id: a.value("--session"), device_id: a.value("--device"), kind: a.value("--kind"), limit: Some(100), ..Default::default() };
            let page = ctx.call(|c, t, team| { let q = q.clone(); async move { c.authed(&t, team.as_deref()).files(q).await } }).await?;
            emit(ctx, to_json(&page), || {
                if page.items.is_empty() {
                    return "No files.".into();
                }
                let mut rows = vec![vec!["FILE".into(), "KIND".into(), "SIZE".into(), "SELF-DESTRUCTS".into(), "LINK".into()]];
                rows.extend(page.items.iter().map(|f| {
                    vec![f.file_id.to_string(), f.kind.as_str().into(), human_size(f.size_bytes), f.self_destruct_at.map_or("never".into(), |t| fmt_time(&t)), f.url.clone()]
                }));
                table(rows)
            });
        }
        "show" | "keep" | "get" => {
            let id = a.req(0, "file id", &format!("bridge file {sub} <file_id>"))?;
            let f = if sub == "keep" {
                ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).keep_file(&id).await } }).await?
            } else {
                ctx.call(|c, t, team| { let id = id.clone(); async move { c.authed(&t, team.as_deref()).file(&id).await } }).await?
            };
            if sub == "get" {
                let path = save_file(ctx, &f, a.value("--out")).await?;
                emit(ctx, json!({"file": f, "saved_to": path}), || format!("Saved {} to {}", f.name, path.display()));
            } else {
                emit(ctx, to_json(&f), || file_line(&f));
            }
        }
        o => return Err(CliError::usage(format!("unknown `bridge file {o}`")).hint("ls, show, get, keep")),
    }
    Ok(0)
}

fn human_size(b: i64) -> String {
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{:.1} KiB", b as f64 / 1024.0),
        _ => format!("{:.1} MiB", b as f64 / 1_048_576.0),
    }
}

fn file_line(f: &FileInfo) -> String {
    format!(
        "{} {} ({}, {}) {}\n  {}",
        f.kind.as_str(),
        f.name,
        human_size(f.size_bytes),
        f.self_destruct_at.map_or("permanent".into(), |t| format!("self-destructs {}", fmt_time(&t))),
        f.file_id,
        f.url
    )
}

async fn save_file(ctx: &mut Ctx, f: &FileInfo, out: Option<String>) -> R<PathBuf> {
    let client = ctx.client().await?;
    let auth = ctx.require_auth()?;
    let bytes = client.download(&f.url, &auth.access_token).await?;
    let dir = ctx.cfg.get("download_dir").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = match out {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_dir() { p.join(&f.name) } else { p }
        }
        None => dir.join(&f.name),
    };
    std::fs::write(&path, bytes).map_err(|e| CliError::usage(format!("writing {}: {e}", path.display())))?;
    Ok(path)
}

async fn report(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, &["--pr"]);
    let message = a.pos.join(" ");
    if message.trim().is_empty() {
        return Err(CliError::usage("describe the bug").hint("bridge report \"what happened, what you expected, how to reproduce\" [--pr <link>]"));
    }
    let context = json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cli": env!("CARGO_PKG_VERSION"),
        "session": store::current_session(&ctx.plane),
        "test": ctx.plane.is_test(),
    });
    let input = ReportInput { message, pr: a.value("--pr"), client_version: format!("bridge-cli {}", env!("CARGO_PKG_VERSION")), context };
    let r = ctx.call(|c, t, team| { let input = input.clone(); async move { c.authed(&t, team.as_deref()).report(&input).await } }).await?;
    let pr = input.pr.is_some();
    emit(ctx, to_json(&r), || {
        let mut s = format!("Report {} sent to the Bridge team (email {}).", r.report_id, r.notification);
        if !pr {
            s.push_str(&format!("\nBridge is open source: if you can fix it, open a pull request at {} and report again with --pr <link>.", r.repository_url));
        }
        s
    });
    Ok(0)
}

// ───────────────────────────── Device commands ─────────────────────────────

fn parse_ttl(s: &str) -> R<u32> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u32 = n.parse().map_err(|_| CliError::usage(format!("{s:?} is not a duration; use 90m, 12h, 7d")))?;
    let minutes = match unit {
        "m" | "min" => n,
        "h" => n * 60,
        "d" | "" => n * 1440,
        _ => return Err(CliError::usage(format!("{s:?}: use m, h or d, like 90m, 12h, 7d"))),
    };
    if !(1..=43_200).contains(&minutes) {
        return Err(CliError::usage(format!("{s:?} is outside 1 minute to 30 days")));
    }
    Ok(minutes)
}

/// Only ADB push/install read local input; shell arguments and pull paths belong to the device.
fn adb_local_input(args: &[String], index: usize) -> bool {
    match args.first().map(String::as_str) {
        Some("push") => args.len() == 3 && index == 1,
        Some("install") => index == args.len().saturating_sub(1) && index > 0 && !args[index].starts_with('-'),
        _ => false,
    }
}

async fn device_command(ctx: &mut Ctx, name: &str, raw: Vec<String>) -> R<i32> {
    let sid = ctx.session_id()?;
    // Bridge's own flags for file-making commands; everything else goes to the device untouched.
    let mut args = Vec::new();
    let mut ttl = None;
    let mut keep = false;
    let mut out = None;
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ttl" => ttl = Some(parse_ttl(&it.next().ok_or_else(|| CliError::usage("--ttl needs a duration, like 7d"))?)?),
            "--keep" => keep = true,
            "--out" => out = Some(it.next().ok_or_else(|| CliError::usage("--out needs a path"))?),
            _ => args.push(a),
        }
    }
    if ttl.is_none()
        && let Some(d) = ctx.cfg.get("self_destruct") {
            ttl = Some(parse_ttl(d)?);
        }
    if name == "screenshot" && !args.iter().any(|a| a == "--scale")
        && let Some(s) = ctx.cfg.get("screenshot_scale") {
            args.push("--scale".into());
            args.push(s.clone());
        }
    // Scripts and media are read here, on the caller's machine, and sent along.
    let mut attachments = Vec::new();
    let reads_files = matches!(name, "replay" | "test" | "install" | "reinstall" | "display" | "batch" | "adb");
    if reads_files {
        for (i, a) in args.clone().iter().enumerate() {
            let prev = i.checked_sub(1).map(|p| args[p].as_str());
            let is_path_flag = matches!(prev, Some("--image" | "--video" | "--steps-file"));
            let positional_file = !a.starts_with('-') && std::path::Path::new(a).is_file() && matches!(name, "replay" | "test" | "install" | "reinstall");
            let adb_file = name == "adb" && adb_local_input(&args, i);
            if (is_path_flag || positional_file || adb_file) && std::path::Path::new(a).is_file() {
                let bytes = std::fs::read(a).map_err(|e| CliError::usage(format!("reading {a}: {e}")))?;
                if bytes.len() > 8 << 20 {
                    return Err(CliError::usage(format!("{a} is larger than 8 MiB; upload it to Briefcase and pass the link instead")));
                }
                let fname = std::path::Path::new(a).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
                attachments.push(Attachment {
                    name: fname.clone(),
                    content_type: guess_type(&fname).into(),
                    content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                });
                args[i] = format!("attachment:{fname}");
            }
        }
    }
    let req = CommandRequest { command: name.to_owned(), args, timeout_ms: ctx.g.timeout, self_destruct_minutes: ttl, permanent: keep, attachments };
    let result = ctx.call(|c, t, team| { let (sid, req) = (sid.clone(), req.clone()); async move { c.authed(&t, team.as_deref()).run(&sid, &req).await } }).await;
    let result = match result {
        Ok(r) => r,
        Err(e) if e.code == ErrorCode::SessionEnded => {
            if store::current_session(&ctx.plane).as_deref() == Some(sid.as_str()) {
                store::set_current_session(&ctx.plane, None)?;
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    let mut saved = Vec::new();
    if let Some(o) = &out {
        for f in &result.files {
            saved.push(save_file(ctx, f, Some(o.clone())).await?);
        }
    }
    if ctx.g.json {
        emit(ctx, json!({"result": result, "saved_to": saved}), String::new);
    } else {
        let mut text = result.text.clone().unwrap_or_else(|| if result.output.is_null() { String::new() } else { serde_json::to_string_pretty(&result.output).unwrap_or_default() });
        for f in &result.files {
            text.push_str(&format!("\n{}", file_line(f)));
        }
        for p in &saved {
            text.push_str(&format!("\nSaved to {}", p.display()));
        }
        if result.ok {
            if !text.trim().is_empty() {
                println!("{}", text.trim_end());
            }
        } else {
            if !text.trim().is_empty() {
                println!("{}", text.trim_end());
            }
            if let Some(e) = &result.error {
                eprintln!("error: {} ({})", e.message, e.code);
            }
        }
    }
    Ok(if result.ok { 0 } else { 1 })
}

fn guess_type(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    match lower.rsplit('.').next().unwrap_or_default() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "apk" => "application/vnd.android.package-archive",
        "json" => "application/json",
        "ad" | "txt" | "yaml" | "yml" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globals_are_pulled_out_anywhere() {
        let (g, rest) = parse_globals(vec!["--test".into(), "abc".into(), "snapshot".into(), "-i".into(), "--json".into()]).unwrap();
        assert_eq!(g.test.as_deref(), Some("abc"));
        assert!(g.json);
        assert_eq!(rest, vec!["snapshot", "-i"]);
    }

    #[test]
    fn adb_files_only_attach_local_inputs() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(adb_local_input(&args(&["push", "file.bin", "/sdcard/file.bin"]), 1));
        assert!(!adb_local_input(&args(&["push", "file.bin", "/sdcard/file.bin"]), 2));
        assert!(adb_local_input(&args(&["install", "-r", "app.apk"]), 2));
        assert!(!adb_local_input(&args(&["shell", "cat", "file.bin"]), 2));
        assert!(!adb_local_input(&args(&["pull", "/sdcard/file.bin"]), 1));
    }

    #[test]
    fn ttl_parsing() {
        assert_eq!(parse_ttl("90m").unwrap(), 90);
        assert_eq!(parse_ttl("2h").unwrap(), 120);
        assert_eq!(parse_ttl("30d").unwrap(), 43_200);
        assert!(parse_ttl("31d").is_err());
        assert!(parse_ttl("0m").is_err());
    }

    #[test]
    fn args_flags() {
        let a = Args::parse(&["4f9c2a".into(), "--name".into(), "Pixel".into(), "--access".into(), "si:a".into(), "--access=si:b".into()], &["--name", "--access"]);
        assert_eq!(a.pos, vec!["4f9c2a"]);
        assert_eq!(a.value("--name").as_deref(), Some("Pixel"));
        assert_eq!(a.values("--access"), vec!["si:a", "si:b"]);
    }
}
