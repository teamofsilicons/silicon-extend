//! `extend` — find and use the devices a Silicon has access to; pair and manage them as a Carbon.
//!
//! Built only on `silicon-extend-client`. Everyone signs in with Silicon Accounts: Carbons with a
//! code they approve (`extend login`), Silicons with a short-lived token (`silicon-accounts login
//! --app extend -q | extend login --slt-stdin`). Stateful on disk (see `store.rs` and `signin.rs`);
//! never asks for a password. Every failure says what went wrong, why, and what to run next, and
//! exits with a code from `docs/migration/contracts/cli.yaml`.

mod args;
mod compat;
mod error;
mod help;
#[macro_use]
mod output;
mod retired;
mod signin;
mod store;

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use extend_protocol::capability::{self, COMMANDS, DeviceKind};
use extend_protocol::model::*;
use extend_protocol::{DeviceId, DeviceOs, ErrorCode};
use serde_json::{Value, json};
use silicon_extend_client::attachments::{self as attach, AttachmentError, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS};
use silicon_extend_client::auth::{self as accounts, SignIn};
use silicon_extend_client::{ActivityQuery, Client, DeviceQuery, ListQuery, StopOutcome};

use args::{Args, Globals, VERBATIM_COMMANDS, missing_value, parse_globals};
use error::{CliError, R};
use output::{Colors, Out, Stream};
use signin::{Loaded, Stored};

const DEFAULT_API: &str = "https://backend.extend.teamofsilicons.com";
const USER_AGENT: &str = concat!("extend-cli/", env!("CARGO_PKG_VERSION"));
/// How a Carbon signs in, and how a Silicon does.
const CARBON_SIGN_IN: &str = "extend login";
const SILICON_SIGN_IN: &str = accounts::SILICON_SIGN_IN;

// ───────────────────────────── Context ─────────────────────────────

/// Why no sign-in is in use although `auth.json` is there.
#[derive(Debug)]
enum Unusable {
    /// Extend 3's Silicon IAM sign-in, which Extend 4 can't use.
    Legacy { id: Option<String> },
    /// It can't be read, and why.
    Unreadable(String),
    /// It is for another Extend service or Silicon Accounts than this command uses.
    OtherOrigin(Box<Stored>),
}

struct Ctx {
    g: Globals,
    cfg: BTreeMap<String, String>,
    client: Option<Client>,
    auth: Option<Stored>,
    unusable: Option<Unusable>,
    out: Out,
}

fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

fn now_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

impl Ctx {
    fn api_url(&self) -> String {
        std::env::var("EXTEND_API_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.cfg.get("api_url").cloned())
            .unwrap_or_else(|| DEFAULT_API.into())
            .trim()
            .trim_end_matches('/')
            .to_owned()
    }

    fn accounts_url(&self) -> String {
        std::env::var("ACCOUNTS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.cfg.get("accounts_url").cloned())
            .unwrap_or_else(|| accounts::DEFAULT_ACCOUNTS_URL.into())
            .trim()
            .trim_end_matches('/')
            .to_owned()
    }

    fn telemetry_on(&self) -> bool {
        let v = std::env::var("EXTEND_TELEMETRY")
            .ok()
            .or_else(|| self.cfg.get("telemetry").cloned())
            .unwrap_or_else(|| "on".into());
        v != "off"
    }

    fn verbose_failure(&self, label: &str, e: &CliError, t: Instant) {
        self.out.verbose(|| {
            format!(
                "{label}: {}{} in {} ms{}",
                e.status.map(|s| format!("{s} ")).unwrap_or_default(),
                e.code.as_str(),
                ms(t),
                e.request_id
                    .as_ref()
                    .map(|r| format!(", request {r}"))
                    .unwrap_or_default()
            )
        });
    }

    fn timed<T, E: Into<CliError>>(&self, label: &str, t: Instant, r: Result<T, E>) -> R<T> {
        match r {
            Ok(v) => {
                self.out.verbose(|| format!("{label}: ok in {} ms", ms(t)));
                Ok(v)
            }
            Err(e) => {
                let e = e.into();
                self.verbose_failure(label, &e, t);
                Err(e)
            }
        }
    }

    async fn client(&mut self) -> R<Client> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        // A script that still sets EXTEND_TEST_SECRET was written for a test environment; Extend 4
        // has none, so the command would reach the real service. Nothing is sent.
        if std::env::var("EXTEND_TEST_SECRET").is_ok_and(|s| !s.trim().is_empty()) {
            return Err(CliError::usage(
                format!(
                    "EXTEND_TEST_SECRET is set, but Extend 4 has no test environments: this command would run against {}. Nothing was sent.",
                    self.api_url()
                ),
                "Unset EXTEND_TEST_SECRET. To try changes, point EXTEND_API_URL and ACCOUNTS_URL at a local Extend and Silicon Accounts.",
            ));
        }
        let url = self.api_url();
        let b = Client::builder(url.clone())
            .user_agent(USER_AGENT)
            .telemetry(self.telemetry_on())
            .isi(std::env::var("ISI").ok());
        let t = Instant::now();
        let c = match b.connect().await {
            Ok(c) => {
                self.out
                    .verbose(|| format!("GET {url}/api/version: API v{} in {} ms", c.api_version(), ms(t)));
                c
            }
            Err(e) => {
                let e = CliError::from(e);
                self.verbose_failure(&format!("GET {url}/api/version"), &e, t);
                return Err(e);
            }
        };
        self.client = Some(c.clone());
        Ok(c)
    }

    /// Signing in at the Silicon Accounts this command uses.
    fn sign_in(&self) -> R<SignIn> {
        sign_in_at(&self.accounts_url())
    }

    /// The account the saved sign-in is for (its uuid), which device-session caches are kept by.
    fn account(&self) -> Option<&str> {
        self.auth.as_ref().map(|a| a.uuid.as_str())
    }

    fn is_silicon(&self) -> bool {
        self.auth.as_ref().is_some_and(Stored::is_silicon)
    }

    /// The signed-in account's current public id.
    fn member_id(&self) -> Option<String> {
        self.auth.as_ref().map(|a| a.id.clone())
    }

    /// `extend <rest>`, the way to suggest a command.
    fn suggest(&self, rest: &str) -> String {
        format!("extend {rest}")
    }

    /// Not signed in: why (no sign-in, an Extend 3 one, an unreadable one, one for another service)
    /// and how to sign in.
    fn not_signed_in(&self) -> CliError {
        let how = format!("Carbons: `{CARBON_SIGN_IN}`. Silicons: `{SILICON_SIGN_IN}`.");
        match &self.unusable {
            None => CliError::new(ErrorCode::NotSignedIn, "You are not signed in to Silicon Extend.")
                .hint(format!("Sign in with Silicon Accounts. {how}")),
            Some(Unusable::Legacy { id }) => CliError::new(
                ErrorCode::NotSignedIn,
                format!(
                    "The saved sign-in{} is from Extend 3 (Silicon IAM), which Extend 4 no longer accepts.",
                    id.as_deref().map(|i| format!(" for {i}")).unwrap_or_default()
                ),
            )
            .hint(format!("Sign in again with Silicon Accounts; it replaces the old one. {how}")),
            Some(Unusable::Unreadable(why)) => CliError::new(
                ErrorCode::NotSignedIn,
                format!(
                    "The saved sign-in in {} can't be read: {why}.",
                    signin::path().display()
                ),
            )
            .hint(format!("Sign in again; it replaces the file. {how}")),
            Some(Unusable::OtherOrigin(s)) => CliError::new(
                ErrorCode::NotSignedIn,
                format!(
                    "You are signed in as {} for the Extend at {} (Silicon Accounts {}), but this command uses the Extend at {} (Silicon Accounts {}), so the sign-in isn't sent there.",
                    s.id,
                    s.api_url,
                    s.accounts_url,
                    self.api_url(),
                    self.accounts_url()
                ),
            )
            .hint(format!(
                "Sign in for this Extend ({how}), or set EXTEND_API_URL and ACCOUNTS_URL back (`extend config ls` shows the settings)."
            )),
        }
    }

    fn require_auth(&self) -> R<Stored> {
        self.auth.clone().ok_or_else(|| self.not_signed_in())
    }

    /// The access token to send, refreshed first when less than a minute is left. If Silicon
    /// Accounts can't be reached then but the token still works, it is used as it is.
    async fn token(&mut self) -> R<String> {
        let auth = self.require_auth()?;
        if auth.seconds_left(now_s()) >= signin::REFRESH_WITHIN_S {
            return Ok(auth.access_token);
        }
        match self.refresh(&auth.access_token).await {
            Ok(fresh) => Ok(fresh.access_token),
            Err(e) if e.code == ErrorCode::ServiceUnavailable && auth.seconds_left(now_s()) > 0 => {
                self.out.verbose(|| {
                    format!(
                        "refreshing the sign-in failed ({}); the access token still works for {} s",
                        e.message,
                        auth.seconds_left(now_s())
                    )
                });
                Ok(auth.access_token)
            }
            Err(e) => Err(e),
        }
    }

    /// Runs a signed-in call. When Extend refuses the access token as expired, the sign-in is
    /// refreshed once (single-flight) and the call repeated. `label` names the call for `-v`.
    async fn call<T, F, Fut>(&mut self, label: &str, f: F) -> R<T>
    where
        F: Fn(Client, String) -> Fut,
        Fut: std::future::Future<Output = Result<T, silicon_extend_client::Error>>,
    {
        let client = self.client().await?;
        let token = self.token().await?;
        let t = Instant::now();
        match f(client.clone(), token.clone()).await {
            Err(e) if silicon_extend_client::needs_refresh(&e) => {
                self.verbose_failure(label, &CliError::from(e), t);
                let fresh = self.refresh(&token).await?;
                let t = Instant::now();
                let r = f(client, fresh.access_token).await;
                self.timed(label, t, r)
            }
            other => self.timed(label, t, other),
        }
    }

    /// Refreshes the saved sign-in once, under the refresh lock. `used` is the access token that
    /// was found stale: when the saved one differs (another process refreshed while this one
    /// waited) and still works, that one is used and nothing is sent. The new pair is saved before
    /// it is used. A refused refresh means the sign-in is over: the file is deleted.
    async fn refresh(&mut self, used: &str) -> R<Stored> {
        let mine = self.require_auth()?;
        let _lock = signin::lock(Duration::from_secs(60)).map_err(|why| {
            CliError::new(
                ErrorCode::ServiceUnavailable,
                format!("Couldn't refresh the sign-in: {why}."),
            )
            .hint("Run the command again in a moment.")
        })?;
        let saved = match signin::load() {
            Loaded::Current(s)
                if s.uuid == mine.uuid && s.api_url == mine.api_url && s.accounts_url == mine.accounts_url =>
            {
                *s
            }
            Loaded::Current(s) => {
                self.auth = None;
                return Err(CliError::new(
                    ErrorCode::NotSignedIn,
                    format!(
                        "The saved sign-in changed to {} while this command ran, so it stopped.",
                        s.id
                    ),
                )
                .hint("Run the command again."));
            }
            _ => {
                self.auth = None;
                return Err(CliError::new(
                    ErrorCode::NotSignedIn,
                    format!("{} was signed out while this command ran.", mine.id),
                )
                .hint(format!("Carbons: `{CARBON_SIGN_IN}`. Silicons: `{SILICON_SIGN_IN}`.")));
            }
        };
        if saved.access_token != used && saved.seconds_left(now_s()) >= signin::REFRESH_WITHIN_S {
            self.out
                .verbose(|| "another extend process refreshed the sign-in; using its tokens".into());
            self.auth = Some(saved.clone());
            return Ok(saved);
        }
        let sign_in = sign_in_at(&saved.accounts_url)?;
        let t = Instant::now();
        let label = format!("POST {}/v1/oauth/token (refresh)", saved.accounts_url);
        let r = sign_in.refresh(&saved.refresh_token).await;
        match r {
            Ok(tokens) => {
                self.out.verbose(|| format!("{label}: ok in {} ms", ms(t)));
                let fresh = saved
                    .refreshed(tokens)
                    .map_err(|m| CliError::new(ErrorCode::Unauthorized, m).hint("Sign in again."))?;
                // Whatever replaced the file while the refresh was out (a writer that didn't wait for
                // the lock) is newer than this sign-in: it is kept, and these tokens are dropped.
                match signin::load() {
                    Loaded::Current(now) if now.refresh_token == saved.refresh_token => {}
                    _ => {
                        let _ = sign_in.revoke(&fresh.refresh_token).await;
                        self.auth = None;
                        return Err(CliError::new(
                            ErrorCode::Unauthorized,
                            "The saved sign-in changed while it was being refreshed, so the newer one was kept and this command stopped.",
                        )
                        .hint("Run the command again."));
                    }
                }
                signin::save(&fresh)?;
                self.auth = Some(fresh.clone());
                Ok(fresh)
            }
            Err(e) if e.sign_in_ended() => {
                let e = CliError::from(e);
                self.verbose_failure(&label, &e, t);
                signin::remove();
                store::forget_sessions(&saved.uuid);
                self.auth = None;
                Err(e)
            }
            Err(e) => {
                let e = CliError::from(e);
                self.verbose_failure(&label, &e, t);
                Err(e)
            }
        }
    }

    fn session_id(&self) -> R<String> {
        self.g
            .session
            .clone()
            .or_else(|| std::env::var("EXTEND_SESSION").ok().filter(|s| !s.is_empty()))
            .or_else(|| store::current_session(self.account()))
            .ok_or_else(|| {
                CliError::new(ErrorCode::NoSession, "No session selected.").hint(format!(
                    "Run `{}`, or pass --session <session_id>.",
                    self.suggest("session new <device_id> --connect")
                ))
            })
    }

    fn emit(&self, data: Value, text: impl FnOnce() -> String) {
        self.out.emit(data, text);
    }
}

/// Signing in to Extend at the Silicon Accounts at `url`.
fn sign_in_at(url: &str) -> R<SignIn> {
    SignIn::for_app(url, extend_protocol::APP_ID, Some(USER_AGENT)).map_err(CliError::from)
}

// ───────────────────────────── Output helpers ─────────────────────────────

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn fmt_time(t: &time::OffsetDateTime) -> String {
    t.format(&time::macros::format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second]Z"
    ))
    .unwrap_or_default()
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
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .map(|r| r.get(c).map_or(0, |x| x.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, x)| {
                    if i + 1 == cols {
                        x.clone()
                    } else {
                        format!("{x:<w$}", w = widths[i])
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_owned()
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
    let cfg = store::load_config();
    let colors = Colors::detect(cfg.get("color").map(String::as_str));
    let json_setting = cfg.get("output").is_some_and(|o| o == "json");
    let (g, rest) = match parse_globals(argv) {
        Ok(v) => v,
        Err(failed) => {
            let (g, e) = *failed;
            let out = Out {
                json: g.json || json_setting,
                colors,
                verbose: g.verbose,
            };
            return out.fail(&e);
        }
    };
    let out = Out {
        json: g.json || json_setting,
        colors,
        verbose: g.verbose,
    };
    if let Some(flag) = g.retired {
        return out.fail(&retired::global_flag(flag));
    }
    let mut ctx = Ctx {
        g,
        cfg,
        client: None,
        auth: None,
        unusable: None,
        out,
    };
    load_sign_in(&mut ctx);
    let started = Instant::now();
    let result = dispatch(&mut ctx, rest.clone()).await;
    let code = match &result {
        Ok(c) => *c,
        Err(e) => ctx.out.fail(e),
    };
    telemetry(&mut ctx, &rest, &result, started.elapsed()).await;
    ctx.out
        .verbose(|| format!("finished in {} ms with exit code {code}", ms(started)));
    code
}

/// Reads the saved sign-in. It is used only for the Extend service and Silicon Accounts it was made
/// for: a command aimed elsewhere never sends its tokens there.
fn load_sign_in(ctx: &mut Ctx) {
    match signin::load() {
        Loaded::Missing => {}
        Loaded::Current(s) => {
            if s.api_url == ctx.api_url() && s.accounts_url == ctx.accounts_url() {
                ctx.auth = Some(*s);
            } else {
                ctx.unusable = Some(Unusable::OtherOrigin(s));
            }
        }
        Loaded::Legacy { id } => ctx.unusable = Some(Unusable::Legacy { id }),
        Loaded::Unreadable(why) => ctx.unusable = Some(Unusable::Unreadable(why)),
    }
}

async fn telemetry(ctx: &mut Ctx, rest: &[String], result: &R<i32>, took: Duration) {
    if !ctx.telemetry_on() || ctx.auth.is_none() || ctx.client.is_none() {
        return;
    }
    let (command, step) = telemetry_step(rest);
    let event = json!({
        "source": "cli",
        "event": "command",
        "step": step,
        "success": matches!(result, Ok(0)),
        "duration_ms": took.as_millis() as u64,
        "error_code": result.as_ref().err().map(|e| e.code.as_str()),
        "command": command,
        "client_version": env!("CARGO_PKG_VERSION"),
    });
    let (Some(client), Some(auth)) = (ctx.client.clone(), ctx.auth.clone()) else {
        return;
    };
    // Never refreshes for telemetry: an expired token just means no event.
    if auth.seconds_left(now_s()) <= 0 {
        return;
    }
    let _ = tokio::time::timeout(Duration::from_millis(800), async move {
        client.authed(&auth.access_token).telemetry(event).await
    })
    .await;
}

/// Sub-command words that may appear in telemetry after a known command.
const TELEMETRY_WORDS: &[&str] = &[
    "ls",
    "show",
    "pair",
    "attach",
    "setup",
    "setup-code",
    "rename",
    "visibility",
    "ttl",
    "stop",
    "rm",
    "access",
    "activity",
    "requests",
    "new",
    "connect",
    "disconnect",
    "status",
    "end",
    "release",
    "send",
    "get",
    "keep",
    "set",
    "unset",
    "home",
    "renounce",
    "wake",
    "wake-requests",
    "on",
    "start",
    "mark",
    "clear",
    "run",
    "read",
    "write",
    "dismiss",
    "accept",
    "wait",
    "shell",
    "exec-out",
    "logcat",
    "push",
    "pull",
    "install",
    "uninstall",
    "get-state",
    "text",
    "attrs",
    "snapshot",
    "screenshot",
    "press",
    "longpress",
    "visible",
    "hidden",
    "exists",
    "absent",
    "editable",
    "selected",
    "focused",
    "up",
    "down",
    "left",
    "right",
    "pan",
    "fling",
    "drag",
    "pinch",
    "rotate",
    "transform",
    "label",
    "role",
    "list",
    "click",
    "fill",
];

/// The `command` and `step` telemetry reports: the command name and a known sub-command word, never
/// an argument (a short-lived token, a pairing code, typed text, a path).
fn telemetry_step(rest: &[String]) -> (String, String) {
    let mut words = rest.iter().map(String::as_str).filter(|s| !s.starts_with('-'));
    let command = match words.next() {
        Some(c) if help::find(c).is_some() || capability::command(c).is_some() || c == "help" => c,
        Some(_) => "unknown",
        None => return (String::new(), "cli".into()),
    };
    let redacted = capability::command(command).is_some_and(|c| c.redact_text);
    match words.next() {
        Some(sub) if !redacted && TELEMETRY_WORDS.contains(&sub) => {
            (command.to_owned(), format!("cli.{command}.{sub}"))
        }
        _ => (command.to_owned(), format!("cli.{command}")),
    }
}

/// The session `--help` describes: the one commands would run in, if its device is cached.
fn connected_cache(ctx: &Ctx) -> Option<store::SessionCache> {
    let sid = ctx.session_id().ok()?;
    store::load_session_cache(ctx.account(), &sid)
}

fn with_connected<T>(ctx: &Ctx, f: impl FnOnce(Option<&help::Connected>) -> T) -> T {
    match connected_cache(ctx) {
        Some(c) => f(Some(&help::Connected {
            session_id: &c.session_id,
            device: &c.device_name,
            os: &c.os,
            commands: &c.commands,
            missing: &c.missing,
            refreshed_at: c.refreshed_at,
        })),
        None => f(None),
    }
}

fn top_help(ctx: &Ctx) -> String {
    with_connected(ctx, help::render_top)
}

/// `extend <words> --help`.
fn help_for(ctx: &Ctx, words: &[String]) -> String {
    let words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !w.starts_with('-'))
        .collect();
    let Some(first) = words.first() else {
        return top_help(ctx);
    };
    if let Some(sub) = words.get(1)
        && let Some(n) = help::find(&format!("{first} {sub}"))
    {
        return help::render_node(n);
    }
    if let Some(n) = help::find(first) {
        return help::render_node(n);
    }
    if let Some(t) = with_connected(ctx, |on| help::render_device_command(first, on)) {
        return t;
    }
    top_help(ctx)
}

async fn dispatch(ctx: &mut Ctx, rest: Vec<String>) -> R<i32> {
    if ctx.g.version && rest.is_empty() {
        return version(ctx, &[]).await;
    }
    let Some(cmd) = rest.first().cloned() else {
        out!("{}", top_help(ctx));
        return Ok(0);
    };
    if ctx.g.help {
        out!("{}", help_for(ctx, &rest));
        return Ok(0);
    }
    if cmd.len() > 1 && cmd.starts_with('-') {
        return Err(CliError::usage(
            format!("{cmd} is not a global flag, and a command comes before its own flags"),
            format!(
                "Global flags: {}. Put a command's own flags after it, like `extend device ls --online`.",
                args::GLOBAL_FLAG_NAMES.join(", ")
            ),
        ));
    }
    let words: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|w| !w.starts_with('-'))
        .collect();
    if let Some(e) = retired::command(&words) {
        return Err(e);
    }
    let args = rest[1..].to_vec();
    let sub = args.first().cloned().unwrap_or_default();
    match cmd.as_str() {
        "help" => {
            out!("{}", help_for(ctx, &args));
            Ok(0)
        }
        "login" if sub == "status" => login_status(ctx, &args[1..]).await,
        "login" => login(ctx, &args).await,
        "logout" => logout(ctx, &args).await,
        "accounts" => accounts_info(ctx, &args, "accounts"),
        // Hidden: Extend 3's `iam --json`, which the Silicon runtime still runs. Same answer.
        "iam" => accounts_info(ctx, &args, "iam"),
        "silicon" => silicon(ctx, &args).await,
        "config" => config(ctx, &args).await,
        "device" => device(ctx, &args).await,
        "session" => session(ctx, &args).await,
        "takeover" => takeover(ctx, &args).await,
        "request" => request(ctx, &args).await,
        "ting" => ting(ctx, &args).await,
        "file" => file(ctx, &args).await,
        "report" => report(ctx, &args).await,
        "version" => version(ctx, &args).await,
        "docs" => {
            Args::parse(&args, "docs")?.at_most(0)?;
            ctx.emit(
                json!({"repository": help::REPO, "docs": help::DOCS, "website": help::WEBSITE, "crate": help::CRATE, "state": store::root(), "install": help::INSTALL, "update": compat::UPDATE}),
                || {
                    format!(
                        "Docs      {}\nSource    {}\nWebsite   {}\nRust      {}\nState     {}\nInstall   {}\nUpdates   automatic through Silicon Apps; check now with `{}`",
                        help::DOCS,
                        help::REPO,
                        help::WEBSITE,
                        help::CRATE,
                        store::root().display(),
                        help::INSTALL,
                        compat::UPDATE
                    )
                },
            );
            Ok(0)
        }
        other if capability::command(other).is_some() => device_command(ctx, other, args).await,
        other => {
            if let Some(repl) = capability::not_exposed(other) {
                let e = CliError::new(
                    ErrorCode::UnknownCommand,
                    format!("`{other}` is a device engine command Extend doesn't relay."),
                );
                return Err(match repl {
                    Some(r) => e.hint(format!("Use `{r}` instead.")),
                    None => e.hint("Extend leaves out the device engine's tools for app developers (simulators, emulators, React Native, web)."),
                });
            }
            let near: Vec<&str> = help::NODES
                .iter()
                .map(|n| n.path)
                .chain(COMMANDS.iter().map(|c| c.name))
                .filter(|n| !n.contains(' ') && (n.starts_with(&other[..1.min(other.len())]) || strsim(n, other)))
                .take(4)
                .collect();
            Err(CliError::new(
                ErrorCode::UnknownCommand,
                format!("`{other}` is not an extend command."),
            )
            .hint(if near.is_empty() {
                "Run `extend --help` to see the tree.".into()
            } else {
                format!(
                    "Did you mean: {}? Run `extend --help` to see the tree.",
                    near.join(", ")
                )
            }))
        }
    }
}

fn strsim(a: &str, b: &str) -> bool {
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    common >= 3
}

/// Splits `args` into a sub-command and its arguments; `default` when the first word is a flag or
/// missing.
fn sub_and_rest<'a>(args: &'a [String], default: &str) -> (String, &'a [String]) {
    match args.first() {
        Some(s) if !s.starts_with('-') => (s.clone(), &args[1..]),
        _ => (default.to_owned(), args),
    }
}

fn unknown_sub(parent: &str, sub: &str) -> CliError {
    let subs: Vec<&str> = args::SPECS
        .iter()
        .filter_map(|s| s.path.strip_prefix(&format!("{parent} ")))
        .filter(|s| !s.contains(' '))
        .collect();
    CliError::new(
        ErrorCode::UnknownCommand,
        format!("`extend {parent} {sub}` is not a command."),
    )
    .hint(format!(
        "`extend {parent}` has: {}. See `extend {parent} --help`.",
        subs.join(", ")
    ))
}

// ───────────────────────────── Getting started ─────────────────────────────

/// RFC 3339 for unix seconds.
fn rfc3339(unix: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

/// What `login`, `login status` and `logout` say about a sign-in, as JSON.
fn account_json(s: &Stored) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("uuid".into(), json!(s.uuid));
    m.insert("id".into(), json!(s.id));
    m.insert("kind".into(), json!(s.kind));
    if let Some(n) = &s.display_name {
        m.insert("display_name".into(), json!(n));
    }
    if let Some(c) = &s.custodian {
        m.insert("custodian".into(), json!({"uuid": c.uuid, "id": c.id}));
    }
    m
}

/// `c:ada (Ada Lovelace), a Carbon` / `si:scout, a Silicon looked after by c:ada`.
fn who_text(s: &Stored) -> String {
    let name = s
        .display_name
        .as_deref()
        .filter(|n| !n.is_empty() && *n != s.id)
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    let kind = if s.is_silicon() {
        match &s.custodian {
            Some(c) => format!("a Silicon looked after by {}", c.id),
            None => "a Silicon".into(),
        }
    } else {
        "a Carbon".into()
    };
    format!("{}{name}, {kind}", s.id)
}

/// The values a sign-in needs, checked before anything is sent: the two URLs and a state directory
/// that can hold the result.
fn check_sign_in_setup(ctx: &Ctx) -> R<()> {
    accounts::check_url(&ctx.accounts_url(), "The Silicon Accounts URL").map_err(|m| {
        CliError::usage(
            m,
            "Set ACCOUNTS_URL (or `extend config set accounts_url <url>`) to the Silicon Accounts to sign in at; production is https://accounts.teamofsilicons.com.",
        )
    })?;
    accounts::check_url(&ctx.api_url(), "The Extend service URL").map_err(|m| {
        CliError::usage(
            m,
            "Set EXTEND_API_URL (or `extend config set api_url <url>`); production is https://backend.extend.teamofsilicons.com.",
        )
    })?;
    signin::check_writable().map_err(|why| {
        CliError::usage(
            format!("Extend can't save a sign-in in {}: {why}. Nothing was sent.", store::root().display()),
            "Make the directory writable by this user, or use another one: SILICON_HOME=<dir>, or `extend config home <dir>`.",
        )
    })
}

/// Asks the Extend service which Silicon Accounts it trusts, before a token is spent: a sign-in at
/// another one would be refused there. When the service can't be asked, it says so and goes on.
async fn check_service_trusts(ctx: &mut Ctx) -> R<()> {
    let client = match ctx.client().await {
        Ok(c) => c,
        Err(e) if matches!(e.code, ErrorCode::ServiceUnavailable | ErrorCode::Internal) => {
            ctx.out.warn(&format!(
                "Couldn't ask the Extend at {} which Silicon Accounts it trusts ({}); signing in at {} anyway.",
                ctx.api_url(),
                e.message,
                ctx.accounts_url()
            ));
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let t = Instant::now();
    let info = match ctx.timed("GET /api/v2/accounts", t, client.accounts().await) {
        Ok(i) => i,
        Err(e) if matches!(e.code, ErrorCode::ServiceUnavailable | ErrorCode::Internal) => {
            ctx.out.warn(&format!(
                "Couldn't ask the Extend at {} which Silicon Accounts it trusts ({}); signing in at {} anyway.",
                ctx.api_url(),
                e.message,
                ctx.accounts_url()
            ));
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let ours = ctx.accounts_url();
    let theirs = info.accounts_url.trim_end_matches('/').to_owned();
    if theirs != ours || info.app_id != extend_protocol::APP_ID {
        return Err(CliError::usage(
            format!(
                "The Extend at {} trusts Silicon Accounts at {theirs} (app {}), but this CLI signs in at {ours}. Nothing was sent: a sign-in there wouldn't work here.",
                ctx.api_url(),
                info.app_id
            ),
            format!("Set ACCOUNTS_URL={theirs} (or `extend config set accounts_url {theirs}`), or point EXTEND_API_URL at the Extend that uses {ours}."),
        )
        .details(json!({"reason": "accounts_mismatch", "service_trusts": theirs, "cli_signs_in_at": ours})));
    }
    Ok(())
}

/// Reads the short-lived token: `--slt`, the positional argument, or stdin. Never printed.
fn read_slt(a: &Args) -> R<Option<(String, &'static str)>> {
    let positional = a.pos.first().cloned();
    let flag = a.value("--slt");
    let stdin = a.flag("--slt-stdin");
    let given = [positional.is_some(), flag.is_some(), stdin]
        .iter()
        .filter(|x| **x)
        .count();
    if given > 1 {
        return Err(CliError::usage(
            "Give the short-lived token one way: as `--slt <token>`, on stdin with `--slt-stdin`, or as the argument.",
            format!("The usual way keeps it out of the process list: {SILICON_SIGN_IN}"),
        ));
    }
    if given == 1 && (a.flag("--open") || a.value("--label").is_some()) {
        return Err(CliError::usage(
            "--open and --label are for a Carbon's sign-in with a code, not a short-lived token.",
            format!("Carbons: `{CARBON_SIGN_IN} [--open] [--label <text>]`. Silicons: `{SILICON_SIGN_IN}`."),
        ));
    }
    if let Some(t) = flag {
        return Ok(Some((t, "--slt")));
    }
    if let Some(t) = positional {
        return Ok(Some((t, "argument")));
    }
    if !stdin {
        return Ok(None);
    }
    if std::io::stdin().is_terminal() {
        errout!("Paste the short-lived token (slt_…) and press Enter: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| CliError::usage(format!("Could not read the token from stdin: {e}"), SILICON_SIGN_IN))?;
        return Ok(Some((line.trim().to_owned(), "stdin")));
    }
    let mut buf = String::new();
    std::io::stdin()
        .take(16 * 1024)
        .read_to_string(&mut buf)
        .map_err(|e| CliError::usage(format!("Could not read the token from stdin: {e}"), SILICON_SIGN_IN))?;
    let t = buf.trim().to_owned();
    if t.is_empty() {
        return Err(CliError::usage(
            "--slt-stdin read nothing: stdin was empty.",
            format!("Pipe the token in: {SILICON_SIGN_IN}"),
        ));
    }
    Ok(Some((t, "stdin")))
}

/// Opens `url` in the browser, best effort (`--open`).
fn open_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// `extend login`: Carbons approve a code at Silicon Accounts (the device flow); Silicons hand over
/// a short-lived token (`--slt`, `--slt-stdin`, or as the argument). Either way the tokens are for
/// Extend alone, kept in `{state}/auth.json`.
async fn login(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, "login")?;
    a.at_most(1)?;
    let slt = read_slt(&a)?;
    check_sign_in_setup(ctx)?;
    check_service_trusts(ctx).await?;
    let sign_in = ctx.sign_in()?;
    let (tokens, method) = match slt {
        Some((token, _how)) => {
            let t = Instant::now();
            let r = sign_in.exchange_slt(&token).await;
            drop(token);
            (
                ctx.timed(
                    &format!("POST {}/v1/oauth/token (short-lived token)", sign_in.accounts_url()),
                    t,
                    r,
                )?,
                "slt",
            )
        }
        None => (device_sign_in(ctx, &sign_in, &a).await?, "device"),
    };
    let stored = Stored::from_tokens(tokens, &ctx.accounts_url(), &ctx.api_url(), method, now_s()).map_err(|why| {
        CliError::new(
            ErrorCode::Internal,
            format!("Silicon Accounts signed you in, but {why}, so the sign-in couldn't be saved."),
        )
        .hint("Run `extend login` again; if it keeps happening, `extend report` it.")
    })?;
    let previous = {
        let _lock = signin::lock(Duration::from_secs(60)).map_err(|why| {
            CliError::new(
                ErrorCode::ServiceUnavailable,
                format!("Couldn't save the sign-in: {why}."),
            )
            .hint("Run `extend login` again in a moment.")
        })?;
        let previous = signin::load();
        signin::save(&stored)?;
        previous
    };
    let cleaned = store::remove_legacy_state();
    ctx.auth = Some(stored.clone());
    ctx.unusable = None;
    // A sign-in this one replaced. Another account's ends, as `extend logout` would end it; the same
    // account's is left to lapse, because ending it would also end the account's running device
    // sessions (Extend treats any sign-out of Extend as one).
    let mut replaced = None;
    if let Loaded::Current(old) = &previous
        && old.uuid != stored.uuid
    {
        let revoked = match sign_in_at(&old.accounts_url) {
            Ok(si) => si.revoke(&old.refresh_token).await.is_ok(),
            Err(_) => false,
        };
        store::forget_sessions(&old.uuid);
        replaced = Some(json!({"id": old.id, "uuid": old.uuid, "revoked": revoked}));
    }
    // Check the new sign-in with Extend itself (and pick up the name and custodian it knows).
    let me = ctx
        .call("GET /api/v2/me", |c, t| async move { c.authed(&t).me().await })
        .await;
    let verified = match me {
        Ok(me) => {
            if let Some(mut s) = ctx.auth.clone() {
                s.id = me.id.clone();
                s.display_name = me.display_name.clone().or(s.display_name);
                if let Some(c) = &me.custodian {
                    s.custodian = Some(accounts::Custodian {
                        uuid: c.uuid.clone(),
                        id: c.id.clone(),
                    });
                }
                if s != stored {
                    let _ = signin::save(&s);
                }
                ctx.auth = Some(s);
            }
            true
        }
        Err(e) => {
            ctx.out.warn(&format!(
                "Signed in at Silicon Accounts, but the Extend at {} didn't confirm it: {} ({}).",
                ctx.api_url(),
                e.message,
                e.code.as_str()
            ));
            false
        }
    };
    let s = ctx.auth.clone().unwrap_or(stored);
    let mut data = account_json(&s);
    data.insert("event".into(), json!("signed_in"));
    data.insert("authenticated".into(), json!(true));
    data.insert("method".into(), json!(s.method));
    data.insert("expires_at".into(), json!(rfc3339(s.expires_at)));
    data.insert("refresh_expires_at".into(), json!(s.refresh_expires_at.map(rfc3339)));
    data.insert("verified".into(), json!(verified));
    data.insert("accounts_url".into(), json!(s.accounts_url));
    data.insert("api_url".into(), json!(s.api_url));
    if let Some(r) = &replaced {
        data.insert("replaced".into(), r.clone());
    }
    if !cleaned.is_empty() {
        data.insert("removed_extend_3_state".into(), json!(cleaned));
    }
    ctx.emit(Value::Object(data), || {
        let mut t = format!("Signed in to Extend as {}.", who_text(&s));
        t.push_str(&format!(
            "\n  uuid {}  ·  access token refreshed automatically  ·  sign-in lasts until {}",
            s.uuid,
            s.refresh_expires_at
                .map(|e| rfc3339(e).chars().take(10).collect::<String>())
                .unwrap_or_else(|| "you sign out".into())
        ));
        if let Some(r) = &replaced {
            t.push_str(&format!(
                "\n  It replaced the sign-in of {} on this machine{}.",
                r["id"].as_str().unwrap_or_default(),
                if r["revoked"] == true {
                    ", which was signed out"
                } else {
                    ""
                }
            ));
        }
        if !cleaned.is_empty() {
            t.push_str("\n  Removed the sign-ins Extend 3 saved here (Silicon IAM, no longer used).");
        }
        t.push_str(if s.is_silicon() {
            "\nNext: extend device ls"
        } else {
            "\nNext: extend device ls, or pair a device: extend device pair <code> --name <name>"
        });
        t
    });
    Ok(0)
}

/// The device flow: shows the code (with `--json`, one line per event on stdout), opens the page
/// with `--open`, and waits for the Carbon to approve it.
async fn device_sign_in(ctx: &mut Ctx, sign_in: &SignIn, a: &Args) -> R<accounts::Tokens> {
    let label = a.value("--label").unwrap_or_else(|| {
        let host = std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .filter(|h| !h.trim().is_empty());
        match host {
            Some(h) => format!("extend CLI on {h}"),
            None => "extend CLI".into(),
        }
    });
    let t = Instant::now();
    let r = sign_in.start_device(Some(&label)).await;
    let code = ctx.timed(&format!("POST {}/v1/device/authorize", sign_in.accounts_url()), t, r)?;
    let link = code
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| code.verification_uri.clone());
    if ctx.out.json {
        outln!(
            "{}",
            json!({"event": "device_code", "user_code": code.user_code, "verification_uri": code.verification_uri,
                   "verification_uri_complete": code.verification_uri_complete, "expires_in": code.expires_in,
                   "expires_at": rfc3339(code.expires_at.unix_timestamp()), "interval": code.interval,
                   "accounts_url": sign_in.accounts_url()})
        );
        let _ = std::io::stdout().flush();
    } else {
        errln!(
            "To sign in to Extend, open {} and enter the code\n\n    {}\n\n(or open {link}). The code works for {} minutes.",
            code.verification_uri,
            ctx.out.colors.paint(Stream::Err, "1", &code.user_code),
            code.expires_in.div_ceil(60)
        );
        errln!(
            "{}",
            ctx.out.colors.paint(
                Stream::Err,
                "2",
                &format!("Waiting for a Carbon to approve it… (Silicons sign in with `{SILICON_SIGN_IN}` instead.)")
            )
        );
    }
    if a.flag("--open") && !open_browser(&link) && !ctx.out.json {
        errln!("(Couldn't open a browser here; open the link yourself.)");
    }
    let json = ctx.out.json;
    let verbose = ctx.out.verbose;
    let t = Instant::now();
    let r = sign_in
        .wait_for_device(&code, |p| match p {
            accounts::DeviceProgress::SlowedDown { interval } => {
                if json {
                    outln!("{}", json!({"event": "slow_down", "interval": interval.as_secs()}));
                    let _ = std::io::stdout().flush();
                } else if verbose {
                    errln!("[extend] Silicon Accounts asked to poll every {} s", interval.as_secs());
                }
            }
            accounts::DeviceProgress::Retrying { message, retry_in } => {
                if json {
                    outln!(
                        "{}",
                        json!({"event": "retrying", "message": message, "retry_in": retry_in.as_secs()})
                    );
                    let _ = std::io::stdout().flush();
                } else {
                    errln!(
                        "(Couldn't check for approval: {message} Trying again in {} s.)",
                        retry_in.as_secs()
                    );
                }
            }
            accounts::DeviceProgress::Waiting { .. } => {}
        })
        .await;
    ctx.timed(
        &format!("POST {}/v1/oauth/token (device code)", sign_in.accounts_url()),
        t,
        r,
    )
}

/// `extend login status`: who is signed in, checked with Extend now unless `--offline`. With
/// `--json` it always exits 0 (`{"authenticated":false}` when signed out); without, 1 when signed
/// out.
async fn login_status(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, "login status")?;
    a.at_most(0)?;
    let signed_out = |ctx: &Ctx, reason: Option<String>| {
        let mut data = json!({"authenticated": false});
        if let Some(r) = &reason {
            data["reason"] = json!(r);
        }
        let hint = format!("Carbons: `{CARBON_SIGN_IN}`. Silicons: `{SILICON_SIGN_IN}`.");
        ctx.emit(data, || match &reason {
            Some(r) => format!("Not signed in: {r}\n{hint}"),
            None => format!("Not signed in to Extend.\n{hint}"),
        });
        Ok(if ctx.out.json { 0 } else { 1 })
    };
    let Some(auth) = ctx.auth.clone() else {
        let reason = ctx.unusable.as_ref().map(|_| ctx.not_signed_in().message);
        return signed_out(ctx, reason);
    };
    let mut verified = false;
    let mut problem = None;
    if !a.flag("--offline") {
        match ctx
            .call("GET /api/v2/me", |c, t| async move { c.authed(&t).me().await })
            .await
        {
            Ok(me) => {
                verified = true;
                if let Some(mut s) = ctx.auth.clone() {
                    s.id = me.id.clone();
                    s.display_name = me.display_name.clone().or(s.display_name);
                    if let Some(c) = &me.custodian {
                        s.custodian = Some(accounts::Custodian {
                            uuid: c.uuid.clone(),
                            id: c.id.clone(),
                        });
                    }
                    if Some(&s) != ctx.auth.as_ref() {
                        let _ = signin::save(&s);
                    }
                    ctx.auth = Some(s);
                }
            }
            // The sign-in is over (a refused refresh deleted it), or Extend refuses the token.
            Err(e)
                if ctx.auth.is_none()
                    || matches!(
                        e.code,
                        ErrorCode::TokenExpired | ErrorCode::Unauthorized | ErrorCode::NotSignedIn
                    ) =>
            {
                return signed_out(
                    ctx,
                    Some(
                        format!("{} {}", e.message, e.hint.unwrap_or_default())
                            .trim()
                            .to_owned(),
                    ),
                );
            }
            Err(e) => problem = Some(e),
        }
    }
    let s = ctx.auth.clone().unwrap_or(auth);
    let mut data = account_json(&s);
    data.insert("authenticated".into(), json!(true));
    data.insert("expires_at".into(), json!(rfc3339(s.expires_at)));
    data.insert("refresh_expires_at".into(), json!(s.refresh_expires_at.map(rfc3339)));
    data.insert("verified".into(), json!(verified));
    data.insert("method".into(), json!(s.method));
    data.insert("accounts_url".into(), json!(s.accounts_url));
    data.insert("api_url".into(), json!(s.api_url));
    if let Some(e) = &problem {
        data.insert("verify_error".into(), e.to_json()["error"].clone());
    }
    ctx.emit(Value::Object(data), || {
        let mut t = format!("Signed in to Extend as {} (uuid {}).", who_text(&s), s.uuid);
        t.push_str(&format!(
            "\n  {}  ·  access token until {}  ·  sign-in until {}",
            if verified {
                "checked with Extend just now".to_owned()
            } else if a.flag("--offline") {
                "read from this machine only (--offline)".to_owned()
            } else {
                format!(
                    "not checked: {}",
                    problem.as_ref().map(|e| e.message.clone()).unwrap_or_default()
                )
            },
            rfc3339(s.expires_at),
            s.refresh_expires_at
                .map(rfc3339)
                .unwrap_or_else(|| "you sign out".into())
        ));
        t
    });
    Ok(0)
}

/// `extend logout`: Extend ends what the sign-in runs (a Silicon's sessions; for a Carbon, the
/// sessions of the Silicons they gave access to, through their own pairs) and revokes the refresh
/// token at Silicon Accounts; if Extend can't be reached, the CLI revokes it there itself. The saved
/// sign-in is deleted either way. Signed out already: says so and exits 0.
async fn logout(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    Args::parse(args, "logout")?.at_most(0)?;
    let Some(auth) = ctx.auth.clone() else {
        return logout_unused(ctx).await;
    };
    let silicon = auth.is_silicon();
    // What ends, read first so the answer can name it; without that read it says so in general.
    let running = running_sessions(ctx).await;
    let names = if !silicon && running.as_ref().is_some_and(|r| !r.is_empty()) {
        device_names(ctx).await
    } else {
        BTreeMap::new()
    };
    // The sign-in may have been refreshed (or ended) by those reads.
    let mut via = "extend";
    let mut revoked = false;
    let mut problem: Option<String> = None;
    if let Some(current) = ctx.auth.clone() {
        let refresh = current.refresh_token.clone();
        let r = ctx
            .call("POST /api/v2/auth/logout", |c, t| {
                let refresh = refresh.clone();
                async move { c.authed(&t).sign_out(Some(&refresh)).await }
            })
            .await;
        match r {
            Ok(()) => revoked = true,
            Err(e) => {
                // Extend couldn't do it (unreachable, or refused the token): revoke at Silicon
                // Accounts directly, so the sign-in still ends. Silicon Accounts tells Extend.
                via = "silicon-accounts";
                let saved = ctx.auth.clone().unwrap_or(current);
                let t = Instant::now();
                let label = format!("POST {}/v1/oauth/revoke", saved.accounts_url);
                match sign_in_at(&saved.accounts_url) {
                    Ok(si) => match ctx.timed(&label, t, si.revoke(&saved.refresh_token).await) {
                        Ok(r) => revoked = r.revoked,
                        Err(e2) => problem = Some(format!("{} Then Silicon Accounts: {}", e.message, e2.message)),
                    },
                    Err(e2) => problem = Some(e2.message),
                }
            }
        }
    } else {
        // A refresh found the sign-in already over.
        revoked = true;
        via = "already_ended";
    }
    signin::remove();
    store::forget_sessions(&auth.uuid);
    let cleaned = store::remove_legacy_state();
    ctx.auth = None;
    if let Some(p) = &problem {
        ctx.out.warn(&format!(
            "The sign-in couldn't be revoked: {p} It was deleted from this machine. To end it everywhere now, remove Extend in Silicon Accounts: silicon-accounts apps remove extend"
        ));
    }
    let ended: Option<Vec<String>> = running
        .as_ref()
        .map(|r| r.iter().map(|s| s.session_id.to_string()).collect());
    let mut data = account_json(&auth);
    data.insert("signed_out".into(), json!(true));
    data.insert("authenticated".into(), json!(false));
    data.insert("revoked".into(), json!(revoked));
    data.insert("via".into(), json!(via));
    data.insert("ended_sessions".into(), json!(ended));
    if !cleaned.is_empty() {
        data.insert("removed_extend_3_state".into(), json!(cleaned));
    }
    ctx.emit(Value::Object(data), || {
        let who = &auth.id;
        let ends = match (&running, silicon) {
            (None, true) => " Your running sessions have ended.".to_owned(),
            (None, false) => " The running sessions of the Silicons you gave access to have ended.".to_owned(),
            (Some(r), true) if r.is_empty() => String::new(),
            (Some(r), true) => format!(
                " Ended session{} {}.",
                if r.len() == 1 { "" } else { "s" },
                and_list(&r.iter().map(|s| s.session_id.to_string()).collect::<Vec<_>>())
            ),
            (Some(r), false) if r.is_empty() => " No Silicon you gave access to was using a device.".to_owned(),
            (Some(r), false) => format!(
                " Ended the sessions of the Silicons you gave access to: {}.",
                r.iter()
                    .map(|s| format!(
                        "{} ({} on {})",
                        s.silicon_id,
                        s.session_id,
                        names
                            .get(&s.device_id.to_string())
                            .cloned()
                            .unwrap_or_else(|| s.device_id.to_string())
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        format!(
            "Signed out {who}.{ends}{}",
            if revoked || problem.is_some() {
                ""
            } else {
                " (Silicon Accounts had already ended this sign-in.)"
            }
        )
    });
    Ok(0)
}

/// `extend logout` with no usable sign-in: an Extend 3 or unreadable file is deleted; one for
/// another Extend is revoked where it was made.
async fn logout_unused(ctx: &mut Ctx) -> R<i32> {
    match ctx.unusable.take() {
        None => {
            ctx.emit(
                json!({"signed_out": false, "authenticated": false, "reason": "No saved sign-in."}),
                || "Not signed in; nothing to sign out.".into(),
            );
        }
        Some(Unusable::OtherOrigin(s)) => {
            let revoked = match sign_in_at(&s.accounts_url) {
                Ok(si) => si.revoke(&s.refresh_token).await.map(|r| r.revoked).unwrap_or(false),
                Err(_) => false,
            };
            signin::remove();
            store::forget_sessions(&s.uuid);
            let mut data = account_json(&s);
            data.insert("signed_out".into(), json!(true));
            data.insert("authenticated".into(), json!(false));
            data.insert("revoked".into(), json!(revoked));
            data.insert("via".into(), json!("silicon-accounts"));
            data.insert("api_url".into(), json!(s.api_url));
            ctx.emit(Value::Object(data), || {
                format!(
                    "Signed out {} (its sign-in was for the Extend at {}){}.",
                    s.id,
                    s.api_url,
                    if revoked {
                        ""
                    } else {
                        "; Silicon Accounts didn't revoke it (it had ended, or couldn't be reached)"
                    }
                )
            });
        }
        Some(Unusable::Legacy { id }) => {
            signin::remove();
            let cleaned = store::remove_legacy_state();
            ctx.emit(
                json!({"signed_out": true, "authenticated": false, "revoked": false, "extend_3": true, "id": id, "removed_extend_3_state": cleaned}),
                || "Removed the Extend 3 sign-in (Silicon IAM), which Extend 4 no longer uses.".into(),
            );
        }
        Some(Unusable::Unreadable(why)) => {
            signin::remove();
            ctx.emit(
                json!({"signed_out": true, "authenticated": false, "revoked": false, "unreadable": why}),
                || format!("Deleted the saved sign-in, which couldn't be read ({why})."),
            );
        }
    }
    Ok(0)
}

/// The sessions still running (active or paused) that signing out ends: a Silicon's own; a
/// Carbon's, on their devices. `None` when they couldn't be read.
async fn running_sessions(ctx: &mut Ctx) -> Option<Vec<Session>> {
    let page = ctx
        .call("GET /api/v2/sessions", |c, t| async move {
            c.authed(&t)
                .sessions(ListQuery {
                    limit: Some(100),
                    ..Default::default()
                })
                .await
        })
        .await
        .ok()?;
    Some(
        page.items
            .into_iter()
            .filter(|s| s.state != SessionState::Ended)
            .collect(),
    )
}

/// The names of the Carbon's devices, by id, for messages (one page; empty when it can't be read).
async fn device_names(ctx: &mut Ctx) -> BTreeMap<String, String> {
    let q = DeviceQuery {
        limit: Some(100),
        ..Default::default()
    };
    ctx.call("GET /api/v2/devices", |c, t| {
        let q = q.clone();
        async move { c.authed(&t).devices(q).await }
    })
    .await
    .map(|p| p.items.into_iter().map(|d| (d.device_id.to_string(), d.name)).collect())
    .unwrap_or_default()
}

/// `extend accounts [--json]` (and the hidden `extend iam --json`): where Extend's accounts come
/// from and how to sign in, read from this CLI's own settings with no network and no sign-in, so it
/// answers anywhere and always exits 0.
fn accounts_info(ctx: &Ctx, args: &[String], path: &str) -> R<i32> {
    Args::parse(args, path)?.at_most(0)?;
    let data = json!({
        "app_id": extend_protocol::APP_ID,
        "accounts_url": ctx.accounts_url(),
        "api_url": ctx.api_url(),
        "version": env!("CARGO_PKG_VERSION"),
        "client_id": extend_protocol::APP_ID,
        "device_flow": true,
        "public_client": true,
        "sign_in": {"carbon": CARBON_SIGN_IN, "silicon": SILICON_SIGN_IN},
        "status": "extend login status --json",
        "website_url": help::WEBSITE,
        "docs_url": help::DOCS,
        "repository_url": help::REPO,
        "package_url": help::CRATE,
        "install": help::INSTALL,
        "update": compat::UPDATE,
    });
    ctx.emit(data, || {
        format!(
            "app_id        {}\nSilicon Accounts  {}\nExtend API    {}\nversion       {}\nCarbons       {CARBON_SIGN_IN}\nSilicons      {SILICON_SIGN_IN}\nWebsite       {}\nDocs          {}",
            extend_protocol::APP_ID,
            ctx.accounts_url(),
            ctx.api_url(),
            env!("CARGO_PKG_VERSION"),
            help::WEBSITE,
            help::DOCS
        )
    });
    Ok(0)
}

/// Local versions, the negotiated API version, and what Extend's compatibility matrix says about
/// them. Exits 0 even when Extend can't be asked; `status` says `unknown` then.
async fn version(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    Args::parse(args, "version")?.at_most(0)?;
    let cli = env!("CARGO_PKG_VERSION");
    let (api, status, service) = match ctx.client().await {
        Ok(c) => {
            let t = Instant::now();
            match ctx.timed("GET /api/v2/contracts", t, c.contracts().await) {
                Ok(matrix) => (
                    Some(c.api_version()),
                    compat::evaluate(&matrix, c.api_version(), cli),
                    matrix["service_version"].as_str().map(str::to_owned),
                ),
                Err(e) => (
                    Some(c.api_version()),
                    compat::Status::unknown(format!(
                        "Extend didn't give its compatibility matrix: {} ({})",
                        e.message,
                        e.code.as_str()
                    )),
                    None,
                ),
            }
        }
        Err(e) if e.code == ErrorCode::ApiVersionSunset => (
            None,
            compat::Status {
                status: "sunset",
                message: format!("{} {}", e.message, e.hint.clone().unwrap_or_default())
                    .trim()
                    .to_owned(),
                ..compat::Status::unknown("")
            },
            None,
        ),
        Err(e) if e.code == ErrorCode::ApiVersionUnsupported => (
            None,
            compat::Status {
                status: "unsupported",
                message: format!("{} {}", e.message, e.hint.clone().unwrap_or_default())
                    .trim()
                    .to_owned(),
                ..compat::Status::unknown("")
            },
            None,
        ),
        Err(e) => (
            None,
            compat::Status::unknown(format!("Extend at {} couldn't be asked: {}", ctx.api_url(), e.message)),
            None,
        ),
    };
    let mut data = json!({
        "cli": cli, "client_crate": cli, "api_version": api, "api_url": ctx.api_url(), "service_version": service,
    });
    if let (Some(d), Value::Object(s)) = (data.as_object_mut(), status.to_json()) {
        d.extend(s);
    }
    let paint = match status.status {
        "current" => "32",
        "deprecated" => "33",
        "sunset" | "unsupported" => "31",
        _ => "2",
    };
    let label = ctx.out.colors.paint(Stream::Out, paint, status.status);
    ctx.emit(data, || {
        format!(
            "extend {cli} (silicon-extend-client {cli}), API {} at {}{}\nStatus: {label}. {}",
            api.map_or("unreachable".to_owned(), |v| format!("v{v}")),
            ctx.api_url(),
            service
                .as_deref()
                .map(|s| format!(" (service {s})"))
                .unwrap_or_default(),
            status.message
        )
    });
    Ok(0)
}

// ───────────────────────────── The Silicons a Carbon looks after ─────────────────────────────

/// `extend silicon ls|show|renounce`: the Silicons a Carbon looks after (they are its custodian in
/// Silicon Accounts) and the ones they gave access to. A custodian sees what its Silicons do in
/// Extend and can stop it, never act as them.
async fn silicon(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("silicon {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("silicon", &sub));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "ls" => {
            a.at_most(0)?;
            let items = ctx
                .call(
                    "GET /api/v2/silicons",
                    |c, t| async move { c.authed(&t).silicons().await },
                )
                .await?;
            ctx.emit(json!({"items": items}), || {
                if items.is_empty() {
                    return "No Silicons yet: you look after none (in Silicon Accounts) and gave none access. Give one access with `extend device access grant <device_id> <si:id>`.".into();
                }
                let mut rows = vec![["SILICON", "UUID", "NAME", "YOU LOOK AFTER IT", "YOUR DEVICES", "GRANTS", "RUNNING"]
                    .map(String::from)
                    .to_vec()];
                rows.extend(items.iter().map(|s| {
                    vec![
                        s.id.clone(),
                        s.uuid.clone(),
                        s.display_name.clone().unwrap_or_default(),
                        if s.looked_after { "yes" } else { "no" }.into(),
                        s.granted_by_you.to_string(),
                        s.grants.map_or("—".into(), |n| n.to_string()),
                        s.running_sessions.map_or("—".into(), |n| n.to_string()),
                    ]
                }));
                let mut t = table(rows);
                if items.iter().any(|s| s.looked_after) {
                    t.push_str("\n\nFor a Silicon you look after: `extend silicon show <si:id>` lists every device it can use, `extend session ls --silicon <si:id>` its sessions, `extend file ls --silicon <si:id>` its files.");
                }
                t
            });
        }
        "show" => {
            a.at_most(1)?;
            let who = a.req(0, "Silicon (si:id or uuid)")?;
            let s = ctx
                .call(&format!("GET /api/v2/silicons/{who}"), |c, t| {
                    let who = who.clone();
                    async move { c.authed(&t).silicon(&who).await }
                })
                .await?;
            let grants = if s.looked_after {
                let who = who.clone();
                Some(
                    ctx.call(&format!("GET /api/v2/silicons/{who}/grants"), |c, t| {
                        let who = who.clone();
                        async move { c.authed(&t).silicon_grants(&who).await }
                    })
                    .await?,
                )
            } else {
                None
            };
            ctx.emit(json!({"silicon": s, "grants": grants}), || {
                let mut t = format!(
                    "{}{} (uuid {})\n  You look after it: {}\n  Devices of yours it can use: {}",
                    s.id,
                    s.display_name.as_deref().map(|n| format!(" — {n}")).unwrap_or_default(),
                    s.uuid,
                    if s.looked_after {
                        "yes (you are its custodian)"
                    } else {
                        "no"
                    },
                    s.granted_by_you
                );
                if let Some(n) = s.running_sessions {
                    t.push_str(&format!("\n  Running sessions: {n}"));
                }
                if let Some(g) = &grants {
                    if g.is_empty() {
                        t.push_str("\n\nIt can use no device yet.");
                    } else {
                        t.push_str("\n\nEvery device it can use:\n");
                        let mut rows = vec![
                            ["DEVICE", "NAME", "OS", "PAIRED BY", "GRANTED", "LAST USED"]
                                .map(String::from)
                                .to_vec(),
                        ];
                        rows.extend(g.iter().map(|x| {
                            vec![
                                x.device_id.to_string(),
                                x.device_name.clone().unwrap_or_default(),
                                x.device_os.map(|o| o.as_str().to_owned()).unwrap_or_default(),
                                x.owner
                                    .as_ref()
                                    .map(|o| o.id.clone())
                                    .unwrap_or_else(|| x.granted_by.clone()),
                                fmt_time(&x.granted_at),
                                x.last_used_at.map(|t| fmt_time(&t)).unwrap_or_else(|| "—".into()),
                            ]
                        }));
                        t.push_str(&table(rows));
                        t.push_str(&format!(
                            "\n\nTake one away: extend silicon renounce {} <device_id>",
                            s.id
                        ));
                    }
                }
                t
            });
        }
        _ => {
            a.at_most(2)?;
            let who = a.req(0, "Silicon (si:id or uuid)")?;
            let device = a.req(1, "device id")?;
            parse_device_id(&device)?;
            ctx.call(&format!("DELETE /api/v2/silicons/{who}/grants/{device}"), |c, t| {
                let (who, device) = (who.clone(), device.clone());
                async move { c.authed(&t).renounce(&who, &device).await }
            })
            .await?;
            ctx.emit(json!({"silicon": who, "device_id": device, "renounced": true}), || {
                format!("{who} can no longer use {device}; its session there, if any, has ended. The device's Carbon sees it in the device's activity.")
            });
        }
    }
    Ok(0)
}

// ───────────────────────────── Settings ─────────────────────────────

fn unknown_setting(k: &str) -> CliError {
    if k == "team" {
        return CliError::usage(
            "the team setting was removed in Extend 4: there are no Teams",
            "Nothing replaces it: `extend device ls` lists every device you can use or paired. `extend config unset team` clears an old value.",
        );
    }
    CliError::usage(
        format!("unknown setting {k:?}"),
        format!(
            "Settings: {}. `extend config ls` shows each with its values and default.",
            store::SETTINGS.iter().map(|s| s.key).collect::<Vec<_>>().join(", ")
        ),
    )
}

/// Checks a setting's value against what it takes, and returns it as saved.
fn validate_setting(key: &str, v: &str) -> R<String> {
    let bad = |why: &str, hint: &str| CliError::usage(format!("{key} can't be {v:?}: {why}"), hint.to_owned());
    let one_of = |choices: &[&str]| -> R<String> {
        if choices.contains(&v) {
            Ok(v.to_owned())
        } else {
            Err(bad(
                &format!("it is one of {}", choices.join(", ")),
                &format!("Example: extend config set {key} {}", choices[0]),
            ))
        }
    };
    match key {
        "api_url" | "accounts_url" => accounts::check_url(v, if key == "api_url" { "The Extend service URL" } else { "The Silicon Accounts URL" })
            .map_err(|why| {
                bad(
                    &why,
                    if key == "api_url" {
                        "Example: extend config set api_url https://backend.extend.teamofsilicons.com (http only for localhost, 127.0.0.1 or [::1])"
                    } else {
                        "Example: extend config set accounts_url https://accounts.teamofsilicons.com (http only for localhost, 127.0.0.1 or [::1])"
                    },
                )
            }),
        "telemetry" => one_of(&["on", "off"]),
        "output" => one_of(&["text", "json"]),
        "color" => one_of(&["auto", "always", "never"]),
        "screenshot_scale" => match v.parse::<f64>() {
            Ok(x) if x.is_finite() && (0.01..=1.0).contains(&x) => Ok(v.to_owned()),
            _ => Err(bad(
                "it is a number from 0.01 to 1 (the fraction of full size)",
                "Example: extend config set screenshot_scale 0.5",
            )),
        },
        "self_destruct" => {
            parse_ttl(v)?;
            Ok(v.to_owned())
        }
        "download_dir" => {
            let p = Path::new(v);
            if !p.is_dir() {
                return Err(bad(
                    "it is not an existing directory",
                    "Create it first (mkdir -p <dir>), then set it again.",
                ));
            }
            Ok(p.canonicalize()
                .map_or_else(|_| v.to_owned(), |p| p.display().to_string()))
        }
        _ => Err(unknown_setting(key)),
    }
}

async fn config(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("config {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("config", &sub));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "ls" => {
            a.at_most(0)?;
            let cfg = ctx.cfg.clone();
            ctx.emit(
                json!({
                    "settings": cfg,
                    "state_dir": store::root(),
                    "keys": store::SETTINGS.iter().map(|s| json!({"key": s.key, "about": s.about, "default": s.default, "value": cfg.get(s.key)})).collect::<Vec<_>>(),
                }),
                || {
                    let mut s = format!("State: {}\n", store::root().display());
                    for st in store::SETTINGS {
                        let value = cfg
                            .get(st.key)
                            .cloned()
                            .unwrap_or_else(|| format!("(default: {})", st.default));
                        s.push_str(&format!("{:<18} {value:<36} {}\n", st.key, st.about));
                    }
                    s
                },
            );
        }
        "get" => {
            a.at_most(1)?;
            let k = a.req(0, "setting")?;
            let st = store::setting(&k).ok_or_else(|| unknown_setting(&k))?;
            let v = ctx.cfg.get(&k).cloned();
            let effective = v.clone().unwrap_or_else(|| st.default.to_owned());
            ctx.emit(
                json!({"key": k, "value": v, "default": st.default, "effective": effective}),
                || match &v {
                    Some(v) => v.clone(),
                    None => format!("{} (default)", st.default),
                },
            );
        }
        "set" => {
            a.at_most(2)?;
            let k = a.req(0, "setting")?;
            let v = a.req(1, "value")?;
            let v = validate_setting(&k, &v)?;
            ctx.cfg.insert(k.clone(), v.clone());
            store::save_config(&ctx.cfg)?;
            ctx.emit(json!({"key": k, "value": v}), || format!("{k} = {v}"));
        }
        "unset" => {
            a.at_most(1)?;
            let k = a.req(0, "setting")?;
            if k == "team" && ctx.cfg.remove("team").is_some() {
                store::save_config(&ctx.cfg)?;
                ctx.emit(json!({"key": k, "value": null}), || {
                    "Cleared the old team setting (Extend 4 has no Teams).".into()
                });
                return Ok(0);
            }
            let st = store::setting(&k).ok_or_else(|| unknown_setting(&k))?;
            ctx.cfg.remove(&k);
            store::save_config(&ctx.cfg)?;
            ctx.emit(json!({"key": k, "value": null, "default": st.default}), || {
                format!("{k} is back to its default ({}).", st.default)
            });
        }
        _ => {
            a.at_most(1)?;
            config_home(ctx, &a)?;
        }
    }
    Ok(0)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `extend config home <dir>`: moves the state to `<dir>/.extend`, so the login and settings come
/// along; refuses when `<dir>` already holds state, unless `--use-existing` switches to it.
fn config_home(ctx: &mut Ctx, a: &Args) -> R<()> {
    let d = a.req(0, "directory")?;
    let new_root = store::root_for_home(Path::new(&d)).map_err(|_| {
        CliError::usage(
            format!("not a directory: {d}"),
            "Give an existing directory (create it first with `mkdir -p <dir>`); Extend keeps its state in <dir>/.extend.",
        )
    })?;
    let old_root = store::root();
    if same_dir(&old_root, &new_root) {
        ctx.emit(json!({"state_dir": new_root, "moved": []}), || {
            format!("Extend state already lives in {}.", new_root.display())
        });
        return Ok(());
    }
    let here = store::state_in(&old_root);
    let there = store::state_in(&new_root);
    if a.flag("--use-existing") {
        store::point_to(&new_root)?;
        ctx.emit(
            json!({"state_dir": new_root, "moved": [], "left_in": old_root, "left": here}),
            || {
                let mut s = format!(
                    "Extend state now lives in {}, using what was already there ({}).",
                    new_root.display(),
                    if there.is_empty() {
                        "nothing yet".to_owned()
                    } else {
                        there.join(", ")
                    }
                );
                if !here.is_empty() {
                    s.push_str(&format!(
                        " The state in {} ({}) stays there; `extend config home {}` switches back.",
                        old_root.display(),
                        here.join(", "),
                        old_root
                            .parent()
                            .map_or_else(|| old_root.display().to_string(), |p| p.display().to_string())
                    ));
                }
                s
            },
        );
        return Ok(());
    }
    if !there.is_empty() {
        return Err(CliError::usage(
            format!(
                "{} already holds Extend state ({}); moving this CLI's state there would overwrite it.",
                new_root.display(),
                there.join(", ")
            ),
            format!(
                "To use the state already there and leave this one in {}, run `extend config home {d} --use-existing`. To move this state there instead, remove {} first.",
                old_root.display(),
                new_root.display()
            ),
        ));
    }
    let login = ctx.auth.as_ref().map(|a| a.id.clone());
    let settings = ctx.cfg.len();
    let copied = store::copy_state(&old_root, &new_root)?;
    if let Err(e) = store::point_to(&new_root) {
        let _ = store::remove_state(&new_root, &copied);
        return Err(e.into());
    }
    let left_behind = store::remove_state(&old_root, &copied).err();
    let mut moved = Vec::new();
    if let Some(m) = &login {
        moved.push(format!("the sign-in of {m}"));
    }
    if settings > 0 {
        moved.push(format!("{settings} setting(s)"));
    }
    if copied.contains(&"sessions") {
        moved.push("sessions".into());
    }
    ctx.emit(
        json!({"state_dir": new_root, "moved_from": old_root, "moved": copied, "login": login}),
        || {
            let mut s = if moved.is_empty() {
                format!(
                    "Extend state now lives in {} (there was nothing to move).",
                    new_root.display()
                )
            } else {
                format!(
                    "Moved {} from {} to {}. Extend state now lives there.",
                    moved.join(", "),
                    old_root.display(),
                    new_root.display()
                )
            };
            if let Some(e) = &left_behind {
                s.push_str(&format!(
                    " The old copy in {} could not be deleted ({e:#}); delete it yourself.",
                    old_root.display()
                ));
            }
            s
        },
    );
    Ok(())
}

// ───────────────────────────── Devices ─────────────────────────────

/// What a Carbon who paired a computer that other Carbons paired too should know. The terminal
/// runs as the computer's own account, whichever Carbon gave access; only the Silicons of the Carbon
/// who installed Extend on it get it (Carbon decision, 2026-09-27).
const SHARED_COMPUTER_NOTE: &str = "Other Carbons paired this computer too. Only Silicons given access by the Carbon who installed Silicon Extend on it can use its terminal, which runs as the computer's own account: those Silicons can reach what that account can, including the other Carbons' pairs. Share a computer only with Carbons you trust.";

/// How a device's kind reads in a sentence ("Extend can't tell when this iPhone wakes").
fn os_noun(os: DeviceOs) -> &'static str {
    match os {
        DeviceOs::Android => "Android device",
        DeviceOs::AndroidTv => "TV",
        DeviceOs::Macos => "Mac",
        DeviceOs::Windows => "Windows computer",
        DeviceOs::Linux => "Linux computer",
        DeviceOs::Ios => "iPhone",
        DeviceOs::Ipados => "iPad",
        DeviceOs::Tvos => "Apple TV",
        DeviceOs::SamsungTv => "Samsung TV",
        DeviceOs::LgTv => "LG TV",
    }
}

/// "offline; last seen asleep".
fn last_seen(s: SleepState) -> &'static str {
    match s {
        SleepState::ScreenOff => "with its screen off",
        SleepState::Locked => "locked",
        SleepState::Asleep => "asleep",
        SleepState::Standby => "in standby",
        SleepState::OtherSession => "on another account",
        SleepState::Other => "not awake",
    }
}

/// The AWAKE column: yes, no (why), or — when Extend can't tell.
fn awake_cell(d: &Device) -> String {
    if d.removed_at.is_some() {
        return "—".into();
    }
    match d.awake {
        Some(true) => "yes".into(),
        Some(false) => match d.sleep_state {
            Some(st) => format!("no ({})", st.label()),
            None => "no".into(),
        },
        None => match d.last_sleep_state.filter(|_| !d.online) {
            Some(s) => format!("— (offline; last seen {})", last_seen(s)),
            None => "—".into(),
        },
    }
}

/// The IN USE column, as far as the viewer may know it: a Silicon only learns who is using a
/// device when it is on its own side (the same Carbon's pair, and the same custodian), and a Carbon
/// only for the Silicons they gave access to.
fn in_use_cell(d: &Device, silicon: bool, me: Option<&str>) -> String {
    if let Some(u) = &d.in_use {
        if Some(u.silicon_id.as_str()) == me {
            return format!("you ({})", u.session_id);
        }
        if silicon {
            return u.silicon_id.clone();
        }
        return format!("{} ({})", u.silicon_id, ago(&u.since));
    }
    if d.in_use_by_other_carried && !silicon {
        return "a carried device (stop it at the computer)".into();
    }
    if d.in_use_by_other {
        return if silicon {
            "in use".into()
        } else {
            "yes (another Carbon's Silicon)".into()
        };
    }
    "—".into()
}

fn device_line(d: &Device, silicon: bool, removed_column: bool, me: Option<&str>) -> Vec<String> {
    let mut name = d.name.clone();
    let mut in_use = in_use_cell(d, silicon, me);
    if silicon {
        // What the Silicon should know besides who uses it: it has asked to wake it, and another
        // Carbon's pair of the same physical device is also its to use (one Silicon at a time).
        let mut notes = Vec::new();
        if d.open_wake_requests.unwrap_or(0) > 0 {
            notes.push("asked".to_owned());
        }
        if let Some(same) = d.same_device.as_ref().filter(|s| !s.is_empty()) {
            let ids: Vec<String> = same.iter().map(ToString::to_string).collect();
            notes.push(format!("(same device as {})", ids.join(", ")));
        }
        if !notes.is_empty() {
            in_use = if in_use == "—" {
                notes.join(" ")
            } else {
                format!("{in_use}, {}", notes.join(" "))
            };
        }
    } else if d.paired_by_others == Some(true) {
        name.push_str(" (shared)");
    }
    let mut row = vec![
        d.device_id.to_string(),
        name,
        d.os.as_str().to_owned(),
        if d.removed_at.is_some() {
            "removed".into()
        } else if d.online {
            "yes".into()
        } else {
            "no".into()
        },
        awake_cell(d),
        in_use,
    ];
    if !silicon {
        row.push(d.access_count.map_or("—".into(), |n| n.to_string()));
        row.push(d.last_used_at.map_or("—".into(), |t| format!("{} ago", ago(&t))));
        row.push(d.days_left.map_or("—".into(), |x| x.to_string()));
        if removed_column {
            row.push(d.removed_at.map_or("—".into(), |t| {
                format!(
                    "{} ({})",
                    fmt_time(&t),
                    d.removed_reason.map_or("unknown", EndReason::as_str)
                )
            }));
        }
    }
    row
}

fn device_table(items: &[Device], silicon: bool, removed: bool, me: Option<&str>) -> String {
    let head: &[&str] = if silicon {
        &["ID", "NAME", "OS", "ONLINE", "AWAKE", "IN USE"]
    } else {
        &[
            "ID",
            "NAME",
            "OS",
            "ONLINE",
            "AWAKE",
            "IN USE",
            "ACCESS",
            "LAST USED",
            "DAYS LEFT",
        ]
    };
    let mut head: Vec<String> = head.iter().map(|h| (*h).to_owned()).collect();
    if removed && !silicon {
        head.push("REMOVED".into());
    }
    let mut rows = vec![head];
    rows.extend(items.iter().map(|d| device_line(d, silicon, removed, me)));
    table(rows)
}

fn parse_device_id(s: &str) -> R<DeviceId> {
    s.parse().map_err(|_| {
        CliError::usage(
            format!("{s:?} is not a device id; device ids are 8 lowercase hexadecimal characters, like 7c1e09ab."),
            "List devices with `extend device ls`.",
        )
    })
}

/// Pages `extend device ls` reads at most (100 devices each).
const DEVICE_PAGES: usize = 100;

/// Every device the query matches, following `next_cursor`. The second value is the cursor where
/// it stopped, when it had to stop early.
async fn all_devices(ctx: &mut Ctx, mut q: DeviceQuery, removed: bool) -> R<(Vec<Device>, Option<String>)> {
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..DEVICE_PAGES {
        let page = ctx
            .call("GET /api/v2/devices", |c, t| {
                let q = q.clone();
                async move {
                    let a = c.authed(&t);
                    if removed {
                        a.devices_including_removed(q).await
                    } else {
                        a.devices(q).await
                    }
                }
            })
            .await?;
        items.extend(page.items);
        match page.next_cursor {
            None => return Ok((items, None)),
            // A cursor seen before would read the same pages again.
            Some(c) if !seen.insert(c.clone()) => return Ok((items, Some(c))),
            Some(c) => q.cursor = Some(c),
        }
    }
    Ok((items, q.cursor))
}

/// A device's name for messages, read once more after the call that needed it; the id when that
/// read fails.
async fn device_name(ctx: &mut Ctx, id: &str) -> String {
    ctx.call(&format!("GET /api/v2/devices/{id}"), |c, t| {
        let id = id.to_owned();
        async move { c.authed(&t).device(&id).await }
    })
    .await
    .map_or_else(|_| id.to_owned(), |d| d.name)
}

/// Says which of Extend's notification types Ting doesn't know yet, and what that means.
fn missing_types_text(missing: &[String]) -> String {
    let listed: Vec<String> = missing
        .iter()
        .map(|name| match extend_protocol::ting::find(name) {
            Some(ty) => format!("  {name}: {}", ty.description),
            None => format!("  {name}"),
        })
        .collect();
    format!(
        "Ting doesn't know these Extend notification types yet, so those notifications can't arrive:\n{}\nThey are registered in Ting for the app extend, once, by whoever runs Extend; tell them. Requests and wake requests still show on the website, with `extend request ls` and `extend device wake-requests ls`, and on the device.",
        listed.join("\n")
    )
}

async fn device(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("device {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("device", &sub));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "ls" => {
            a.at_most(0)?;
            let removed = a.flag("--removed");
            let online = a.flag("--online");
            let q = DeviceQuery {
                scope: None,
                online: online.then_some(true),
                os: a.value("--os"),
                limit: Some(100),
                cursor: None,
            };
            let (mut items, stopped_at) = all_devices(ctx, q, removed).await?;
            if online {
                // The filter is the service's; this keeps an older service's pages honest too.
                items.retain(|d| d.online);
            }
            let (silicon, me) = (ctx.is_silicon(), ctx.member_id());
            ctx.emit(json!({"items": items, "next_cursor": stopped_at}), || {
                if items.is_empty() {
                    return if online || a.value("--os").is_some() {
                        "No devices match. Drop --online or --os to see all of them.".into()
                    } else if silicon {
                        "No devices you can use yet. A Carbon gives you access to a device they paired (`extend device access grant <device_id> <your si:id>`); your custodian sees your id with `extend silicon ls`.".into()
                    } else {
                        "No devices. Pair one at extend.teamofsilicons.com or with `extend device pair <code> --name <name>`.".into()
                    };
                }
                let mut s = device_table(&items, silicon, removed, me.as_deref());
                if let Some(c) = &stopped_at {
                    s.push_str(&format!(
                        "\n\nThis list is incomplete: it shows the first {} devices, and Extend has more (the next page starts after {c}). Narrow it with --online or --os.",
                        items.len()
                    ));
                }
                s
            });
        }
        "show" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            parse_device_id(&id)?;
            let d = ctx
                .call(&format!("GET /api/v2/devices/{id}"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).device(&id).await }
                })
                .await?;
            if ctx.out.json {
                ctx.emit(to_json(&d), String::new);
                return Ok(0);
            }
            let silicon = ctx.is_silicon();
            let me = ctx.member_id();
            let owner = !silicon && me.as_deref() == Some(d.owner.id.as_str());
            // The owner's view adds who has access and why setup isn't done; both are extra
            // reads, so a failure only leaves that part out.
            let (mut grants, mut setup) = (None, None);
            if owner && d.removed_at.is_none() {
                grants = read_access(ctx, &id).await.ok();
                if d.state == DeviceState::Setup {
                    setup = read_setup(ctx, &id).await.ok();
                }
            }
            let view = ShowView {
                owner,
                silicon,
                me,
                grants,
                setup,
                extend: ctx.suggest("").trim_end().to_owned(),
                colors: ctx.out.colors,
            };
            ctx.emit(Value::Null, || device_text(&d, &view));
        }
        "pair" => {
            a.at_most(1)?;
            let code = a.req(0, "pairing code")?;
            let name = a.value("--name").ok_or_else(|| {
                CliError::usage(
                    "--name is required",
                    "Name the device: extend device pair <pairing_code> --name \"Saket's Pixel\"",
                )
            })?;
            let ttl = a
                .value("--ttl-days")
                .map(|v| {
                    v.parse::<i32>().map_err(|_| {
                        CliError::usage(
                            format!("--ttl-days takes a number of days, 1–30, got {v:?}"),
                            "Example: --ttl-days 14 (the default).",
                        )
                    })
                })
                .transpose()?;
            let claim = PairingClaim {
                pairing_code: code,
                name,
                visibility: None,
                pair_ttl_days: ttl,
                silicon_ids: a.values("--access"),
            };
            let d = ctx
                .call("POST /api/v2/pairings", |c, t| {
                    let claim = claim.clone();
                    async move { c.authed(&t).pair(&claim).await }
                })
                .await?;
            ctx.emit(to_json(&d), || {
                let mut s = format!("Paired {} \"{}\" ({}).", d.device_id, d.name, d.os.as_str());
                if d.paired_by_others == Some(true) {
                    s.push_str(
                        " This device is also paired by another Carbon; your pair is separate (own name, access and lifetime).",
                    );
                    if d.os.kind() == DeviceKind::Computer {
                        s.push(' ');
                        s.push_str(SHARED_COMPUTER_NOTE);
                    }
                }
                s.push_str(&format!(
                    " Next: finish the device's own setup — watch it with `extend device setup {} --watch`.",
                    d.device_id
                ));
                s
            });
        }
        "attach" => {
            a.at_most(1)?;
            let host = a.req(0, "host device id")?;
            let os_raw = a.value("--os").ok_or_else(|| {
                CliError::usage(
                    "--os is required",
                    "Say what you're attaching: --os ios, ipados, tvos, samsung_tv or lg_tv.",
                )
            })?;
            let os: extend_protocol::DeviceOs = serde_json::from_value(json!(os_raw)).map_err(|_| {
                CliError::usage(
                    format!("unknown --os {os_raw:?}"),
                    "Devices attach through a computer: --os ios, ipados, tvos, samsung_tv or lg_tv.",
                )
            })?;
            let name = a
                .value("--name")
                .ok_or_else(|| CliError::usage("--name is required", "Name the device: --name \"Saket's iPhone\""))?;
            let input = AttachmentCreate {
                os,
                name,
                visibility: None,
                pair_ttl_days: None,
                address: a.value("--address"),
            };
            let d = ctx
                .call(&format!("POST /api/v2/devices/{host}/attachments"), |c, t| {
                    let (host, input) = (host.clone(), input.clone());
                    async move { c.authed(&t).attach(&host, &input).await }
                })
                .await?;
            ctx.emit(to_json(&d), || {
                format!(
                    "Created {} \"{}\" through {host}. Follow setup with `extend device setup {} --watch`.",
                    d.device_id, d.name, d.device_id
                )
            });
        }
        "setup" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            if a.flag("--retry") {
                return setup_retry(ctx, &id, a.value("--step")).await;
            }
            if a.value("--step").is_some() {
                return Err(CliError::usage(
                    "--step names the step to run again, so it goes with --retry",
                    format!("extend device setup {id} --retry --step <key>"),
                ));
            }
            loop {
                let s = read_setup(ctx, &id).await?;
                let done = s.state == SetupState::Complete;
                if !a.flag("--watch") || done || ctx.out.json {
                    let colors = ctx.out.colors;
                    ctx.emit(to_json(&s), || setup_text(&s, colors, &id));
                    break;
                }
                out!(
                    "\x1b[2J\x1b[H{}\n(watching; Ctrl-C to stop)\n",
                    setup_text(&s, ctx.out.colors, &id)
                );
                let _ = std::io::stdout().flush();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        "setup-code" => {
            a.at_most(2)?;
            let id = a.req(0, "device id")?;
            let code = a.req(1, "code")?;
            let s = ctx
                .call(&format!("POST /api/v2/devices/{id}/setup/code"), |c, t| {
                    let (id, code) = (id.clone(), code.clone());
                    async move { c.authed(&t).setup_code(&id, &code).await }
                })
                .await?;
            let colors = ctx.out.colors;
            ctx.emit(to_json(&s), || format!("Code sent.\n{}", setup_text(&s, colors, &id)));
        }
        "banner" => {
            a.at_most(2)?;
            let id = a.req(0, "device id")?;
            parse_device_id(&id)?;
            let value = a.req(1, "on or off")?;
            let indicator = match value.as_str() {
                "on" => InUseIndicator::Shown,
                "off" => InUseIndicator::Hidden,
                _ => {
                    return Err(CliError::usage(
                        "banner takes on or off",
                        format!("extend device banner {id} off"),
                    ));
                }
            };
            let d = ctx
                .call(&format!("PATCH /api/v2/devices/{id}"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).set_in_use_indicator(&id, indicator).await }
                })
                .await?;
            ctx.emit(to_json(&d), || {
                format!(
                    "Saved in-use banner {} for {}. This applies to every Carbon's pair of this device. The device app, or the computer it pairs through, needs Silicon Extend 1.1 or later; offline devices apply it when they reconnect.",
                    d.in_use_indicator.on_off(),
                    d.name,
                )
            });
        }
        "rename" | "ttl" => {
            a.at_most(2)?;
            let id = a.req(0, "device id")?;
            let v = a.req(1, if sub == "rename" { "name" } else { "value" })?;
            let patch = if sub == "rename" {
                DevicePatch {
                    name: Some(v),
                    ..Default::default()
                }
            } else {
                DevicePatch {
                    pair_ttl_days: Some(v.parse().map_err(|_| {
                        CliError::usage(
                            format!("ttl takes a number of days, 1–30, got {v:?}"),
                            format!("extend device ttl {id} 14"),
                        )
                    })?),
                    ..Default::default()
                }
            };
            let d = ctx
                .call(&format!("PATCH /api/v2/devices/{id}"), |c, t| {
                    let (id, patch) = (id.clone(), patch.clone());
                    async move { c.authed(&t).update_device(&id, None, &patch).await }
                })
                .await?;
            ctx.emit(to_json(&d), || match sub.as_str() {
                "ttl" => format!(
                    "{} stays paired until {} unless used ({} days without activity).",
                    d.name,
                    d.pair_expires_at.map(|t| fmt_time(&t)).unwrap_or_default(),
                    d.pair_ttl_days.unwrap_or_default()
                ),
                _ => format!("Renamed to \"{}\".", d.name),
            });
        }
        "stop" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let outcome = ctx
                .call(&format!("POST /api/v2/devices/{id}/stop"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).stop(&id).await }
                })
                .await?;
            match outcome {
                StopOutcome::Session(s) => {
                    ctx.emit(to_json(&s), || {
                        format!(
                            "Stopped {} (session {}){}.",
                            s.silicon_id,
                            s.session_id,
                            s.device.as_ref().map(|d| format!(" on {}", d.name)).unwrap_or_default()
                        )
                    });
                }
                StopOutcome::Other(stopped) => {
                    let name = if ctx.out.json {
                        id.clone()
                    } else {
                        device_name(ctx, &id).await
                    };
                    ctx.emit(to_json(&stopped), || {
                        format!("Stopped the Silicon using {name} (another Carbon gave it access).")
                    });
                }
                _ => ctx.emit(json!({"device_id": id}), || format!("Stopped the Silicon using {id}.")),
            }
        }
        "rm" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let d = ctx
                .call(&format!("GET /api/v2/devices/{id}"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).device(&id).await }
                })
                .await?;
            if !a.flag("--yes") {
                let session = match (&d.in_use, d.in_use_by_other) {
                    (Some(u), _) => format!("{}'s session {}", u.silicon_id, u.session_id),
                    (None, true) => {
                        "the session of the Silicon using it through your pair, if it is one of yours".into()
                    }
                    _ => "no running session".into(),
                };
                let mut message = format!(
                    "This removes \"{}\" ({id}): it ends {session}, removes access for {} Silicon(s), and unpairs it.",
                    d.name,
                    d.access_count.unwrap_or(0)
                );
                if d.paired_by_others == Some(true) {
                    message.push_str(" Only your pair ends: the other Carbons who paired it keep theirs.");
                }
                return Err(CliError::new(ErrorCode::ConfirmationRequired, message)
                    .hint(format!("Run `extend device rm {id} --yes` to confirm.")));
            }
            let version = d.version;
            ctx.call(&format!("DELETE /api/v2/devices/{id}"), |c, t| {
                let id = id.clone();
                async move { c.authed(&t).remove_device(&id, version).await }
            })
            .await?;
            ctx.emit(json!({"removed": id}), || {
                format!(
                    "Removed {id} (\"{}\"). Its activity stays readable: extend device activity {id}",
                    d.name
                )
            });
        }
        "access" => device_access(ctx, &a).await?,
        "activity" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let limit = match a.value("--limit") {
                None => Some(50),
                Some(l) => Some(l.parse::<u32>().ok().filter(|n| (1..=100).contains(n)).ok_or_else(|| {
                    CliError::usage(
                        format!("--limit takes a number of entries, 1–100, got {l:?}"),
                        "Example: --limit 100. Use --since/--until to reach older entries.",
                    )
                })?),
            };
            let q = ActivityQuery {
                silicon_id: a.value("--silicon"),
                // `--session` is the global flag; here it filters.
                session_id: ctx.g.session.clone(),
                since: a.value("--since").map(|s| relative_time(&s)).transpose()?,
                until: a.value("--until").map(|s| relative_time(&s)).transpose()?,
                limit,
                cursor: None,
            };
            let page = ctx
                .call(&format!("GET /api/v2/devices/{id}/activity"), |c, t| {
                    let (id, q) = (id.clone(), q.clone());
                    async move { c.authed(&t).activity(&id, q).await }
                })
                .await?;
            ctx.emit(to_json(&page), || {
                let mut rows = vec![vec![
                    "TIME".into(),
                    "WHO".into(),
                    "SESSION".into(),
                    "ACTION".into(),
                    "OUTCOME".into(),
                    "FILES".into(),
                ]];
                rows.extend(page.items.iter().map(|e| {
                    let action = match (&e.command, &e.args) {
                        (Some(c), Some(a)) => format!("{c} {}", a.join(" ")),
                        (Some(c), None) => c.clone(),
                        _ => e.action.clone(),
                    };
                    vec![
                        fmt_time(&e.at),
                        e.actor.id.clone(),
                        e.session_id.as_ref().map_or("—".into(), ToString::to_string),
                        action,
                        e.outcome.clone().unwrap_or_default(),
                        e.files.len().to_string(),
                    ]
                }));
                table(rows)
            });
        }
        "wake" => device_wake(ctx, &a).await?,
        "wake-requests" => device_wake_requests(ctx, &a).await?,
        _ => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let page = ctx
                .call(&format!("GET /api/v2/devices/{id}/requests"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).device_requests(&id, ListQuery::default()).await }
                })
                .await?;
            let me = ctx.member_id();
            ctx.emit(to_json(&page), || requests_table(&page.items, me.as_deref()));
        }
    }
    Ok(0)
}

/// `extend device access ls|grant|revoke`.
async fn device_access(ctx: &mut Ctx, a: &Args) -> R<()> {
    let op = a.pos.first().cloned().unwrap_or_else(|| "ls".into());
    if !["ls", "grant", "revoke"].contains(&op.as_str()) {
        return Err(CliError::usage(
            format!("unknown `extend device access {op}`"),
            "Usage: extend device access ls <device_id> | grant <device_id> <si:id>... | revoke <device_id> <si:id>...",
        ));
    }
    let id = a.req(1, "device id")?;
    if op == "ls" {
        a.at_most(2)?;
        let list = read_access(ctx, &id).await?;
        ctx.emit(json!({"items": list}), || {
            if list.is_empty() {
                return format!("No Silicon has access yet. Grant it with `extend device access grant {id} <si:id>`.");
            }
            let mut rows = vec![vec![
                "SILICON".into(),
                "UUID".into(),
                "GRANTED".into(),
                "LAST USED".into(),
                "WAKE REQUESTS".into(),
            ]];
            rows.extend(list.iter().map(|g| {
                vec![
                    g.silicon_id.clone(),
                    g.silicon_uuid.clone().unwrap_or_else(|| "—".into()),
                    fmt_time(&g.granted_at),
                    g.last_used_at.map(|t| fmt_time(&t)).unwrap_or_else(|| "—".into()),
                    if g.wake_muted == Some(true) { "off" } else { "on" }.into(),
                ]
            }));
            table(rows)
        });
        return Ok(());
    }
    let silicons: Vec<String> = a.pos[2..].to_vec();
    if silicons.is_empty() {
        return Err(CliError::usage(
            "name at least one Silicon",
            format!(
                "extend device access {op} {id} si:chef (any Silicon, by its si: id or uuid; `extend silicon ls` lists the ones you look after or gave access to)"
            ),
        ));
    }
    if op == "grant" {
        let mut granted = Vec::new();
        for s in &silicons {
            let g = ctx
                .call(&format!("PUT /api/v2/devices/{id}/access/{s}"), |c, t| {
                    let (id, s) = (id.clone(), s.clone());
                    async move { c.authed(&t).grant(&id, &s).await }
                })
                .await?;
            granted.push(g);
        }
        // Wake requests and requests for devices in use reach the Carbon through Ting, which needs
        // Extend's notification types; say so when Ting reported some missing.
        let ting = ctx
            .call("GET /api/v2/ting-registration", |c, t| async move {
                c.authed(&t).ting_registration().await
            })
            .await
            .ok();
        let missing = ting
            .as_ref()
            .filter(|r| r.delivery_enabled != Some(false))
            .map(|r| r.missing_types.clone())
            .unwrap_or_default();
        if !missing.is_empty() {
            ctx.out.warn(&missing_types_text(&missing));
        }
        ctx.emit(
            json!({"device_id": id, "grant": silicons, "grants": granted, "ting_missing_types": missing}),
            || {
                format!(
                    "Granted {} access to {id}. {} can use it now (one Silicon at a time) and {} custodian can see it.",
                    and_list(&granted.iter().map(|g| g.silicon_id.clone()).collect::<Vec<_>>()),
                    if granted.len() == 1 { "It" } else { "They" },
                    if granted.len() == 1 { "its" } else { "their" }
                )
            },
        );
        return Ok(());
    }
    for silicon in &silicons {
        ctx.call(&format!("DELETE /api/v2/devices/{id}/access/{silicon}"), |c, t| {
            let (id, silicon) = (id.clone(), silicon.clone());
            async move { c.authed(&t).revoke(&id, &silicon).await }
        })
        .await?;
    }
    ctx.emit(json!({"device_id": id, "revoke": silicons}), || {
        format!(
            "Revoked access for {} on {id}; any running session of theirs there has ended.",
            and_list(&silicons)
        )
    });
    Ok(())
}

async fn read_access(ctx: &mut Ctx, id: &str) -> R<Vec<AccessGrant>> {
    ctx.call(&format!("GET /api/v2/devices/{id}/access"), |c, t| {
        let id = id.to_owned();
        async move { c.authed(&t).access(&id).await }
    })
    .await
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A Carbon's view of an Extend time: the time of day when it is today (UTC), else the date too.
fn short_time(t: &time::OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    if t.date() == time::OffsetDateTime::now_utc().date() {
        t.format(&time::macros::format_description!("[hour]:[minute]Z"))
            .unwrap_or_default()
    } else {
        fmt_time(&t)
    }
}

/// Everything `extend device show` prints besides the device.
struct ShowView {
    /// The viewer is the Carbon who paired this device (this pair).
    owner: bool,
    silicon: bool,
    me: Option<String>,
    grants: Option<Vec<AccessGrant>>,
    setup: Option<Setup>,
    /// `extend`, to start the commands it suggests.
    extend: String,
    colors: Colors,
}

fn awake_line(d: &Device) -> String {
    let since = d.awake_changed_at.map(|t| format!(" since {}", short_time(&t)));
    match d.awake {
        Some(true) => format!(
            "yes{}",
            since.map(|s| format!(" ({})", s.trim_start())).unwrap_or_default()
        ),
        Some(false) => {
            let why: Vec<String> = d
                .sleep_state
                .map(|st| st.label().to_owned())
                .into_iter()
                .chain(since.map(|x| x.trim_start().to_owned()))
                .collect();
            if why.is_empty() {
                "no".into()
            } else {
                format!("no ({})", why.join(" "))
            }
        }
        None if !d.online => awake_cell(d),
        None => "— (Extend can't tell)".into(),
    }
}

fn wake_request_line(w: &WakeRequest) -> String {
    let notice = match w.device_notice {
        DeviceNotice::Sent => "sent to the device".to_owned(),
        DeviceNotice::Shown => "the device showed it".to_owned(),
        DeviceNotice::NotShown => format!(
            "the device couldn't show it{}",
            w.device_notice_note
                .as_deref()
                .map(|n| format!(" ({})", n.trim_end_matches('.')))
                .unwrap_or_default()
        ),
        DeviceNotice::Offline => "the device was offline".to_owned(),
        DeviceNotice::Unsupported => "the device can't show it".to_owned(),
        DeviceNotice::Other => "unknown".to_owned(),
    };
    let ting = match w.ting {
        Some(t) => format!("; Ting: {t}"),
        None if w.ting_covered_by.is_some() => "; Ting: covered by an earlier one".to_owned(),
        None => String::new(),
    };
    format!(
        "{}, asked {}{}: \"{}\" — expires {}; {notice}{ting}",
        w.from,
        short_time(&w.last_asked_at),
        if w.asks > 1 {
            format!(" (ask {})", w.asks)
        } else {
            String::new()
        },
        w.reason,
        short_time(&w.expires_at)
    )
}

fn device_text(d: &Device, v: &ShowView) -> String {
    let id = &d.device_id;
    let x = &v.extend;
    let mut s = format!(
        "{} ({})\n  OS:        {}{}\n  Owner:     {}\n",
        d.name,
        id,
        d.os.as_str(),
        d.os_version.as_ref().map(|v| format!(" {v}")).unwrap_or_default(),
        d.owner.id,
    );
    if let Some(same) = d.same_device.as_ref().filter(|x| !x.is_empty()) {
        let ids: Vec<String> = same.iter().map(ToString::to_string).collect();
        s.push_str(&format!(
            "  Same device as {}: another Carbon's pair of it, also yours to use (one Silicon at a time on all of them)\n",
            and_list(&ids)
        ));
    }
    if let Some(at) = &d.removed_at {
        s.push_str(&format!(
            "  Removed:   {} — {}. Nothing works on it any more; its activity stays readable with `extend device activity {}`.\n",
            fmt_time(at),
            d.removed_reason.map_or("reason unknown", EndReason::explain),
            id
        ));
    } else {
        s.push_str(&format!(
            "  Online:    {}\n  Awake:     {}\n",
            if d.online { "yes" } else { "no" },
            awake_line(d)
        ));
        if d.wake_detectable == Some(false) {
            s.push_str(&format!(
                "  Waking:    Extend can't tell when this {} wakes; {}\n",
                os_noun(d.os),
                if v.owner {
                    format!("when you wake it, say so: extend device wake-requests answer {id} woken")
                } else {
                    "its Carbon says so when they answer a wake request".to_owned()
                }
            ));
        }
        let in_use = match &d.in_use {
            Some(u) => format!(
                "{} in session {} since {}{}",
                if Some(u.silicon_id.as_str()) == v.me.as_deref() {
                    "you".to_owned()
                } else {
                    u.silicon_id.clone()
                },
                u.session_id,
                fmt_time(&u.since),
                if u.paused { " (paused for takeover)" } else { "" }
            ),
            None if d.in_use_by_other_carried && !v.silicon => {
                "a device this computer carries is in use; stop it from the computer's Silicon Extend app".to_owned()
            }
            None if d.in_use_by_other && !v.silicon => {
                format!("yes, by a Silicon another Carbon gave access to. Stop it: extend device stop {id}")
            }
            None if d.in_use_by_other => {
                format!("yes. Ask for it: {x} request send {id} --reason \"...\"")
            }
            None => "no".to_owned(),
        };
        s.push_str(&format!("  In use:    {in_use}\n"));
        s.push_str(&format!("  Banner:    {}\n", d.in_use_indicator.on_off()));
        if let Some(days) = d.days_left {
            s.push_str(&format!(
                "  Pairing:   {days} day(s) left of {}\n",
                d.pair_ttl_days.unwrap_or(0)
            ));
        }
    }
    if let Some(h) = &d.host_device_id {
        s.push_str(&format!("  Through:   {h}\n"));
    }
    if v.owner {
        if d.paired_by_others == Some(true) {
            s.push_str("  Shared:    Also paired by another Carbon. Your pair is separate: its name, access, lifetime and activity are yours.\n");
        }
        if let Some(muted) = d.wake_muted {
            s.push_str(&format!(
                "  Wake requests: {}\n",
                if muted {
                    format!("off (turn them on: extend device wake-requests unmute {id})")
                } else {
                    "on".to_owned()
                }
            ));
        }
    }
    if let Some(grants) = &v.grants {
        if grants.is_empty() {
            s.push_str(&format!(
                "\nAccess: no Silicon yet. Grant it: extend device access grant {id} <si:id>\n"
            ));
        } else {
            let who: Vec<String> = grants
                .iter()
                .map(|g| {
                    if g.wake_muted == Some(true) {
                        format!("{} (wake requests off)", g.silicon_id)
                    } else {
                        g.silicon_id.clone()
                    }
                })
                .collect();
            s.push_str(&format!("\nAccess: {}\n", who.join(", ")));
        }
    }
    let open: Vec<&WakeRequest> = d
        .wake_requests
        .iter()
        .flatten()
        .filter(|w| w.state == WakeState::Open)
        .collect();
    if !open.is_empty() {
        if v.silicon {
            s.push_str("\nYour wake request:\n");
            for w in &open {
                s.push_str(&format!("  {}\n", wake_request_line(w)));
            }
            s.push_str(&format!("  Withdraw it: {x} device wake {id} --cancel\n"));
        } else {
            s.push_str("\nOpen wake requests:\n");
            for w in &open {
                s.push_str(&format!("  {}\n", wake_request_line(w)));
            }
            if v.owner {
                s.push_str(&format!(
                    "  Answer: extend device wake-requests answer {id} woken (it's awake) or declined\n"
                ));
            }
        }
    }
    if v.owner && d.paired_by_others == Some(true) && d.os.kind() == DeviceKind::Computer {
        s.push_str(&format!("\nShared computer: {SHARED_COMPUTER_NOTE}\n"));
    }
    if let Some(setup) = v.setup.as_ref().filter(|st| st.state != SetupState::Complete) {
        s.push('\n');
        s.push_str(&setup_text(setup, v.colors, id.as_str()));
    }
    if let Some(cmds) = d.commands.as_ref().filter(|c| !c.is_empty()) {
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

fn relative_time(s: &str) -> R<String> {
    if let Some(n) = s
        .strip_suffix('h')
        .or_else(|| s.strip_suffix('d'))
        .or_else(|| s.strip_suffix('m'))
        .and_then(|n| n.parse::<i64>().ok())
    {
        let unit = s.chars().last().unwrap_or('h');
        let d = match unit {
            'm' => time::Duration::minutes(n),
            'd' => time::Duration::days(n),
            _ => time::Duration::hours(n),
        };
        return Ok((time::OffsetDateTime::now_utc() - d)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default());
    }
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).map_err(|_| {
        CliError::usage(
            format!("{s:?} is not a time"),
            "Use RFC 3339 (2026-09-27T10:00:00Z) or a time ago: 30m, 2h, 3d.",
        )
    })?;
    Ok(s.to_owned())
}

async fn read_setup(ctx: &mut Ctx, id: &str) -> R<Setup> {
    ctx.call(&format!("GET /api/v2/devices/{id}/setup"), |c, t| {
        let id = id.to_owned();
        async move { c.authed(&t).setup(&id).await }
    })
    .await
}

fn setup_text(s: &Setup, colors: Colors, id: &str) -> String {
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
        let (mark, sgr) = match st.status {
            StepStatus::Done => ("✓", "32"),
            StepStatus::InProgress => ("…", "36"),
            StepStatus::NeedsCarbon => ("!", "33"),
            StepStatus::Failed => ("✗", "31"),
            StepStatus::Todo => (" ", "0"),
        };
        out.push_str(&format!("  {} {}", colors.paint(Stream::Out, sgr, mark), st.title));
        if st.status != StepStatus::Done {
            if let Some(h) = &st.help {
                out.push_str(&format!(" — {h}"));
            }
            if let Some(e) = &st.error {
                out.push_str(&format!(" ({e})"));
            }
        }
        out.push('\n');
        if st.status == StepStatus::Failed {
            out.push_str(&format!(
                "      Retry: extend device setup {id} --retry --step {}\n",
                st.key
            ));
        }
    }
    out
}

/// How long `--retry` waits for a retried step that still reads failed to show a new try, before
/// it stops following (the device may not have reported yet).
const RETRY_QUIET_S: u64 = 20;

/// `extend device setup <id> --retry [--step <key>]`: asks the device to run the failed steps
/// again, then follows them until each finishes or fails.
async fn setup_retry(ctx: &mut Ctx, id: &str, step: Option<String>) -> R<i32> {
    let before = read_setup(ctx, id).await?;
    let r = ctx
        .call(&format!("POST /api/v2/devices/{id}/setup/retry"), |c, t| {
            let (id, step) = (id.to_owned(), step.clone());
            async move { c.authed(&t).retry_setup(&id, step.as_deref()).await }
        })
        .await?;
    // With --json it follows quietly and prints one document once the steps finish or fail.
    let json = ctx.out.json;
    let name = if json {
        id.to_owned()
    } else {
        device_name(ctx, id).await
    };
    let title = |k: &str| before.step(k).map_or_else(|| k.to_owned(), |s| s.title.clone());
    let titles: Vec<String> = r.retrying.iter().map(|k| title(k)).collect();
    let head = format!("Retrying {} on {name}.", and_list(&titles));
    if !json {
        outln!("{head}");
        let _ = std::io::stdout().flush();
    }
    // A retried step can read failed until the device reports its new try, and fail again before
    // the next read: it counts as retried once it read anything else, or its error changed.
    let mut moved: std::collections::HashSet<String> = std::collections::HashSet::new();
    let started = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let s = read_setup(ctx, id).await?;
        for k in &r.retrying {
            if let Some(st) = s.step(k)
                && (st.status != StepStatus::Failed || st.error != before.step(k).and_then(|b| b.error.clone()))
            {
                moved.insert(k.clone());
            }
        }
        let quiet = started.elapsed() >= Duration::from_secs(RETRY_QUIET_S);
        let finished = r.retrying.iter().all(|k| match s.step(k) {
            None => true,
            Some(st) => {
                st.status == StepStatus::Done || (st.status == StepStatus::Failed && (moved.contains(k) || quiet))
            }
        });
        if !finished {
            if !json {
                out!(
                    "\x1b[2J\x1b[H{head}\n{}\n(following the retry; Ctrl-C to stop)\n",
                    setup_text(&s, ctx.out.colors, id)
                );
                let _ = std::io::stdout().flush();
            }
            continue;
        }
        if json {
            ctx.emit(
                json!({"device_id": id, "retrying": r.retrying, "setup": s}),
                String::new,
            );
            return Ok(0);
        }
        let mut lines = vec![setup_text(&s, ctx.out.colors, id).trim_end().to_owned()];
        let done: Vec<String> = r
            .retrying
            .iter()
            .filter(|k| s.step(k).is_none_or(|st| st.status == StepStatus::Done))
            .map(|k| title(k))
            .collect();
        if !done.is_empty() {
            lines.push(format!("Done: {}.", and_list(&done)));
        }
        for k in &r.retrying {
            let Some(st) = s.step(k).filter(|st| st.status == StepStatus::Failed) else {
                continue;
            };
            if moved.contains(k) {
                lines.push(format!(
                    "{} failed again: {}",
                    st.title,
                    st.error.as_deref().unwrap_or("the device gave no reason.")
                ));
            } else {
                lines.push(format!(
                    "{} hasn't reported a new try yet. Check again with `extend device setup {id} --watch`.",
                    st.title
                ));
            }
        }
        outln!("{}", lines.join("\n"));
        return Ok(0);
    }
}

/// `extend device wake <id> --reason "..."` and `--cancel`.
async fn device_wake(ctx: &mut Ctx, a: &Args) -> R<()> {
    a.at_most(1)?;
    let id = a.req(0, "device id")?;
    parse_device_id(&id)?;
    if a.flag("--cancel") {
        if a.value("--reason").is_some() {
            return Err(CliError::usage(
                "--cancel withdraws your wake request, so it takes no --reason",
                format!("{} to withdraw it.", ctx.suggest(&format!("device wake {id} --cancel"))),
            ));
        }
        let open = ctx
            .call(&format!("GET /api/v2/devices/{id}/wake-requests"), |c, t| {
                let id = id.clone();
                async move {
                    c.authed(&t)
                        .wake_requests(
                            &id,
                            ListQuery {
                                state: Some("open".into()),
                                limit: Some(50),
                                ..Default::default()
                            },
                        )
                        .await
                }
            })
            .await?;
        let me = ctx.member_id();
        let Some(w) = open
            .items
            .into_iter()
            .find(|w| w.state == WakeState::Open && me.as_deref().is_none_or(|m| w.from == m))
        else {
            return Err(CliError::new(
                ErrorCode::RequestNotFound,
                format!("You have no open request to wake {id}."),
            )
            .hint(format!(
                "Ask with `{}`.",
                ctx.suggest(&format!("device wake {id} --reason \"...\""))
            )));
        };
        let wake_id = w.wake_id;
        ctx.call(
            &format!("DELETE /api/v2/devices/{id}/wake-requests/{wake_id}"),
            |c, t| {
                let id = id.clone();
                async move { c.authed(&t).cancel_wake(&id, wake_id).await }
            },
        )
        .await?;
        ctx.emit(json!({"device_id": id, "cancelled": wake_id}), || {
            format!("Withdrew your request to wake {id}.")
        });
        return Ok(());
    }
    let reason = a.value("--reason").ok_or_else(|| {
        CliError::usage(
            "--reason is required (1–300 characters)",
            format!(
                "Say why you need it awake: {}",
                ctx.suggest(&format!(
                    "device wake {id} --reason \"Need the TV on to check the new menu\""
                ))
            ),
        )
    })?;
    let n = reason.trim().chars().count();
    if n == 0 || n > extend_protocol::REASON_MAX_CHARS {
        return Err(CliError::usage(
            format!("--reason must be 1–300 characters; it is {n}."),
            "Its Carbon reads it as written, on the device and in Ting; keep it to one or two sentences.",
        ));
    }
    let w = ctx
        .call(&format!("POST /api/v2/devices/{id}/wake-requests"), |c, t| {
            let (id, reason) = (id.clone(), reason.clone());
            async move { c.authed(&t).wake(&id, &reason).await }
        })
        .await?;
    if ctx.out.json {
        ctx.emit(to_json(&w), String::new);
        return Ok(());
    }
    let name = device_name(ctx, &id).await;
    let next = ctx.suggest(&format!("session new {id}"));
    ctx.emit(Value::Null, || wake_text(&w, &name, &next));
    Ok(())
}

/// What `extend device wake` says about a request it made or refreshed.
fn wake_text(w: &WakeRequest, name: &str, session_new: &str) -> String {
    let id = &w.device_id;
    let mut lines = Vec::new();
    if w.asks > 1 {
        lines.push(format!(
            "Asked again (ask {}); the request now expires at {}.",
            w.asks,
            short_time(&w.expires_at)
        ));
    } else {
        lines.push(format!(
            "Asked {} to wake {name} ({id}); the request expires at {}.",
            w.to,
            short_time(&w.expires_at)
        ));
        match w.device_notice {
            DeviceNotice::Sent | DeviceNotice::Shown => {
                lines.push(format!("{name} shows your name and reason where it can."));
            }
            DeviceNotice::NotShown => lines.push(format!(
                "{name} couldn't show it{}.",
                w.device_notice_note
                    .as_deref()
                    .map(|n| format!(": {}", n.trim_end_matches('.')))
                    .unwrap_or_default()
            )),
            DeviceNotice::Offline => {
                lines.push(format!("{name} is offline; it gets the request when it reconnects."));
            }
            DeviceNotice::Unsupported => {
                lines.push(format!(
                    "{name} can't show wake requests itself; its Carbon gets it through Ting."
                ));
            }
            DeviceNotice::Other => {}
        }
        if let Some(h) = &w.host {
            lines.push(format!(
                "{} ({}), which it pairs through, must be awake too{}.",
                h.name,
                h.device_id,
                if h.online { "" } else { "; it is offline now" }
            ));
        }
    }
    if !w.wake_detectable {
        lines.push(format!("Extend can't tell when {name} wakes; its Carbon will say so."));
    }
    match (w.ting, w.ting_covered_by) {
        (Some(TingDelivery::Delivered), _) => lines.push("Its Carbon was told through Ting.".into()),
        (Some(TingDelivery::Pending | TingDelivery::Deferred), _) => {
            lines.push("Its Carbon will be told through Ting shortly.".into());
        }
        (Some(TingDelivery::Covered), _) | (None, Some(_)) => {
            lines.push("Its Carbon was already told through Ting about this device in the last 15 minutes.".into())
        }
        (Some(TingDelivery::Failed), _) => lines.push("Ting couldn't tell its Carbon.".into()),
        _ => {}
    }
    if let Some(e) = &w.ting_last_error {
        lines.push(e.clone());
    }
    lines.push(format!("When it's awake you get a Ting; then run: {session_new}"));
    lines.join("\n")
}

/// `extend device wake-requests ls|answer|mute|unmute`.
async fn device_wake_requests(ctx: &mut Ctx, a: &Args) -> R<()> {
    const USAGE: &str = "Usage: extend device wake-requests ls <device_id> [--open] | answer <device_id> woken|declined [--wake-id <id>]... | mute <device_id> [--silicon <si:id>] | unmute <device_id> [--silicon <si:id>]";
    let op = a.pos.first().cloned().unwrap_or_else(|| "ls".into());
    if !["ls", "answer", "mute", "unmute"].contains(&op.as_str()) {
        return Err(CliError::usage(
            format!("unknown `extend device wake-requests {op}`"),
            USAGE,
        ));
    }
    let takes = |allowed: &[&str]| -> R<()> {
        match a.flags.iter().find(|(f, _)| !allowed.contains(&f.as_str())) {
            Some((f, _)) => Err(CliError::usage(
                format!("`extend device wake-requests {op}` doesn't take {f}"),
                USAGE,
            )),
            None => Ok(()),
        }
    };
    let id = a.req(1, "device id")?;
    parse_device_id(&id)?;
    match op.as_str() {
        "ls" => {
            takes(&["--open"])?;
            a.at_most(2)?;
            let state = if a.flag("--open") { "open" } else { "all" };
            let page = ctx
                .call(&format!("GET /api/v2/devices/{id}/wake-requests"), |c, t| {
                    let id = id.clone();
                    async move {
                        c.authed(&t)
                            .wake_requests(
                                &id,
                                ListQuery {
                                    state: Some(state.into()),
                                    limit: Some(100),
                                    ..Default::default()
                                },
                            )
                            .await
                    }
                })
                .await?;
            ctx.emit(to_json(&page), || {
                if page.items.is_empty() {
                    return format!(
                        "No {}requests to wake {id}.",
                        if state == "open" { "open " } else { "" }
                    );
                }
                let mut rows = vec![vec![
                    "TIME".into(),
                    "SILICON".into(),
                    "STATE".into(),
                    "EXPIRES".into(),
                    "SHOWN".into(),
                    "TING".into(),
                    "REASON".into(),
                ]];
                rows.extend(page.items.iter().map(|w| {
                    vec![
                        fmt_time(&w.last_asked_at),
                        w.from.clone(),
                        match w.end_reason {
                            Some(r) => format!("{} ({r})", w.state),
                            None => w.state.to_string(),
                        },
                        fmt_time(&w.expires_at),
                        w.device_notice.to_string(),
                        w.ting.map_or("—".into(), |t| t.to_string()),
                        w.reason.clone(),
                    ]
                }));
                let mut s = table(rows);
                for w in page.items.iter().filter(|w| w.ting_last_error.is_some()) {
                    s.push_str(&format!(
                        "\n{} from {}: {}",
                        w.wake_id,
                        w.from,
                        w.ting_last_error.as_deref().unwrap_or_default()
                    ));
                }
                s
            });
        }
        "answer" => {
            takes(&["--wake-id"])?;
            a.at_most(3)?;
            let kind = a.req(2, "answer (woken or declined)")?;
            let ids = a
                .values("--wake-id")
                .iter()
                .map(|w| {
                    uuid::Uuid::parse_str(w).map_err(|_| {
                        CliError::usage(
                            format!("{w:?} is not a wake request id"),
                            format!("The ids are in `extend device wake-requests ls {id} --open`."),
                        )
                    })
                })
                .collect::<R<Vec<_>>>()?;
            let answer = match kind.as_str() {
                "woken" if !ids.is_empty() => {
                    return Err(CliError::usage(
                        "woken says the whole device is awake, so it takes no --wake-id",
                        format!(
                            "extend device wake-requests answer {id} woken, or decline some: answer {id} declined --wake-id <id>"
                        ),
                    ));
                }
                "woken" => WakeAnswer::woken(),
                "declined" if ids.is_empty() => WakeAnswer::declined(),
                "declined" => WakeAnswer::declined().wake_ids(ids),
                other => {
                    return Err(CliError::usage(
                        format!("the answer is woken or declined, got {other:?}"),
                        format!("extend device wake-requests answer {id} woken (it's awake now), or declined"),
                    ));
                }
            };
            let woken = kind == "woken";
            let r = ctx
                .call(&format!("POST /api/v2/devices/{id}/wake-requests/answer"), |c, t| {
                    let (id, answer) = (id.clone(), answer.clone());
                    async move { c.authed(&t).answer_wake(&id, &answer).await }
                })
                .await?;
            ctx.emit(to_json(&r), || {
                let who: Vec<String> = r.ended.iter().map(|w| w.from.clone()).collect();
                if woken {
                    format!(
                        "{id} is awake: every open request to wake it has ended, whichever Carbon's Silicons asked, and each Silicon that asked gets a Ting.{}",
                        if who.is_empty() {
                            String::new()
                        } else {
                            format!(" Yours: {}.", who.join(", "))
                        }
                    )
                } else {
                    format!(
                        "Declined {} request(s) to wake {id}{}. Each Silicon that asked gets a Ting.",
                        r.ended.len(),
                        if who.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", who.join(", "))
                        }
                    )
                }
            });
        }
        _ => {
            takes(&["--silicon"])?;
            a.at_most(2)?;
            let muted = op == "mute";
            let silicon = a.value("--silicon");
            let mut settings = WakeSettings::new(muted);
            if let Some(s) = &silicon {
                settings = settings.silicon(s.clone());
            }
            let view = ctx
                .call(&format!("PUT /api/v2/devices/{id}/wake-settings"), |c, t| {
                    let (id, settings) = (id.clone(), settings.clone());
                    async move { c.authed(&t).set_wake_settings(&id, &settings).await }
                })
                .await?;
            ctx.emit(to_json(&view), || {
                let whose = match &silicon {
                    Some(s) => format!("Wake requests from {s} for {id}"),
                    None => format!("Wake requests for {id}"),
                };
                if muted {
                    format!(
                        "{whose} are off; the open ones were withdrawn. Turn them on again: extend device wake-requests unmute {id}{}",
                        silicon.as_deref().map(|s| format!(" --silicon {s}")).unwrap_or_default()
                    )
                } else {
                    format!("{whose} are on.")
                }
            });
        }
    }
    Ok(())
}

fn requests_table(items: &[RequestInfo], me: Option<&str>) -> String {
    if items.is_empty() {
        return "No requests.".into();
    }
    let mut rows = vec![vec![
        "TIME".into(),
        "FROM".into(),
        "TO".into(),
        "DEVICE".into(),
        "DELIVERY".into(),
        "REASON".into(),
    ]];
    rows.extend(items.iter().map(|r| {
        vec![
            fmt_time(&r.created_at),
            r.from.clone(),
            // A request routed to this Carbon, because the Silicon using their device isn't on the
            // asker's side.
            if Some(r.to.as_str()) == me {
                "you".into()
            } else {
                r.to.clone()
            },
            r.device_id.to_string(),
            format!("{:?}", r.delivery).to_lowercase(),
            r.reason.clone(),
        ]
    }));
    let mut s = table(rows);
    for r in items.iter().filter(|r| r.last_error.is_some()) {
        s.push_str(&format!(
            "\n{} to {}: {}",
            r.request_id,
            r.to,
            r.last_error.as_deref().unwrap_or_default()
        ));
    }
    s
}

// ───────────────────────────── Ting ─────────────────────────────

/// `extend ting status|on`: whether Extend's notifications reach you through Ting, and why not.
fn ting_text(r: &TingRegistration, turned_on: bool) -> String {
    if r.delivery_enabled == Some(false) {
        return "Notifications through Ting are off on this Extend server, so none are sent: requests and wake requests show on the website, with `extend request ls` and `extend device wake-requests ls`, and on the device.".into();
    }
    let mut s = match r.status {
        TingStatus::On if turned_on => {
            "Extend's notifications reach you through Ting again; the ones that waited were sent.".to_owned()
        }
        TingStatus::On => "Extend's notifications reach you through Ting.".to_owned(),
        TingStatus::Off => "You turned Extend's notifications off in Ting. Turn them on: extend ting on".to_owned(),
        _ => {
            "Extend hasn't registered you with Ting yet; it does at your next use of Extend. Do it now: extend ting on"
                .to_owned()
        }
    };
    if let Some(e) = &r.last_error {
        s.push_str(&format!("\nLast problem: {e}"));
    }
    if !r.missing_types.is_empty() {
        s.push_str(&format!("\n\n{}", missing_types_text(&r.missing_types)));
    }
    s
}

async fn ting(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "status");
    let path = format!("ting {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("ting", &sub));
    }
    let a = Args::parse(rest, &path)?;
    a.at_most(0)?;
    let on = sub == "on";
    let r = if on {
        ctx.call("PUT /api/v2/ting-registration", |c, t| async move {
            c.authed(&t).ting_turn_on().await
        })
        .await?
    } else {
        ctx.call("GET /api/v2/ting-registration", |c, t| async move {
            c.authed(&t).ting_registration().await
        })
        .await?
    };
    ctx.emit(to_json(&r), || ting_text(&r, on));
    Ok(0)
}

// ───────────────────────────── Sessions ─────────────────────────────

/// Keeps what `extend --help` lists in step with the session as Extend last described it: saved
/// for the connected session (or one already cached, or `connect`), and forgotten once it ended.
fn remember_session(ctx: &Ctx, s: &Session, connect: bool) -> R<()> {
    let sid = s.session_id.to_string();
    if s.state == SessionState::Ended {
        forget_session(ctx, &sid);
        return Ok(());
    }
    let current = store::current_session(ctx.account());
    let cached = store::load_session_cache(ctx.account(), &sid).is_some();
    if !(connect || cached || current.as_deref() == Some(sid.as_str())) {
        return Ok(());
    }
    let d = s.device.as_deref();
    let cache = store::SessionCache {
        session_id: sid.clone(),
        device_id: s.device_id.to_string(),
        device_name: d.map(|d| d.name.clone()).unwrap_or_default(),
        os: d.map(|d| d.os.as_str().to_owned()).unwrap_or_default(),
        capabilities: s
            .capabilities
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect(),
        commands: s.commands.clone().unwrap_or_default(),
        missing: d
            .and_then(|d| d.missing.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|m| store::MissingNote {
                capability: m.capability.as_str().to_owned(),
                reason: m.reason,
            })
            .collect(),
        refreshed_at: now_s(),
    };
    store::save_session_cache(ctx.account(), &cache)?;
    if connect {
        store::set_current_session(ctx.account(), Some(&sid))?;
    }
    Ok(())
}

/// The session ended or is gone: stop listing its device's commands, and disconnect from it.
fn forget_session(ctx: &Ctx, sid: &str) {
    store::remove_session_cache(ctx.account(), sid);
    if store::current_session(ctx.account()).as_deref() == Some(sid) {
        let _ = store::set_current_session(ctx.account(), None);
    }
}

async fn read_session(ctx: &mut Ctx, id: &str) -> R<Session> {
    ctx.call(&format!("GET /api/v2/sessions/{id}"), |c, t| {
        let id = id.to_owned();
        async move { c.authed(&t).session(&id).await }
    })
    .await
    .inspect_err(|e| {
        if e.code == ErrorCode::SessionNotFound {
            forget_session(ctx, id);
        }
    })
}

async fn connect_session(ctx: &mut Ctx, id: &str) -> R<Session> {
    let s = read_session(ctx, id).await?;
    if s.state == SessionState::Ended {
        forget_session(ctx, id);
        return Err(CliError::new(
            ErrorCode::SessionEnded,
            format!(
                "Session {id} has ended ({}).",
                s.end_reason.map_or("unknown reason", EndReason::explain)
            ),
        )
        .hint(format!(
            "Start a new one: {}",
            ctx.suggest(&format!("session new {}", s.device_id))
        )));
    }
    remember_session(ctx, &s, true)?;
    Ok(s)
}

async fn session(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "status");
    let path = format!("session {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("session", &sub));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "new" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let did = parse_device_id(&id)?;
            let s = ctx
                .call("POST /api/v2/sessions", |c, t| {
                    let did = did.clone();
                    async move { c.authed(&t).start_session(&did).await }
                })
                .await?;
            let sid = s.session_id.to_string();
            let connected = if a.flag("--connect") {
                Some(connect_session(ctx, &sid).await?)
            } else {
                None
            };
            if ctx.out.json {
                ctx.emit(to_json(&s), String::new);
            } else {
                outln!("{sid}");
                errln!(
                    "Started session {sid} on {id}.{} It ends after 5 minutes without a command; end it with `{}`.",
                    if a.flag("--connect") {
                        format!(" Connected — try `{}`.", ctx.suggest("snapshot -i"))
                    } else {
                        format!(" Next: {}.", ctx.suggest(&format!("session connect {sid}")))
                    },
                    ctx.suggest(&format!("session end {sid}"))
                );
                // Not being awake refuses nothing (the terminal and Android debugging work as
                // usual), so the session starts; the note says what won't work yet, and how to ask.
                let device = match s.device.clone().or_else(|| connected.and_then(|c| c.device)) {
                    Some(d) => Some(*d),
                    None => ctx
                        .call(&format!("GET /api/v2/devices/{id}"), |c, t| {
                            let id = id.clone();
                            async move { c.authed(&t).device(&id).await }
                        })
                        .await
                        .ok(),
                };
                if let Some(d) = device.filter(|d| d.awake == Some(false)) {
                    ctx.out.note(&format!(
                        "{} isn't awake{}; commands that need its screen will fail until its Carbon turns it on. Ask: {}",
                        d.name,
                        d.sleep_state.map(|st| format!(" ({})", st.label())).unwrap_or_default(),
                        ctx.suggest(&format!("device wake {id} --reason \"...\""))
                    ));
                }
            }
        }
        "connect" => {
            a.at_most(1)?;
            let id = a.req(0, "session id")?;
            let s = connect_session(ctx, &id).await?;
            let d = s.device.clone();
            let try_it = ctx.suggest("snapshot -i");
            ctx.emit(to_json(&s), || {
                format!(
                    "Connected to {id} on {} ({}). `extend --help` now lists only what works there. Try: {try_it}",
                    d.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
                    d.as_ref().map(|d| d.os.as_str()).unwrap_or_default()
                )
            });
        }
        "disconnect" => {
            a.at_most(0)?;
            store::set_current_session(ctx.account(), None)?;
            ctx.emit(json!({"connected": null}), || {
                "Disconnected. The session keeps running until it's ended or idle for 5 minutes.".into()
            });
        }
        "status" => {
            a.at_most(1)?;
            let id = a.pos.first().cloned().map_or_else(|| ctx.session_id(), Ok)?;
            let s = read_session(ctx, &id).await?;
            remember_session(ctx, &s, false)?;
            ctx.emit(to_json(&s), || {
                let dev = s
                    .device
                    .as_ref()
                    .map(|d| format!("{} ({})", d.name, d.device_id))
                    .unwrap_or_else(|| s.device_id.to_string());
                match s.state {
                    SessionState::Ended => format!(
                        "{} ended {} on {dev}: {}.",
                        s.session_id,
                        s.ended_at.map(|t| fmt_time(&t)).unwrap_or_default(),
                        s.end_reason.map_or("unknown", EndReason::explain)
                    ),
                    st => format!(
                        "{} {} on {dev} since {}, {} command(s), ends if idle at {}. {} command(s) work there now.",
                        s.session_id,
                        if st == SessionState::Paused {
                            "paused (takeover)"
                        } else {
                            "active"
                        },
                        fmt_time(&s.started_at),
                        s.command_count,
                        s.idle_ends_at.map(|t| fmt_time(&t)).unwrap_or_default(),
                        s.commands.as_ref().map_or(0, Vec::len)
                    ),
                }
            });
        }
        "ls" => {
            a.at_most(0)?;
            let silicon = a.value("--silicon");
            let q = ListQuery {
                device_id: a.value("--device"),
                state: a.value("--state"),
                silicon: silicon.clone(),
                limit: Some(50),
                ..Default::default()
            };
            let page = ctx
                .call("GET /api/v2/sessions", |c, t| {
                    let q = q.clone();
                    async move { c.authed(&t).sessions(q).await }
                })
                .await?;
            ctx.emit(to_json(&page), || {
                if page.items.is_empty() {
                    return match &silicon {
                        Some(s) => format!("No sessions of {s}."),
                        None => "No sessions.".into(),
                    };
                }
                let mut rows = vec![
                    [
                        "SESSION",
                        "DEVICE",
                        "SILICON",
                        "STATE",
                        "STARTED",
                        "COMMANDS",
                        "ENDED BECAUSE",
                    ]
                    .map(String::from)
                    .to_vec(),
                ];
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
                let mut t = table(rows);
                if silicon.is_some() && page.items.iter().any(|s| s.state != SessionState::Ended) {
                    t.push_str("\n\nStop one: extend session end <session_id>");
                }
                t
            });
        }
        _ => {
            a.at_most(1)?;
            let id = a.pos.first().cloned().map_or_else(|| ctx.session_id(), Ok)?;
            let s = ctx
                .call(&format!("POST /api/v2/sessions/{id}/end"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).end_session(&id).await }
                })
                .await?;
            forget_session(ctx, &id);
            ctx.emit(to_json(&s), || {
                format!(
                    "Ended {} after {} command(s). {} is free for other Silicons.",
                    s.session_id, s.command_count, s.device_id
                )
            });
        }
    }
    Ok(0)
}

async fn takeover(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, "takeover")?;
    a.at_most(1)?;
    let sid = ctx.session_id()?;
    match a.pos.first().map(String::as_str) {
        None => {
            let reason = a.value("--reason").ok_or_else(|| {
                CliError::usage(
                    "--reason is required",
                    format!(
                        "Tell the Carbon what to do: {}",
                        ctx.suggest("takeover --reason \"Please approve the Face ID prompt\"")
                    ),
                )
            })?;
            let t = ctx
                .call(&format!("POST /api/v2/sessions/{sid}/takeover"), |c, tok| {
                    let (sid, reason) = (sid.clone(), reason.clone());
                    async move { c.authed(&tok).takeover(&sid, &reason).await }
                })
                .await?;
            ctx.emit(to_json(&t), || {
                format!(
                    "Session {sid} is paused; the device shows your reason and a Done button. Commands wait until the Carbon taps Done (by {}).",
                    fmt_time(&t.expires_at)
                )
            });
        }
        Some("status") => {
            let t = ctx
                .call(&format!("GET /api/v2/sessions/{sid}/takeover"), |c, tok| {
                    let sid = sid.clone();
                    async move { c.authed(&tok).takeover_status(&sid).await }
                })
                .await?;
            ctx.emit(to_json(&t), || match &t {
                Some(t) => format!(
                    "Paused since {}: {} (ends by {}).",
                    fmt_time(&t.started_at),
                    t.reason,
                    fmt_time(&t.expires_at)
                ),
                None => "No takeover in progress.".into(),
            });
        }
        Some("release") => {
            ctx.call(&format!("DELETE /api/v2/sessions/{sid}/takeover"), |c, tok| {
                let sid = sid.clone();
                async move { c.authed(&tok).release_takeover(&sid).await }
            })
            .await?;
            ctx.emit(json!({"released": sid}), || format!("Session {sid} is active again."));
        }
        Some(o) => {
            return Err(CliError::usage(
                format!("unknown `extend takeover {o}`"),
                "Usage: extend takeover --reason \"...\" | extend takeover status | extend takeover release",
            ));
        }
    }
    Ok(0)
}

async fn request(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("request {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("request", &sub));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "send" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            parse_device_id(&id)?;
            let reason = a.value("--reason").ok_or_else(|| {
                CliError::usage(
                    "--reason is required (1–300 characters)",
                    format!(
                        "Say why you need it: {}",
                        ctx.suggest(&format!("request send {id} --reason \"Need 2 minutes to read an OTP\""))
                    ),
                )
            })?;
            let n = reason.trim().chars().count();
            if n == 0 || n > extend_protocol::REASON_MAX_CHARS {
                return Err(CliError::usage(
                    format!("--reason must be 1–300 characters; it is {n}."),
                    "The Silicon using the device reads it as written; keep it to one or two sentences.",
                ));
            }
            let r = ctx
                .call(&format!("POST /api/v2/devices/{id}/requests"), |c, t| {
                    let (id, reason) = (id.clone(), reason.clone());
                    async move { c.authed(&t).send_request(&id, &reason).await }
                })
                .await?;
            ctx.emit(to_json(&r), || {
                let delivery = format!("{:?}", r.delivery).to_lowercase();
                // Routed to the Carbon who gave the Silicon using the device access: that Silicon
                // isn't on the asker's side (another custodian's Silicon, or another Carbon's pair), so
                // it isn't named.
                let mut s = if r.to_hidden || r.routed_to == Some(RequestRoute::Carbon) {
                    format!(
                        "Sent to the Carbon who gave access to the Silicon using it; it's in use by a Silicon you can't see. Delivery: {delivery}."
                    )
                } else {
                    format!(
                        "Sent to {} (using {}{}). Delivery: {delivery}.",
                        r.to,
                        r.device_id,
                        r.session_id
                            .as_ref()
                            .map(|s| format!(" in session {s}"))
                            .unwrap_or_default(),
                    )
                };
                if let Some(e) = &r.last_error {
                    s.push_str(&format!(" {e}"));
                }
                s
            });
        }
        _ => {
            a.at_most(0)?;
            let direction = match (a.flag("--sent"), a.flag("--received")) {
                (true, true) => {
                    return Err(CliError::usage(
                        "--sent and --received can't be used together",
                        "Leave both out to list requests in both directions.",
                    ));
                }
                (true, false) => Some("sent".into()),
                (false, true) => Some("received".into()),
                (false, false) => None,
            };
            let q = ListQuery {
                direction,
                device_id: a.value("--device"),
                silicon: a.value("--silicon"),
                limit: Some(50),
                ..Default::default()
            };
            let page = ctx
                .call("GET /api/v2/requests", |c, t| {
                    let q = q.clone();
                    async move { c.authed(&t).requests(q).await }
                })
                .await?;
            let me = ctx.member_id();
            ctx.emit(to_json(&page), || requests_table(&page.items, me.as_deref()));
        }
    }
    Ok(0)
}

// ───────────────────────────── Files ─────────────────────────────

async fn file(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("file {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("file", &sub));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "ls" => {
            a.at_most(0)?;
            let q = ListQuery {
                // `--session` is the global flag; here it filters.
                session_id: ctx.g.session.clone(),
                device_id: a.value("--device"),
                kind: a.value("--kind"),
                silicon: a.value("--silicon"),
                limit: Some(100),
                ..Default::default()
            };
            let page = ctx
                .call("GET /api/v2/files", |c, t| {
                    let q = q.clone();
                    async move { c.authed(&t).files(q).await }
                })
                .await?;
            ctx.emit(to_json(&page), || {
                if page.items.is_empty() {
                    return "No files.".into();
                }
                let mut rows = vec![
                    ["FILE", "KIND", "BY", "SIZE", "SELF-DESTRUCTS", "LINK"]
                        .map(String::from)
                        .to_vec(),
                ];
                rows.extend(page.items.iter().map(|f| {
                    vec![
                        f.file_id.to_string(),
                        f.kind.as_str().into(),
                        f.created_by.clone().unwrap_or_else(|| "—".into()),
                        readable_size(f.size_bytes),
                        f.self_destruct_at.map_or("never".into(), |t| fmt_time(&t)),
                        f.url.clone(),
                    ]
                }));
                table(rows)
            });
        }
        _ => {
            a.at_most(1)?;
            let id = a.req(0, "file id")?;
            let f = if sub == "keep" {
                ctx.call(&format!("POST /api/v2/files/{id}/keep"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).keep_file(&id).await }
                })
                .await?
            } else {
                ctx.call(&format!("GET /api/v2/files/{id}"), |c, t| {
                    let id = id.clone();
                    async move { c.authed(&t).file(&id).await }
                })
                .await?
            };
            if sub == "get" {
                // The link first, so it's there even if the download fails.
                if !ctx.out.json {
                    outln!("{}", file_line(&f));
                    let _ = std::io::stdout().flush();
                }
                let out = a.value("--out");
                let saved = save_file(ctx, &f, out.as_deref(), 0).await?;
                ctx.emit(json!({"file": f, "saved_to": saved.path, "bytes": saved.bytes}), || {
                    format!(
                        "Saved to {} ({})",
                        saved.path.display(),
                        readable_size(saved.bytes as i64)
                    )
                });
            } else {
                ctx.emit(to_json(&f), || file_line(&f));
            }
        }
    }
    Ok(0)
}

fn readable_size(b: i64) -> String {
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{:.1} KiB", b as f64 / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MiB", b as f64 / 1_048_576.0),
        _ => format!("{:.1} GiB", b as f64 / 1_073_741_824.0),
    }
}

fn file_line(f: &FileInfo) -> String {
    format!(
        "{} {} ({}, {}) {}\n  {}",
        f.kind.as_str(),
        f.name,
        readable_size(f.size_bytes),
        f.self_destruct_at
            .map_or("permanent".into(), |t| format!("self-destructs {}", fmt_time(&t))),
        f.file_id,
        f.url
    )
}

/// A file name that stays inside the directory it's saved to.
fn safe_name(f: &FileInfo) -> String {
    Path::new(&f.name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n != "." && n != "..")
        .unwrap_or_else(|| f.file_id.to_string())
}

/// `shot.png` → `shot-2.png`, for the second of several files saved to one `--out` path.
fn numbered(p: &Path, n: usize) -> PathBuf {
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match p.extension() {
        Some(e) => format!("{stem}-{n}.{}", e.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    p.with_file_name(name)
}

struct Saved {
    path: PathBuf,
    bytes: u64,
}

/// Where a file goes: `--out` (a directory, or a path; the `index`th of several files gets a
/// numbered name), else `download_dir`, else here.
fn save_path(ctx: &Ctx, f: &FileInfo, out: Option<&str>, index: usize) -> R<PathBuf> {
    let name = safe_name(f);
    Ok(match out {
        Some(o) => {
            let p = PathBuf::from(o);
            if o.ends_with('/') || o.ends_with(std::path::MAIN_SEPARATOR) || p.is_dir() {
                std::fs::create_dir_all(&p).map_err(|e| {
                    CliError::usage(
                        format!("Could not create the directory {o}: {e}"),
                        "Give --out a directory this user can write, or a file path.",
                    )
                })?;
                p.join(name)
            } else if index == 0 {
                p
            } else {
                numbered(&p, index + 1)
            }
        }
        None => ctx
            .cfg
            .get("download_dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(name),
    })
}

/// Downloads a file through Extend (`GET /api/v2/files/{file_id}/content`), which reads it from
/// Briefcase as the caller, and streams it to disk.
async fn save_file(ctx: &mut Ctx, f: &FileInfo, out: Option<&str>, index: usize) -> R<Saved> {
    let path = save_path(ctx, f, out, index)?;
    let id = f.file_id.to_string();
    let failed = |e: CliError| -> CliError {
        let hint = format!(
            "{}The file is still in Briefcase: {}",
            e.hint.as_deref().map(|h| format!("{h} ")).unwrap_or_default(),
            f.url
        );
        CliError {
            message: format!("Could not download {} ({}): {}", f.name, f.file_id, e.message),
            hint: Some(hint),
            details: Box::new(json!({"file_id": f.file_id, "url": f.url, "details": e.details})),
            ..e
        }
    };
    let t = Instant::now();
    let mut dl = ctx
        .call(&format!("GET /api/v2/files/{id}/content"), |c, tok| {
            let id = id.clone();
            async move { c.authed(&tok).file_download(&id, None).await }
        })
        .await
        .map_err(failed)?;
    let write_failed = |e: std::io::Error| {
        CliError::usage(
            format!("Could not write {}: {e}", path.display()),
            format!(
                "Check the directory exists and this user can write it, or pass another --out. The file is still in Briefcase: {}",
                f.url
            ),
        )
    };
    let file_name = path
        .file_name()
        .map_or_else(|| "download".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{file_name}.part{}", std::process::id()));
    let mut file = std::fs::File::create(&tmp).map_err(write_failed)?;
    let mut bytes = 0u64;
    loop {
        match dl.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(e) = file.write_all(&chunk) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(write_failed(e));
                }
                bytes += chunk.len() as u64;
            }
            Ok(None) => break,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(failed(e.into()));
            }
        }
    }
    let finish = file.sync_all().and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = finish {
        let _ = std::fs::remove_file(&tmp);
        return Err(write_failed(e));
    }
    ctx.out
        .verbose(|| format!("saved {bytes} bytes to {} in {} ms", path.display(), ms(t)));
    Ok(Saved { path, bytes })
}

async fn report(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, "report")?;
    let message = a.pos.join(" ");
    if message.trim().is_empty() {
        return Err(CliError::usage(
            "describe the bug",
            "extend report \"what happened, what you expected, how to reproduce\" [--pr <link>]",
        ));
    }
    let context = json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cli": env!("CARGO_PKG_VERSION"),
        "session": store::current_session(ctx.account()),
    });
    let input = ReportInput {
        message,
        pr: a.value("--pr"),
        client_version: format!("extend-cli {}", env!("CARGO_PKG_VERSION")),
        context,
    };
    let r = ctx
        .call("POST /api/v2/reports", |c, t| {
            let input = input.clone();
            async move { c.authed(&t).report(&input).await }
        })
        .await?;
    let pr = input.pr.is_some();
    ctx.emit(to_json(&r), || {
        let mut s = match r.notification.as_str() {
            "sent" => format!("Report {} sent to Extend's maintainers.", r.report_id),
            "queued" => format!(
                "Report {} saved. Emailing it to Extend's maintainers failed for now; Extend keeps retrying.",
                r.report_id
            ),
            _ => format!(
                "Report {} saved, but not emailed: this Extend doesn't send email (a development service, or one \
                 without email set up).",
                r.report_id
            ),
        };
        if !pr {
            s.push_str(&format!("\nExtend is open source: if you can fix it, open a pull request at {} and report again with --pr <link>.", r.repository_url));
        }
        s
    });
    Ok(0)
}

// ───────────────────────────── Device commands ─────────────────────────────

fn parse_ttl(s: &str) -> R<u32> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let example = "Use a duration from 1m to 30d: 90m, 12h, 7d.";
    let n: u32 = n
        .parse()
        .map_err(|_| CliError::usage(format!("{s:?} is not a duration"), example))?;
    let minutes = match unit {
        "m" | "min" => Some(n),
        "h" => n.checked_mul(60),
        "d" | "" => n.checked_mul(1440),
        _ => {
            return Err(CliError::usage(
                format!("{s:?} has an unknown unit {unit:?}"),
                "Units are m (minutes), h (hours) and d (days): 90m, 12h, 7d.",
            ));
        }
    };
    match minutes {
        Some(m) if (1..=extend_protocol::SELF_DESTRUCT_MAX_MIN).contains(&m) => Ok(m),
        _ => Err(CliError::usage(
            format!("{s:?} is outside 1 minute to 30 days"),
            format!("{example} To keep a file for good, use --keep (or `extend file keep <file_id>`)."),
        )),
    }
}

/// A device command's arguments with Extend's own flags taken out.
#[derive(Debug, Default, PartialEq)]
struct DeviceArgs {
    /// What the device receives.
    args: Vec<String>,
    ttl: Option<u32>,
    keep: bool,
    out: Option<String>,
}

/// Takes Extend's flags for file-making commands (`--ttl`, `--keep`, `--out`) out of a device
/// command's arguments.
///
/// Most commands take them anywhere before a `--`; the `--` and everything after it go to the
/// device untouched, so the device's parser also reads the rest as positional. A verbatim command
/// (`adb`) takes them only before its first argument: from there on every token is the device's,
/// and a leading `--` just ends Extend's flags and is not sent.
fn split_device_args(name: &str, raw: Vec<String>) -> R<DeviceArgs> {
    let verbatim = VERBATIM_COMMANDS.contains(&name);
    let mut d = DeviceArgs::default();
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ttl" => d.ttl = Some(parse_ttl(&it.next().ok_or_else(|| missing_value("--ttl"))?)?),
            "--keep" => d.keep = true,
            "--out" => d.out = Some(it.next().ok_or_else(|| missing_value("--out"))?),
            "--" => {
                if !verbatim {
                    d.args.push(a);
                }
                d.args.extend(it);
                break;
            }
            _ => {
                d.args.push(a);
                if verbatim {
                    d.args.extend(it);
                    break;
                }
            }
        }
    }
    if name == "adb" {
        take_adb_pull_destination(&mut d)?;
    }
    Ok(d)
}

const ADB_PULL_EXAMPLE: &str =
    "extend adb pull <device path> <local path>, like: extend adb pull /sdcard/Download/report.pdf ./report.pdf";

/// `adb pull` sends the device exactly one device path. Where to save the file on this computer is
/// taken out here and never sent: a second path, as real adb reads `pull REMOTE LOCAL`, or
/// `--out <local path>` before or after the device path (real adb pull has no `--out`, so it can't
/// be meant for the device).
fn take_adb_pull_destination(d: &mut DeviceArgs) -> R<()> {
    if d.args.first().map(String::as_str) != Some("pull") {
        return Ok(());
    }
    let mut rest = d.args.split_off(1);
    // `--out` given before `pull` comes first.
    let mut locals: Vec<String> = d.out.take().into_iter().collect();
    while let Some(i) = rest.iter().position(|a| a == "--out") {
        if i + 1 == rest.len() {
            return Err(missing_value("--out").hint(ADB_PULL_EXAMPLE));
        }
        locals.push(rest.remove(i + 1));
        rest.remove(i);
    }
    if let [remote, local] = rest.as_slice()
        && !remote.starts_with('-')
        && !local.starts_with('-')
    {
        locals.extend(rest.pop());
    }
    if locals.len() > 1 {
        return Err(CliError::usage(
            format!(
                "`extend adb pull` was given {} local paths to save to ({}); it saves one pulled file to one place.",
                locals.len(),
                locals.join(", ")
            ),
            format!("Give the local path once: {ADB_PULL_EXAMPLE}"),
        ));
    }
    d.out = locals.pop();
    d.args.extend(rest);
    Ok(())
}

/// `extend adb push` / `extend adb install` / `extend install`, for messages.
fn command_label(name: &str, args: &[String]) -> String {
    match (name, args.first()) {
        ("adb", Some(verb)) => format!("extend adb {verb}"),
        _ => format!("extend {name}"),
    }
}

/// Commands that take a local file only: their shape is checked here, so a mistake is explained
/// before anything is sent. These are the only forms Android takes (AdbCommands.kt and
/// AdbExecutor.kt in apps/android): `push <file> <path>`, `install [-r] <apk>`,
/// `install|reinstall <package> <apk>`.
fn check_local_input_shape(name: &str, args: &[String]) -> R<()> {
    let label = command_label(name, args);
    let got = format!(
        "`{}`",
        std::iter::once(format!("extend {name}"))
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    );
    match (name, args.first().map(String::as_str)) {
        ("adb", Some("push")) if args.len() != 3 || args[1..].iter().any(|a| a.starts_with('-')) => {
            Err(CliError::usage(
                format!("`{label}` takes one local file and one device path, and got {got}."),
                "Push one file per command, without adb push options: extend adb push ./photo.jpg /sdcard/Download/photo.jpg",
            ))
        }
        ("adb", Some("install")) => {
            let rest = &args[1..];
            let apks = if rest.first().map(String::as_str) == Some("-r") {
                &rest[1..]
            } else {
                rest
            };
            if apks.len() == 1 && !apks[0].starts_with('-') {
                return Ok(());
            }
            Err(CliError::usage(
                format!("`{label}` takes an optional -r and one local .apk, and got {got}."),
                "Run extend adb install -r ./app.apk. For other pm install options, push the APK first \
                 (extend adb push ./app.apk /data/local/tmp/app.apk), then run extend adb shell pm install <options> /data/local/tmp/app.apk.",
            ))
        }
        ("install" | "reinstall", _) if args.len() != 2 => Err(CliError::usage(
            format!("`{label}` takes a package name and a local .apk, and got {got}."),
            format!("Run {label} com.example.app ./app.apk, with the package name the APK declares."),
        )),
        _ => Ok(()),
    }
}

/// The same command again with `local` in place of its local file.
fn retry_with(name: &str, args: &[String], local: &str) -> String {
    let label = command_label(name, args);
    match (name, args.first().map(String::as_str)) {
        ("adb", Some("push")) => format!(
            "{label} {local} {}",
            args.get(2).map_or("<device path>", String::as_str)
        ),
        ("adb", _) => format!("{label} {local}"),
        (_, Some(package)) => format!("{label} {package} {local}"),
        _ => format!("{label} <package> {local}"),
    }
}

/// Why `a` can't be sent as the local file a command needs, and what to do instead.
fn not_a_local_file(name: &str, args: &[String], a: &str) -> CliError {
    let label = command_label(name, args);
    let push = label == "extend adb push";
    let local = if push { "./file" } else { "./app.apk" };
    let retry = retry_with(name, args, local);
    let sends = if push {
        "sends a file from this computer with the command"
    } else {
        "sends the APK from this computer with the command"
    };
    let path = Path::new(a);
    if a.contains("://") {
        CliError::usage(
            format!("`{label}` got a link, {a}, but it {sends} and can't fetch links or Briefcase files yet."),
            format!(
                "Download it first (`extend file get <file_id> --out {local}` for a Briefcase file, or `curl -L -o {local} '{a}'`), then run `{retry}`."
            ),
        )
    } else if path.is_dir() {
        if push {
            let dir = path
                .file_name()
                .map_or_else(|| "files".to_owned(), |f| f.to_string_lossy().into_owned());
            CliError::usage(
                format!("{a} is a directory; `{label}` sends one file from this computer per command."),
                format!(
                    "Push its files one at a time, or pack it into one file (`tar -cf {dir}.tar -C {a} .`), push that, and unpack it on the device: \
                 `extend adb shell 'mkdir -p /data/local/tmp/{dir} && tar -xf /data/local/tmp/{dir}.tar -C /data/local/tmp/{dir}'`."
                ),
            )
        } else {
            CliError::usage(
                format!("{a} is a directory, not an APK; `{label}` {sends}."),
                "Pass the .apk file itself, for example app/build/outputs/apk/release/app-release.apk.",
            )
        }
    } else if path.exists() {
        CliError::usage(
            format!("{a} is not a regular file (it may be a device or a pipe); `{label}` {sends}."),
            format!("Copy it into a regular file first, then run `{retry}`."),
        )
    } else if uuid::Uuid::parse_str(a).is_ok() {
        CliError::usage(
            format!(
                "There is no file named {a} here, and it looks like a Briefcase file id; `{label}` {sends} and can't take Briefcase files yet."
            ),
            format!("Download it first with `extend file get {a} --out {local}`, then run `{retry}`."),
        )
    } else {
        let cwd =
            std::env::current_dir().map_or_else(|_| "the current directory".to_owned(), |d| d.display().to_string());
        CliError::usage(
            format!("There is no file at {a} on this computer; `{label}` {sends}, so it needs a local path."),
            format!(
                "Check the path (a relative path starts at {cwd}). A Briefcase link or file id isn't accepted in its place yet: download it first with `extend file get <file_id> --out {local}`."
            ),
        )
    }
}

/// Reads the local files a command names (silicon-extend-client's `attachments`), replacing each
/// argument with `attachment:<name>`, and explains a refusal in terms of this command.
fn attach_local_files(name: &str, args: &mut [String]) -> R<Vec<Attachment>> {
    check_local_input_shape(name, args)?;
    let before = args.to_vec();
    attach::attach_local_files(name, args).map_err(|e| match e {
        AttachmentError::NotAFile { path, .. } => not_a_local_file(name, &before, &path),
        AttachmentError::TooLarge { path, size, already } => too_big_error(name, &before, &path, size, already),
        AttachmentError::TooMany { .. } => CliError::usage(
            format!("`extend {name}` names more than {MAX_ATTACHMENTS} local files; a command can carry at most {MAX_ATTACHMENTS}."),
            "Send them in several commands with fewer files each.",
        ),
        AttachmentError::AppBundle { path } => CliError::usage(
            format!("{path} is an Android App Bundle (.aab); Android installs APKs, and Extend can't turn a bundle into one."),
            format!(
                "Build a universal APK from it on this computer (`bundletool build-apks --bundle={path} --output=app.apks --mode=universal && unzip -o app.apks universal.apk`), then run `{}`.",
                retry_with(name, &before, "./universal.apk")
            ),
        ),
        AttachmentError::Unreadable { path, error } => CliError::usage(
            format!("Could not read {path}: {error}."),
            "Check that the file exists and this user can read it (ls -l).",
        ),
    })
}

/// Says why a local file can't be sent, and what works instead for this command.
fn too_big_error(name: &str, args: &[String], local: &str, size: u64, already: u64) -> CliError {
    let limit = format!("8 MiB ({MAX_ATTACHMENT_BYTES} bytes)");
    // Exact bytes where rounding would make a size look like the limit itself.
    let size_text = |b: u64| {
        if readable_size(b as i64) == readable_size(MAX_ATTACHMENT_BYTES as i64) {
            format!("{b} bytes")
        } else {
            readable_size(b as i64)
        }
    };
    let message = if already == 0 {
        format!(
            "{local} is {}; a command can carry at most {limit} of local files.",
            size_text(size)
        )
    } else {
        format!(
            "{local} ({}) would bring this command's local files to {}; a command can carry at most {limit} of them in total.",
            size_text(size),
            size_text(already + size)
        )
    };
    let file = Path::new(local)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let parts = format!(
        "split it into parts of 8 MiB or less (`split -b 8m {local} {file}.part.`), push each part to its own path under /data/local/tmp, and join them there with `extend adb shell 'cat /data/local/tmp/{file}.part.* > /data/local/tmp/{file}'`"
    );
    let hint = match (name, args.first().map(String::as_str)) {
        ("adb", Some("push")) => format!("Extend can't push a Briefcase link or file id yet. To send a larger file, {parts}."),
        ("adb", _) | ("install" | "reinstall", _) => format!(
            "Extend can't install from a Briefcase link or file id yet. Use an APK of 8 MiB or less (for example one built for a single ABI), or {parts}, then install it with `extend adb shell pm install -r /data/local/tmp/{file}`."
        ),
        ("display", _) => "Pass an http(s) link to it instead (`--image https://…` or `--video https://…`); the device loads it itself.".into(),
        _ => "Split the steps across smaller scripts, or send fewer files per command.".into(),
    };
    CliError::usage(message, hint)
}

async fn device_command(ctx: &mut Ctx, name: &str, raw: Vec<String>) -> R<i32> {
    let sid = ctx.session_id()?;
    let DeviceArgs {
        mut args,
        mut ttl,
        keep,
        out,
    } = split_device_args(name, raw)?;
    if ttl.is_none()
        && let Some(d) = ctx.cfg.get("self_destruct")
    {
        ttl = Some(parse_ttl(d)?);
    }
    // Flags Extend adds go before any `--`, where the device still reads flags.
    let flags_end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    if name == "screenshot"
        && !args[..flags_end].iter().any(|a| a == "--scale")
        && let Some(s) = ctx.cfg.get("screenshot_scale")
    {
        args.splice(flags_end..flags_end, ["--scale".to_owned(), s.clone()]);
    }
    // Scripts and media are read here, on the caller's machine, and sent along.
    let attachments = attach_local_files(name, &mut args)?;
    let req = CommandRequest {
        command: name.to_owned(),
        args,
        timeout_ms: ctx.g.timeout,
        self_destruct_minutes: ttl,
        permanent: keep,
        attachments,
    };
    // While the command runs, re-read the session, so `extend --help` lists what its device can
    // do now (a permission granted since connecting, a device that came online).
    let watch = store::current_session(ctx.account()).as_deref() == Some(sid.as_str())
        || store::load_session_cache(ctx.account(), &sid).is_some();
    let peek = {
        let client = ctx.client().await?;
        let token = ctx.token().await?;
        let sid = sid.clone();
        async move {
            if watch {
                client.authed(&token).session(&sid).await.ok()
            } else {
                None
            }
        }
    };
    let label = format!("POST /api/v2/sessions/{sid}/commands");
    let run = ctx.call(&label, |c, t| {
        let (sid, req) = (sid.clone(), req.clone());
        async move { c.authed(&t).run(&sid, &req).await }
    });
    let (result, fresh) = tokio::join!(run, peek);
    if let Some(s) = &fresh {
        let _ = remember_session(ctx, s, false);
    }
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            if matches!(e.code, ErrorCode::SessionEnded | ErrorCode::SessionNotFound) {
                forget_session(ctx, &sid);
            }
            return Err(e);
        }
    };
    if result.error.as_ref().is_some_and(|e| e.code == "session_ended") {
        forget_session(ctx, &sid);
    }
    let files = result.files.clone();
    if ctx.out.json {
        // One document at the end, so download first.
        let mut saved: Vec<PathBuf> = Vec::new();
        if let Some(o) = &out {
            for (i, f) in files.iter().enumerate() {
                match save_file(ctx, f, Some(o), i).await {
                    Ok(s) => saved.push(s.path),
                    Err(e) => {
                        let details = json!({"result": result, "saved_to": saved, "file": e.details});
                        return Err(e.details(details));
                    }
                }
            }
        }
        let mut doc = to_json(&result);
        if out.is_some() {
            doc["saved_to"] = json!(saved);
        }
        ctx.emit(doc, String::new);
    } else {
        // What the device said and each file's Briefcase link come first, so they're there even
        // if a download fails.
        let mut text = result.text.clone().unwrap_or_else(|| {
            if result.output.is_null() {
                String::new()
            } else {
                serde_json::to_string_pretty(&result.output).unwrap_or_default()
            }
        });
        for f in &files {
            text.push_str(&format!("\n{}", file_line(f)));
        }
        if result.ok
            && name == "snapshot"
            && !req
                .args
                .iter()
                .take_while(|arg| arg.as_str() != "--")
                .any(|arg| arg == "--raw")
        {
            text.push_str("\nRun --raw to get the entire accessibility tree.");
        }
        if !text.trim().is_empty() {
            outln!("{}", text.trim_end());
        }
        let _ = std::io::stdout().flush();
        for w in &result.warnings {
            ctx.out.warn(w);
        }
        if !result.ok
            && let Some(e) = &result.error
        {
            errln!(
                "{} {} ({})",
                ctx.out.colors.paint(Stream::Err, "1;31", "error:"),
                e.message,
                e.code
            );
        }
        if let Some(o) = &out {
            for (i, f) in files.iter().enumerate() {
                let s = save_file(ctx, f, Some(o), i).await?;
                outln!("Saved {} to {}", f.name, s.path.display());
            }
        }
    }
    Ok(if result.ok { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globals_are_pulled_out_anywhere() {
        let (g, rest) = parse_globals(vec![
            "--session".into(),
            "abc".into(),
            "snapshot".into(),
            "-i".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(g.session.as_deref(), Some("abc"));
        assert!(g.json);
        assert_eq!(rest, vec!["snapshot", "-i"]);
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// What the device would receive for `extend <argv...>`: globals, then the device's arguments.
    fn device_view(argv: &[&str]) -> (Globals, DeviceArgs) {
        let (g, rest) = parse_globals(strings(argv)).unwrap();
        let d = split_device_args(&rest[0], rest[1..].to_vec()).unwrap();
        (g, d)
    }

    #[test]
    fn adb_arguments_reach_the_device_exactly_as_typed() {
        for (argv, device) in [
            (
                &["adb", "shell", "grep", "-v", "error", "/sdcard/app.log"][..],
                &["shell", "grep", "-v", "error", "/sdcard/app.log"][..],
            ),
            (&["adb", "shell", "df", "-h"], &["shell", "df", "-h"]),
            (
                &["adb", "logcat", "-d", "-v", "threadtime"],
                &["logcat", "-d", "-v", "threadtime"],
            ),
            (
                &["adb", "shell", "dumpsys", "battery", "--json"],
                &["shell", "dumpsys", "battery", "--json"],
            ),
            (&["adb", "shell", "dumpsys", "--help"], &["shell", "dumpsys", "--help"]),
            (&["adb", "shell", "ls", "-V"], &["shell", "ls", "-V"]),
            (
                &[
                    "adb",
                    "shell",
                    "perfetto",
                    "--out",
                    "/data/misc/perfetto-traces/t",
                    "--timeout",
                    "5",
                    "--keep",
                ],
                &[
                    "shell",
                    "perfetto",
                    "--out",
                    "/data/misc/perfetto-traces/t",
                    "--timeout",
                    "5",
                    "--keep",
                ],
            ),
            (
                &[
                    "adb",
                    "shell",
                    "tool",
                    "--session",
                    "x",
                    "--team",
                    "y",
                    "--test",
                    "z",
                    "--ttl",
                    "1d",
                    "--session=q",
                ],
                &[
                    "shell",
                    "tool",
                    "--session",
                    "x",
                    "--team",
                    "y",
                    "--test",
                    "z",
                    "--ttl",
                    "1d",
                    "--session=q",
                ],
            ),
            (&["adb", "shell", "--", "ls"], &["shell", "--", "ls"]),
        ] {
            let (g, d) = device_view(argv);
            assert_eq!(d.args, strings(device), "{argv:?}");
            assert!(
                !g.help && !g.verbose && !g.json && !g.version,
                "{argv:?} set a global flag: {g:?}"
            );
            assert!(
                g.timeout.is_none() && g.session.is_none() && g.retired.is_none(),
                "{argv:?}: {g:?}"
            );
            assert_eq!((d.ttl, d.keep, d.out.clone()), (None, false, None), "{argv:?}");
        }
    }

    #[test]
    fn extend_flags_before_the_first_adb_argument_are_extends() {
        let (g, d) = device_view(&["--json", "--session", "a3f", "adb", "shell", "df", "-h"]);
        assert!(g.json);
        assert_eq!(g.session.as_deref(), Some("a3f"));
        assert_eq!(d.args, strings(&["shell", "df", "-h"]));

        let (g, d) = device_view(&[
            "adb",
            "--json",
            "--timeout",
            "60000",
            "--out",
            "./shot.png",
            "--keep",
            "--ttl",
            "7d",
            "-v",
            "exec-out",
            "screencap",
            "-p",
        ]);
        assert!(g.json && g.verbose);
        assert_eq!(g.timeout, Some(60_000));
        assert_eq!(
            (d.ttl, d.keep, d.out.as_deref()),
            (Some(7 * 1440), true, Some("./shot.png"))
        );
        assert_eq!(d.args, strings(&["exec-out", "screencap", "-p"]));

        // A value that looks like a flag or a command is still the flag's value.
        let (_, d) = device_view(&["adb", "--out", "-h", "exec-out", "cat", "/x"]);
        assert_eq!(d.out.as_deref(), Some("-h"));
        assert_eq!(d.args, strings(&["exec-out", "cat", "/x"]));

        let (g, _) = parse_globals(strings(&["adb", "-h"])).unwrap();
        assert!(g.help, "`extend adb -h` is Extend's help for adb");
        assert!(parse_globals(strings(&["adb", "--out"])).is_err());
    }

    #[test]
    fn double_dash_ends_extends_flags() {
        // adb: a leading `--` ends Extend's flags and is not sent.
        let (g, d) = device_view(&["adb", "--", "shell", "grep", "-v", "x"]);
        assert!(!g.verbose);
        assert_eq!(d.args, strings(&["shell", "grep", "-v", "x"]));
        let (g, d) = device_view(&["adb", "--json", "--", "--out", "x", "-h"]);
        assert!(g.json && !g.help);
        assert_eq!(d.out, None);
        assert_eq!(d.args, strings(&["--out", "x", "-h"]));

        // Other device commands: flags anywhere before `--`; `--` and the rest reach the device.
        let (g, d) = device_view(&["type", "--", "-v", "--json"]);
        assert!(!g.verbose && !g.json);
        assert_eq!(d.args, strings(&["--", "-v", "--json"]));
        let (_, d) = device_view(&["terminal", "run", "--", "grep", "-v", "x", "--out", "y"]);
        assert_eq!(d.out, None);
        assert_eq!(d.args, strings(&["run", "--", "grep", "-v", "x", "--out", "y"]));
        let (g, d) = device_view(&["screenshot", "home", "--out", "./s.png", "--json", "--", "--keep"]);
        assert!(g.json);
        assert_eq!((d.out.as_deref(), d.keep), (Some("./s.png"), false));
        assert_eq!(d.args, strings(&["home", "--", "--keep"]));

        // Extend's own commands: everything after `--` is positional.
        let a = Args::parse(&strings(&["7c1e09ab", "--", "--name", "-x"]), "device rename").unwrap();
        assert_eq!(a.pos, strings(&["7c1e09ab", "--name", "-x"]));
        assert!(a.flags.is_empty());
    }

    #[test]
    fn adb_pull_saves_to_the_local_path() {
        for argv in [
            &["adb", "pull", "/sdcard/x.bin", "--out", "./x.bin"][..],
            &["adb", "pull", "/sdcard/x.bin", "./x.bin"],
            &["adb", "--out", "./x.bin", "pull", "/sdcard/x.bin"],
            // `--out` right after `pull`: real adb pull has no --out, so it is never the device's.
            &["adb", "pull", "--out", "./x.bin", "/sdcard/x.bin"],
            &["adb", "--", "pull", "--out", "./x.bin", "/sdcard/x.bin"],
        ] {
            let (_, d) = device_view(argv);
            assert_eq!(d.args, strings(&["pull", "/sdcard/x.bin"]), "{argv:?}");
            assert_eq!(d.out.as_deref(), Some("./x.bin"), "{argv:?}");
        }
        // Options stay with the device.
        let (_, d) = device_view(&["adb", "pull", "-a", "/sdcard/x.bin"]);
        assert_eq!((d.args, d.out), (strings(&["pull", "-a", "/sdcard/x.bin"]), None));
        let (_, d) = device_view(&["adb", "pull", "-a", "--out", "./x.bin", "/sdcard/x.bin"]);
        assert_eq!(
            (d.args, d.out.as_deref()),
            (strings(&["pull", "-a", "/sdcard/x.bin"]), Some("./x.bin"))
        );
        let (_, d) = device_view(&["adb", "pull", "/sdcard/x.bin"]);
        assert_eq!((d.args, d.out), (strings(&["pull", "/sdcard/x.bin"]), None));

        let err = |argv: &[&str]| {
            let (_, rest) = parse_globals(strings(argv)).unwrap();
            split_device_args("adb", rest[1..].to_vec()).unwrap_err()
        };
        for argv in [
            &["adb", "--out", "./a", "pull", "/sdcard/x", "./b"][..],
            &["adb", "pull", "--out", "./a", "/sdcard/x", "./b"],
            &["adb", "pull", "--out", "./a", "/sdcard/x", "--out", "./b"],
        ] {
            let e = err(argv);
            assert!(
                e.message.contains("was given 2 local paths to save to (./a, ./b)"),
                "{argv:?}: {}",
                e.message
            );
            assert!(e.hint.unwrap().contains("extend adb pull <device path> <local path>"));
        }
        for argv in [&["adb", "pull", "/sdcard/x", "--out"][..], &["adb", "pull", "--out"]] {
            assert!(err(argv).message.contains("--out needs a local path"), "{argv:?}");
        }
    }

    #[test]
    fn globals_still_work_anywhere_for_other_commands() {
        let (g, d) = device_view(&["snapshot", "-i", "--json", "--session", "a3f", "-v"]);
        assert!(g.json && g.verbose);
        assert_eq!(g.session.as_deref(), Some("a3f"));
        assert_eq!(d.args, strings(&["-i"]));
    }

    /// A scratch directory for one test.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("extend-cli-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn adb_shell_arguments_are_never_uploaded() {
        let dir = scratch("adb-shell-upload");
        let image = dir.join("a.png");
        std::fs::write(&image, b"png").unwrap();
        let image = image.display().to_string();
        let mut args = strings(&["shell", "some-tool", "--image", &image, "--steps-file", &image]);
        let before = args.clone();
        assert!(attach_local_files("adb", &mut args).unwrap().is_empty());
        assert_eq!(args, before, "adb shell arguments are the device's");

        let mut args = strings(&["show", "--image", &image]);
        let sent = attach_local_files("display", &mut args).unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(args, strings(&["show", "--image", "attachment:a.png"]));

        let mut args = strings(&["push", &image, "/sdcard/a.png"]);
        assert_eq!(attach_local_files("adb", &mut args).unwrap().len(), 1);
        assert_eq!(args, strings(&["push", "attachment:a.png", "/sdcard/a.png"]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_files_are_refused_before_reading() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("oversized");
        let big = dir.join("system.img");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(4 << 30).unwrap(); // sparse: takes no disk space
        drop(f);
        // Unreadable, so any attempt to read it would fail with "Permission denied" instead.
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o000)).unwrap();
        let big = big.display().to_string();

        let mut args = strings(&["push", &big, "/sdcard/system.img"]);
        let e = attach_local_files("adb", &mut args).unwrap_err();
        assert!(
            e.message
                .contains("is 4.0 GiB; a command can carry at most 8 MiB (8388608 bytes) of local files."),
            "{}",
            e.message
        );
        let hint = e.hint.unwrap();
        assert!(
            hint.contains("split -b 8m") && hint.contains("cat /data/local/tmp/system.img.part.*"),
            "{hint}"
        );
        assert!(!hint.contains("pass the link"), "{hint}");

        let mut args = strings(&["com.example.app", &big]);
        let e = attach_local_files("install", &mut args).unwrap_err();
        let hint = e.hint.unwrap();
        assert!(
            hint.contains("can't install from a Briefcase link or file id")
                && hint.contains("pm install -r /data/local/tmp/system.img"),
            "{hint}"
        );

        let mut args = strings(&["show", "--video", &big]);
        assert!(
            attach_local_files("display", &mut args)
                .unwrap_err()
                .hint
                .unwrap()
                .contains("http(s) link")
        );
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn attachments_are_limited_in_total_and_count() {
        let dir = scratch("attachment-limits");
        let mut names = Vec::new();
        for i in 0..9 {
            let p = dir.join(format!("s{i}.ad"));
            std::fs::write(
                &p,
                if i < 2 {
                    vec![b'x'; 5 << 20]
                } else {
                    b"open settings".to_vec()
                },
            )
            .unwrap();
            names.push(p.display().to_string());
        }
        let mut args = vec![names[0].clone(), names[1].clone()];
        let e = attach_local_files("test", &mut args).unwrap_err();
        assert!(
            e.message.contains("would bring this command's local files to 10.0 MiB"),
            "{}",
            e.message
        );

        // One byte over: sizes that would round to the limit are given in bytes.
        let over = dir.join("over.ad");
        std::fs::write(&over, vec![b'x'; (8 << 20) + 1]).unwrap();
        let mut args = vec![over.display().to_string()];
        let e = attach_local_files("replay", &mut args).unwrap_err();
        assert!(
            e.message.ends_with(
                "over.ad is 8388609 bytes; a command can carry at most 8 MiB (8388608 bytes) of local files."
            ),
            "{}",
            e.message
        );
        let exact = dir.join("exact.ad");
        std::fs::write(&exact, vec![b'x'; 8 << 20]).unwrap();
        let mut args = vec![exact.display().to_string()];
        assert_eq!(
            attach_local_files("replay", &mut args).unwrap().len(),
            1,
            "exactly 8 MiB is allowed"
        );

        let mut args: Vec<String> = names[2..9].to_vec();
        assert_eq!(attach_local_files("test", &mut args).unwrap().len(), 7);
        let mut args: Vec<String> = names[0..1].iter().chain(&names[2..9]).cloned().collect();
        assert_eq!(attach_local_files("test", &mut args).unwrap().len(), 8);
        let mut args: Vec<String> = names[0..2]
            .iter()
            .take(1)
            .chain(&names[2..9])
            .chain(&names[2..3])
            .cloned()
            .collect();
        assert!(
            attach_local_files("test", &mut args)
                .unwrap_err()
                .message
                .contains("more than 8 local files")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn telemetry_never_carries_arguments() {
        let step = |argv: &[&str]| telemetry_step(&strings(argv));
        assert_eq!(
            step(&["login", "oac_short_lived_token"]),
            ("login".into(), "cli.login".into())
        );
        assert_eq!(step(&["login", "status"]), ("login".into(), "cli.login.status".into()));
        assert_eq!(step(&["type", "hunter2"]), ("type".into(), "cli.type".into()));
        assert_eq!(
            step(&["type", "start"]),
            ("type".into(), "cli.type".into()),
            "typed text is never reported"
        );
        assert_eq!(
            step(&["device", "pair", "4f9c2a", "--name", "x"]),
            ("device".into(), "cli.device.pair".into())
        );
        assert_eq!(
            step(&["adb", "--out", "/Users/me/private.bin", "pull", "/sdcard/x"]),
            ("adb".into(), "cli.adb".into())
        );
        assert_eq!(step(&["adb", "shell", "id"]), ("adb".into(), "cli.adb.shell".into()));
        assert_eq!(
            step(&["oac_pasted_as_command"]),
            ("unknown".into(), "cli.unknown".into())
        );
    }

    #[test]
    fn install_and_push_refuse_what_is_not_a_local_file() {
        let dir = scratch("not-local");
        let apk = dir.join("app.apk");
        std::fs::write(&apk, b"PK apk").unwrap();
        let apk = apk.display().to_string();
        let missing = dir.join("does-not-exist.apk").display().to_string();
        let folder = dir.display().to_string();
        let refused = |name: &str, items: &[&str]| {
            let mut args = strings(items);
            let e = attach_local_files(name, &mut args).expect_err(&format!("extend {name} {items:?} was sent"));
            (e.message, e.hint.unwrap_or_default())
        };

        let (m, h) = refused("install", &["com.example.app", "0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70"]);
        assert!(
            m.contains("looks like a Briefcase file id") && m.contains("sends the APK from this computer"),
            "{m}"
        );
        assert!(
            h.contains("extend file get 0192f0c4-7b1a-7c3e-9a4d-2b6f1e8c5a70 --out ./app.apk")
                && h.contains("extend install com.example.app ./app.apk"),
            "{h}"
        );

        let (m, h) = refused("install", &["com.example.app", "https://briefcase.example/f/abc"]);
        assert!(
            m.contains("got a link, https://briefcase.example/f/abc")
                && m.contains("can't fetch links or Briefcase files yet"),
            "{m}"
        );
        assert!(
            h.contains("curl -L -o ./app.apk 'https://briefcase.example/f/abc'"),
            "{h}"
        );

        let (m, h) = refused("reinstall", &["com.example.app", &missing]);
        assert!(
            m.starts_with(&format!(
                "There is no file at {missing} on this computer; `extend reinstall` sends the APK"
            )),
            "{m}"
        );
        assert!(h.contains("Check the path"), "{h}");

        let (m, h) = refused("adb", &["install", &missing]);
        assert!(m.contains("`extend adb install` sends the APK"), "{m}");
        assert!(h.contains("extend file get"), "{h}");
        let (m, _) = refused("adb", &["install", "-r", "https://example.com/app.apk"]);
        assert!(m.contains("`extend adb install` got a link"), "{m}");

        let (m, h) = refused("adb", &["push", &missing, "/sdcard/x"]);
        assert!(m.contains("`extend adb push` sends a file from this computer"), "{m}");
        assert!(h.contains("Check the path"), "{h}");
        let (m, h) = refused("adb", &["push", &folder, "/sdcard/x"]);
        assert!(m.contains("is a directory") && h.contains("tar -cf"), "{m} / {h}");
        let (m, _) = refused("install", &["com.example.app", &folder]);
        assert!(m.contains("is a directory"), "{m}");

        let bundle = dir.join("app.aab");
        std::fs::write(&bundle, b"PK aab").unwrap();
        let (m, h) = refused("install", &["com.example.app", &bundle.display().to_string()]);
        assert!(
            m.contains("Android App Bundle (.aab)") && h.contains("bundletool build-apks"),
            "{m} / {h}"
        );

        // Shapes the device can't take are explained here.
        let (m, h) = refused("adb", &["push", &apk, &apk, "/sdcard/"]);
        assert!(m.contains("takes one local file and one device path"), "{m}");
        assert!(h.contains("extend adb push ./photo.jpg"), "{h}");
        let (m, _) = refused("adb", &["push", "--sync", &apk, "/sdcard/"]);
        assert!(m.contains("takes one local file and one device path"), "{m}");
        let (m, h) = refused("adb", &["install", "-t", &apk]);
        assert!(m.contains("takes an optional -r and one local .apk"), "{m}");
        assert!(
            h.contains("extend adb shell pm install <options> /data/local/tmp/app.apk"),
            "{h}"
        );
        let (m, _) = refused("install", &[&apk]);
        assert!(m.contains("takes a package name and a local .apk, and got"), "{m}");

        // What works is still sent.
        let mut args = strings(&["com.example.app", &apk]);
        assert_eq!(attach_local_files("install", &mut args).unwrap().len(), 1);
        assert_eq!(args, strings(&["com.example.app", "attachment:app.apk"]));
        let mut args = strings(&["install", "-r", &apk]);
        assert_eq!(attach_local_files("adb", &mut args).unwrap().len(), 1);
        assert_eq!(args, strings(&["install", "-r", "attachment:app.apk"]));
        // A local file named like the package is not uploaded.
        std::fs::write(dir.join("com.example.app"), b"not an apk").unwrap();
        let mut args = strings(&[&dir.join("com.example.app").display().to_string(), &apk]);
        assert_eq!(attach_local_files("install", &mut args).unwrap().len(), 1);
        assert!(!args[0].starts_with("attachment:"), "{args:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ttl_parsing() {
        assert_eq!(parse_ttl("90m").unwrap(), 90);
        assert_eq!(parse_ttl("2h").unwrap(), 120);
        assert_eq!(parse_ttl("30d").unwrap(), 43_200);
        assert!(parse_ttl("31d").is_err());
        assert!(parse_ttl("0m").is_err());
        assert!(parse_ttl("99999999d").is_err(), "no overflow");
        assert!(parse_ttl("7w").unwrap_err().hint.unwrap().contains("Units are m"));
    }

    #[test]
    fn args_flags() {
        let a = Args::parse(
            &[
                "4f9c2a".into(),
                "--name".into(),
                "Pixel".into(),
                "--access".into(),
                "si:a".into(),
                "--access=si:b".into(),
            ],
            "device pair",
        )
        .unwrap();
        assert_eq!(a.pos, vec!["4f9c2a"]);
        assert_eq!(a.value("--name").as_deref(), Some("Pixel"));
        assert_eq!(a.values("--access"), vec!["si:a", "si:b"]);
    }

    #[test]
    fn settings_are_checked_against_their_ranges() {
        for (k, v) in [
            ("color", "purple"),
            ("screenshot_scale", "7"),
            ("screenshot_scale", "0"),
            ("screenshot_scale", "NaN"),
            ("api_url", "ftp://x"),
            ("api_url", "http://example.com"),
            ("api_url", "http://localhost.evil.com"),
            ("telemetry", "maybe"),
            ("output", "yaml"),
            ("self_destruct", "31d"),
            ("team", "acme"),
            ("accounts_url", "http://accounts.example.com"),
            ("api_url", "http://10.0.2.2:8480"),
            ("download_dir", "/definitely/not/here"),
        ] {
            let e = validate_setting(k, v).expect_err(&format!("{k} = {v} was accepted"));
            assert!(e.hint.is_some(), "{k} = {v}: no hint");
        }
        for (k, v) in [
            ("color", "never"),
            ("screenshot_scale", "0.01"),
            ("screenshot_scale", "1"),
            ("api_url", "https://backend.extend.teamofsilicons.com"),
            ("api_url", "http://127.0.0.1:8480"),
            ("telemetry", "off"),
            ("output", "json"),
            ("self_destruct", "90m"),
            ("accounts_url", "https://accounts.teamofsilicons.com"),
            ("accounts_url", "http://localhost:9590"),
        ] {
            assert!(validate_setting(k, v).is_ok(), "{k} = {v} was refused");
        }
        assert_eq!(
            validate_setting("api_url", "https://x.example/").unwrap(),
            "https://x.example"
        );
        assert!(
            validate_setting("bogus", "1")
                .unwrap_err()
                .hint
                .unwrap()
                .contains("screenshot_scale")
        );
    }

    #[test]
    fn saved_files_stay_inside_the_directory() {
        let f = |name: &str| FileInfo {
            file_id: uuid::Uuid::nil(),
            name: name.into(),
            kind: FileKind::Screenshot,
            content_type: "image/png".into(),
            size_bytes: 1,
            url: "https://briefcase.example/f/1".into(),
            self_destruct_at: None,
            permanent: false,
            session_id: None,
            device_id: None,
            command_id: None,
            created_by: None,
            shared_with: None,
            created_at: None,
            team: None,
            created_by_uuid: None,
            shared_with_uuid: None,
        };
        assert_eq!(safe_name(&f("shot.png")), "shot.png");
        assert_eq!(safe_name(&f("../../etc/passwd")), "passwd");
        assert_eq!(safe_name(&f("..")), uuid::Uuid::nil().to_string());
        assert_eq!(numbered(Path::new("out/shot.png"), 2), PathBuf::from("out/shot-2.png"));
        assert_eq!(numbered(Path::new("shot"), 3), PathBuf::from("shot-3"));
    }
}
