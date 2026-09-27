//! `extend` — find and use the devices a Silicon has access to; pair and manage them as a Carbon.
//!
//! Built only on `silicon-extend-client`. Stateful on disk (see `store.rs`); never asks for a
//! password. Every failure says what went wrong, why, and what to run next, and exits with a code
//! from `understanding/cli.yaml`.

mod args;
mod compat;
mod error;
mod help;
#[macro_use]
mod output;
mod store;

use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use extend_protocol::capability::{self, COMMANDS};
use extend_protocol::model::*;
use extend_protocol::{DeviceId, ErrorCode};
use serde_json::{Value, json};
use silicon_extend_client::attachments::{self as attach, AttachmentError, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS};
use silicon_extend_client::{ActivityQuery, Client, DeviceQuery, ListQuery};

use args::{Args, Globals, VERBATIM_COMMANDS, missing_value, parse_globals};
use error::{CliError, R};
use output::{Colors, Out, Stream};
use store::{Auth, Plane};

const DEFAULT_API: &str = "https://backend.extend.teamofsilicons.com";

// ───────────────────────────── Context ─────────────────────────────

struct Ctx {
    g: Globals,
    plane: Plane,
    cfg: BTreeMap<String, String>,
    client: Option<Client>,
    auth: Option<Auth>,
    test_name: Option<String>,
    out: Out,
    /// The test secret came from `EXTEND_TEST_SECRET` and hasn't been checked against `--test`'s id.
    verify_env_secret: bool,
}

fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

impl Ctx {
    fn api_url(&self) -> String {
        std::env::var("EXTEND_API_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.cfg.get("api_url").cloned())
            .unwrap_or_else(|| DEFAULT_API.into())
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

    fn timed<T>(&self, label: &str, t: Instant, r: Result<T, silicon_extend_client::Error>) -> R<T> {
        match r {
            Ok(v) => {
                self.out.verbose(|| format!("{label}: ok in {} ms", ms(t)));
                Ok(v)
            }
            Err(e) => {
                let e = CliError::from(e);
                self.verbose_failure(label, &e, t);
                Err(e)
            }
        }
    }

    async fn client(&mut self) -> R<Client> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        // A script that sets EXTEND_TEST_SECRET means a test environment; without --test the
        // command would reach production instead, so nothing is sent.
        if !self.plane.is_test() && std::env::var("EXTEND_TEST_SECRET").is_ok_and(|s| !s.trim().is_empty()) {
            return Err(CliError::usage(
                "EXTEND_TEST_SECRET is set, but this command has no --test <test_id>, so it would run in production. Nothing was sent.",
                "Add --test <test_id> (the environment the secret belongs to), or unset EXTEND_TEST_SECRET to use production.",
            ));
        }
        let url = self.api_url();
        let mut b = Client::builder(url.clone())
            .user_agent(concat!("extend-cli/", env!("CARGO_PKG_VERSION")))
            .telemetry(self.telemetry_on())
            .isi(std::env::var("ISI").ok());
        if let Plane::Test { secret, .. } = &self.plane {
            b = b.testing_secret(secret.clone());
        }
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
        if self.verify_env_secret {
            self.check_env_secret(&c).await?;
        }
        self.client = Some(c.clone());
        Ok(c)
    }

    /// `EXTEND_TEST_SECRET` must belong to the environment `--test` names; once it does, that is
    /// remembered (by digest, never the secret) so later runs skip the check.
    async fn check_env_secret(&mut self, c: &Client) -> R<()> {
        let Plane::Test { id, secret } = self.plane.clone() else {
            return Ok(());
        };
        let t = Instant::now();
        let env = self.timed("GET /api/v1/testing-environment", t, c.testing_environment().await)?;
        if uuid::Uuid::parse_str(&id).ok() != Some(env.environment_id) {
            return Err(CliError::new(
                ErrorCode::TestingSecretInvalid,
                format!(
                    "EXTEND_TEST_SECRET belongs to test environment \"{}\" ({}), not {id}, so nothing ran.",
                    env.name, env.environment_id
                ),
            )
            .hint(format!(
                "Run with --test {}, or set EXTEND_TEST_SECRET to the app secret of {id}.",
                env.environment_id
            )));
        }
        let mut saved = store::find_test(&id)?.unwrap_or_default();
        saved.verified_env_secret = Some(extend_protocol::ids::secret_digest(&secret));
        saved.name = Some(env.name.clone());
        store::save_test(&id, &saved)?;
        self.test_name = Some(env.name);
        self.verify_env_secret = false;
        Ok(())
    }

    fn team(&self) -> Option<String> {
        self.g
            .team
            .clone()
            .or_else(|| self.auth.as_ref().and_then(|a| a.team.clone()))
            .or_else(|| self.cfg.get("team").cloned())
    }

    fn require_auth(&self) -> R<Auth> {
        self.auth.clone().ok_or_else(|| {
            let mut e = CliError::new(ErrorCode::NotSignedIn, "You are not signed in to Silicon Extend");
            e.message.push_str(if self.plane.is_test() {
                " in this test environment."
            } else {
                "."
            });
            e.hint("Get a short-lived token from Silicon IAM and run `extend login <slt>`.")
        })
    }

    /// Runs an authenticated call, refreshing the access token once if it expired. `label` names
    /// the call for `-v`.
    async fn call<T, F, Fut>(&mut self, label: &str, f: F) -> R<T>
    where
        F: Fn(Client, String, Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<T, silicon_extend_client::Error>>,
    {
        let client = self.client().await?;
        let auth = self.require_auth()?;
        let team = self.team();
        let t = Instant::now();
        match f(client.clone(), auth.access_token.clone(), team.clone()).await {
            Err(e) if silicon_extend_client::needs_refresh(&e) => {
                self.verbose_failure(label, &CliError::from(e), t);
                let fresh = self.refresh(&client).await?;
                let t = Instant::now();
                let r = f(client, fresh.access_token, team).await;
                self.timed(label, t, r)
            }
            other => self.timed(label, t, other),
        }
    }

    async fn refresh(&mut self, client: &Client) -> R<Auth> {
        let _lock = store::Lock::acquire("refresh");
        // Another process may have refreshed while we waited.
        if let Some(disk) = store::load_auth(&self.plane)
            && self.auth.as_ref().is_some_and(|a| a.access_token != disk.access_token)
        {
            self.auth = Some(disk.clone());
            return Ok(disk);
        }
        let auth = self.require_auth()?;
        let key = format!(
            "refresh-{}",
            &extend_protocol::ids::secret_digest(&auth.refresh_token)[..32]
        );
        let t = Instant::now();
        let s = self
            .timed(
                "POST /api/v1/auth/refresh",
                t,
                client.refresh(&auth.refresh_token, &key).await,
            )
            .map_err(|mut c| {
                c.code = ErrorCode::TokenExpired;
                c.hint = Some("Your login ended. Get a new short-lived token and run `extend login <slt>`.".into());
                c
            })?;
        let fresh = Auth {
            access_token: s.access_token,
            refresh_token: s.refresh_token,
            expires_at: now_s() + s.expires_in,
            teams: if s.teams.is_empty() {
                auth.teams.clone()
            } else {
                s.teams
            },
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
            .or_else(|| std::env::var("EXTEND_SESSION").ok().filter(|s| !s.is_empty()))
            .or_else(|| store::current_session(&self.plane))
            .ok_or_else(|| {
                CliError::new(ErrorCode::NoSession, "No session selected.")
                    .hint("Run `extend session new <device_id> --connect`, or pass --session <session_id>.")
            })
    }

    fn emit(&self, data: Value, text: impl FnOnce() -> String) {
        self.out.emit(data, text);
    }
}

fn now_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
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

/// The last line on stderr whenever a test environment was asked for, even when the command failed
/// before it started.
fn test_trailer(out: &Out, id: &str, name: Option<&str>, who: Option<&str>, nothing_ran: bool) {
    errln!(
        "{}",
        out.colors.paint(
            Stream::Err,
            "2",
            &format!(
                "[test environment: {} ({id}) as {}{}]",
                name.unwrap_or("unknown"),
                who.unwrap_or("not signed in"),
                if nothing_ran { "; nothing ran" } else { "" }
            )
        )
    );
}

async fn run(argv: Vec<String>) -> i32 {
    let cfg = store::load_config();
    let colors = Colors::detect(cfg.get("color").map(String::as_str));
    let json_setting = cfg.get("output").is_some_and(|o| o == "json");
    let (g, rest) = match parse_globals(argv.clone()) {
        Ok(v) => v,
        Err(failed) => {
            let (g, e) = *failed;
            let out = Out {
                json: g.json || json_setting,
                colors,
                verbose: g.verbose,
            };
            let code = out.fail(&e);
            if let Some(id) = g.test.clone().or_else(|| args::find_test_id(&argv)) {
                let name = store::find_test(&id).ok().flatten().and_then(|t| t.name);
                test_trailer(&out, &id, name.as_deref(), None, true);
            }
            return code;
        }
    };
    let out = Out {
        json: g.json || json_setting,
        colors,
        verbose: g.verbose,
    };
    let mut ctx = Ctx {
        g,
        plane: Plane::Production,
        cfg,
        client: None,
        auth: None,
        test_name: None,
        out,
        verify_env_secret: false,
    };
    if let Some(id) = ctx.g.test.clone()
        && let Err(e) = select_test(&mut ctx, &id)
    {
        let code = ctx.out.fail(&e);
        test_trailer(&ctx.out, &id, ctx.test_name.as_deref(), None, true);
        return code;
    }
    ctx.auth = store::load_auth(&ctx.plane);
    let started = Instant::now();
    let result = dispatch(&mut ctx, rest.clone()).await;
    let code = match &result {
        Ok(c) => *c,
        Err(e) => ctx.out.fail(e),
    };
    telemetry(&mut ctx, &rest, &result, started.elapsed()).await;
    ctx.out
        .verbose(|| format!("finished in {} ms with exit code {code}", ms(started)));
    if let Plane::Test { id, .. } = &ctx.plane {
        let who = ctx.auth.as_ref().map(|a| a.member_id.clone());
        test_trailer(&ctx.out, id, ctx.test_name.as_deref(), who.as_deref(), false);
    }
    code
}

/// Chooses the test environment `--test <id>` names: its secret from `EXTEND_TEST_SECRET`, else the
/// one `extend config test add` saved.
fn select_test(ctx: &mut Ctx, id: &str) -> R<()> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(CliError::usage(
            format!("{id:?} is not a test id; test ids are the Honeycomb environment UUID."),
            "Use the environment_id Honeycomb gave you: extend --test 9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c <command>. `extend config test ls` lists the ones added here.",
        ));
    }
    let saved = store::find_test(id)?;
    ctx.test_name = saved.as_ref().and_then(|t| t.name.clone());
    let from_env = std::env::var("EXTEND_TEST_SECRET")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let secret = match (from_env, &saved) {
        (Some(s), _) => {
            if !extend_protocol::ids::is_secret(extend_protocol::ids::APP_SECRET_PREFIX, &s) {
                return Err(CliError::new(
                    ErrorCode::TestingSecretInvalid,
                    "EXTEND_TEST_SECRET is not a test application secret: expected ask_ followed by 43 characters. Nothing ran.",
                )
                .hint("Set it to the environment's app secret from Honeycomb, or unset it to use the one saved with `extend config test add`."));
            }
            let digest = extend_protocol::ids::secret_digest(&s);
            ctx.verify_env_secret =
                saved.as_ref().and_then(|t| t.verified_env_secret.as_deref()) != Some(digest.as_str());
            s
        }
        (None, Some(t)) if !t.secret.is_empty() => t.secret.clone(),
        (None, Some(_)) => {
            return Err(CliError::new(
                ErrorCode::TestingSecretInvalid,
                format!("Test environment {id} was used with EXTEND_TEST_SECRET, which is not set now, and no secret is saved for it. Nothing ran."),
            )
            .hint(format!(
                "Set EXTEND_TEST_SECRET to its app secret again, or save it once: printf %s \"$TEST_APP_SECRET\" | extend config test add {id}"
            )));
        }
        (None, None) => {
            return Err(CliError::new(
                ErrorCode::TestingSecretInvalid,
                format!("Test environment {id} is not added to this CLI, so nothing ran."),
            )
            .hint(format!(
                "Add it once: printf %s \"$TEST_APP_SECRET\" | extend config test add {id}. Or give the secret for this command only: EXTEND_TEST_SECRET=<secret> extend --test {id} <command>."
            )));
        }
    };
    ctx.plane = Plane::Test {
        id: id.to_owned(),
        secret,
    };
    Ok(())
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
    let team = ctx.team();
    let _ = tokio::time::timeout(Duration::from_millis(800), async move {
        client
            .authed(&auth.access_token, team.as_deref())
            .telemetry(event)
            .await
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
    "test",
    "use",
    "silicons",
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
    store::load_session_cache(&ctx.plane, &sid)
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
        "iam" => iam(ctx, &args).await,
        "team" => team(ctx, &args).await,
        "config" => config(ctx, &args).await,
        "device" => device(ctx, &args).await,
        "session" => session(ctx, &args).await,
        "takeover" => takeover(ctx, &args).await,
        "request" => request(ctx, &args).await,
        "file" => file(ctx, &args).await,
        "report" => report(ctx, &args).await,
        "env" => env_cmd(ctx, &args).await,
        "version" => version(ctx, &args).await,
        "docs" => {
            Args::parse(&args, "docs")?.at_most(0)?;
            ctx.emit(
                json!({"repository": help::REPO, "docs": help::DOCS, "website": help::WEBSITE, "crate": help::CRATE, "state": store::root()}),
                || {
                    format!(
                        "Docs      {}\nSource    {}\nWebsite   {}\nRust      {}\nState     {}\nInstall   honeycomb install 'extend'",
                        help::DOCS,
                        help::REPO,
                        help::WEBSITE,
                        help::CRATE,
                        store::root().display()
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
                    format!("`{other}` is an agent-device command Extend doesn't relay."),
                );
                return Err(match repl {
                    Some(r) => e.hint(format!("Use `{r}` instead.")),
                    None => e.hint("Extend leaves out agent-device's tools for app developers (simulators, emulators, React Native, web)."),
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

async fn login(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, "login")?;
    a.at_most(1)?;
    let slt = a.pos.first().cloned().filter(|s| !s.is_empty()).ok_or_else(|| {
        CliError::usage(
            "missing the short-lived token",
            "Usage: extend login <slt>. Generate one with the IAM CLI for app_id `extend` (see `extend iam`).",
        )
    })?;
    let client = ctx.client().await?;
    let t = Instant::now();
    let s = ctx.timed("POST /api/v1/auth/login", t, client.login(&slt).await)?;
    let team = ctx
        .cfg
        .get("team")
        .cloned()
        .filter(|t| s.teams.contains(t))
        .or_else(|| s.teams.first().cloned());
    let auth = Auth {
        api_url: ctx.api_url(),
        access_token: s.access_token.clone(),
        refresh_token: s.refresh_token.clone(),
        expires_at: now_s() + s.expires_in,
        member_id: s.member.id.clone(),
        member_kind: if s.member.kind == MemberKind::Silicon {
            "silicon".into()
        } else {
            "carbon".into()
        },
        teams: s.teams.clone(),
        team: team.clone(),
    };
    store::save_auth(&ctx.plane, Some(&auth))?;
    ctx.auth = Some(auth);
    if let (Plane::Test { id, .. }, Some(env)) = (&ctx.plane, &s.testing_environment) {
        let mut t = store::find_test(id)?.unwrap_or_default();
        t.name = Some(env.name.clone());
        store::save_test(id, &t)?;
        ctx.test_name = Some(env.name.clone());
    }
    let kind = if s.member.kind == MemberKind::Silicon {
        "Silicon"
    } else {
        "Carbon"
    };
    ctx.emit(
        json!({"authenticated": true, "member": s.member, "teams": s.teams, "team": team, "testing_environment": s.testing_environment}),
        || {
            format!(
                "Signed in as {} ({kind}) in teams: {}. Default team: {}.",
                s.member.id,
                s.teams.join(", "),
                team.clone().unwrap_or_default()
            )
        },
    );
    Ok(0)
}

/// Exits 0 whether or not a login works: the check itself succeeded (the Team CLIs' convention).
async fn login_status(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    Args::parse(args, "login status")?.at_most(0)?;
    if ctx.auth.is_none() {
        let reason = if ctx.plane.is_test() {
            "No saved login in this test environment."
        } else {
            "No saved login."
        };
        ctx.emit(json!({"authenticated": false, "reason": reason}), || {
            format!("Not signed in: {reason} Run `extend login <slt>` with a short-lived token from Silicon IAM.")
        });
        return Ok(0);
    }
    match ctx
        .call("GET /api/v1/auth/me", |c, t, team| async move {
            c.authed(&t, team.as_deref()).me().await
        })
        .await
    {
        Ok(me) => {
            let kind = if me.member.kind == MemberKind::Silicon {
                "Silicon"
            } else {
                "Carbon"
            };
            ctx.emit(to_json(&me), || {
                format!(
                    "Authenticated as {} ({kind}) in team {}. Teams: {}.",
                    me.member.id,
                    me.team.clone().unwrap_or_else(|| "(none selected)".into()),
                    me.teams.join(", ")
                )
            });
            Ok(0)
        }
        Err(e)
            if matches!(
                e.code,
                ErrorCode::TokenExpired | ErrorCode::NotSignedIn | ErrorCode::Unauthorized | ErrorCode::SltInvalid
            ) =>
        {
            ctx.emit(
                json!({"authenticated": false, "reason": e.message, "code": e.code.as_str()}),
                || format!("Not signed in: {} Run `extend login <slt>`.", e.message),
            );
            Ok(0)
        }
        Err(e) => Err(e),
    }
}

async fn logout(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    Args::parse(args, "logout")?.at_most(0)?;
    let auth = ctx.require_auth()?;
    let client = ctx.client().await?;
    let t = Instant::now();
    let r = client.logout(&auth.refresh_token, Some(&auth.access_token)).await;
    let _ = ctx.timed("POST /api/v1/auth/logout", t, r);
    store::save_auth(&ctx.plane, None)?;
    store::set_current_session(&ctx.plane, None)?;
    ctx.auth = None;
    ctx.emit(json!({"authenticated": false, "signed_out": auth.member_id}), || {
        format!("Signed out {}.", auth.member_id)
    });
    Ok(0)
}

async fn iam(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    Args::parse(args, "iam")?.at_most(0)?;
    let client = ctx.client().await?;
    let t = Instant::now();
    let info = ctx.timed("GET /api/v1/iam", t, client.iam().await)?;
    ctx.emit(to_json(&info), || {
        format!(
            "app_id   {}\nIAM      {}\nAPI      {}\nWebsite  {}\nDocs     {}",
            info.app_id, info.iam_base_url, info.api_base_url, info.website_url, info.docs_url
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
            match ctx.timed("GET /api/v1/contracts", t, c.contracts().await) {
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

async fn team(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("team {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(unknown_sub("team", &sub));
    }
    let a = Args::parse(rest, &path)?;
    let mut auth = ctx.require_auth()?;
    match sub.as_str() {
        "ls" => {
            a.at_most(0)?;
            let me = ctx
                .call(
                    "GET /api/v1/auth/me",
                    |c, t, _| async move { c.authed(&t, None).me().await },
                )
                .await?;
            let default = ctx.team();
            ctx.emit(json!({"teams": me.teams, "default": default}), || {
                me.teams
                    .iter()
                    .map(|t| {
                        if Some(t) == default.as_ref() {
                            format!("{t} (default)")
                        } else {
                            t.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        "silicons" => {
            a.at_most(0)?;
            let list = ctx
                .call("GET /api/v1/team/silicons", |c, t, team| async move {
                    c.authed(&t, team.as_deref()).team_silicons().await
                })
                .await?;
            ctx.emit(json!({"items": list}), || {
                if list.is_empty() {
                    "No Silicons in this team.".into()
                } else {
                    list.iter()
                        .map(|s| match &s.display_name {
                            Some(n) => format!("{}  {n}", s.id),
                            None => s.id.clone(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            });
        }
        _ => {
            a.at_most(1)?;
            let t = a.req(0, "team handle")?;
            if !auth.teams.contains(&t) {
                return Err(CliError::new(
                    ErrorCode::NotATeamMember,
                    format!("This login doesn't reach team {t:?}."),
                )
                .hint(format!(
                    "Teams this login reaches: {}. Sign in as a member of {t} to use it.",
                    auth.teams.join(", ")
                )));
            }
            auth.team = Some(t.clone());
            store::save_auth(&ctx.plane, Some(&auth))?;
            ctx.auth = Some(auth);
            ctx.emit(json!({"default": t}), || format!("Default team is now {t}."));
        }
    }
    Ok(0)
}

// ───────────────────────────── Settings ─────────────────────────────

fn unknown_setting(k: &str) -> CliError {
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
        "api_url" => {
            let local = [
                "http://127.0.0.1",
                "http://localhost",
                "http://[::1]",
                "http://10.0.2.2",
            ];
            let is_local = local.iter().any(|p| {
                v.strip_prefix(p)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
            });
            let https = v
                .strip_prefix("https://")
                .is_some_and(|h| !h.is_empty() && !h.starts_with('/'));
            if https || is_local {
                Ok(v.trim_end_matches('/').to_owned())
            } else {
                Err(bad(
                    "it must be an https URL; plain http is allowed only for a local address (127.0.0.1, localhost, [::1], 10.0.2.2)",
                    "Example: extend config set api_url https://backend.extend.teamofsilicons.com",
                ))
            }
        }
        "telemetry" => one_of(&["on", "off"]),
        "output" => one_of(&["text", "json"]),
        "color" => one_of(&["auto", "always", "never"]),
        "team" => {
            if !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c)) {
                Ok(v.to_owned())
            } else {
                Err(bad(
                    "a team handle is 1–64 letters, digits, '-', '_', '.' or ':'",
                    "`extend team ls` lists the teams this login reaches; `extend team use <handle>` sets the default for this login.",
                ))
            }
        }
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
    if sub == "test" {
        return config_test(ctx, rest).await;
    }
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
    let login = store::load_auth(&Plane::Production).map(|a| a.member_id);
    let tests = store::list_tests().len();
    let settings = ctx.cfg.len();
    let copied = store::copy_state(&old_root, &new_root)?;
    if let Err(e) = store::point_to(&new_root) {
        let _ = store::remove_state(&new_root, &copied);
        return Err(e.into());
    }
    let left_behind = store::remove_state(&old_root, &copied).err();
    let mut moved = Vec::new();
    if let Some(m) = &login {
        moved.push(format!("the login for {m}"));
    }
    if settings > 0 {
        moved.push(format!("{settings} setting(s)"));
    }
    if tests > 0 {
        moved.push(format!("{tests} test environment(s)"));
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

async fn config_test(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let (sub, rest) = sub_and_rest(args, "ls");
    let path = format!("config test {sub}");
    if !args::SPECS.iter().any(|s| s.path == path) {
        return Err(CliError::new(
            ErrorCode::UnknownCommand,
            format!("`extend config test {sub}` is not a command."),
        )
        .hint("`extend config test` has: add <test_id> (secret on stdin), ls, rm <test_id>."));
    }
    let a = Args::parse(rest, &path)?;
    match sub.as_str() {
        "add" => {
            a.at_most(1)?;
            let id = a.req(0, "test id")?;
            let Ok(uuid) = uuid::Uuid::parse_str(&id) else {
                return Err(CliError::usage(
                    format!("{id:?} is not a test id; test ids are the Honeycomb environment UUID."),
                    "Use the environment_id Honeycomb gave you: printf %s \"$SECRET\" | extend config test add 9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c",
                ));
            };
            let mut secret = String::new();
            if std::io::stdin().is_terminal() {
                errout!("Paste the test application secret (ask_...): ");
            }
            std::io::stdin().read_to_string(&mut secret).map_err(|e| {
                CliError::usage(
                    format!("Could not read the secret from stdin: {e}"),
                    "Pipe it in: printf %s \"$TEST_APP_SECRET\" | extend config test add <test_id>",
                )
            })?;
            let secret = secret.trim().to_owned();
            if !extend_protocol::ids::is_secret(extend_protocol::ids::APP_SECRET_PREFIX, &secret) {
                return Err(CliError::new(
                    ErrorCode::TestingSecretInvalid,
                    "That is not a test application secret: expected ask_ followed by 43 characters.",
                )
                .hint("Pipe in the environment's app secret from Honeycomb: printf %s \"$TEST_APP_SECRET\" | extend config test add <test_id>"));
            }
            let client = Client::builder(ctx.api_url())
                .testing_secret(secret.clone())
                .connect()
                .await?;
            let t = Instant::now();
            let env = ctx.timed("GET /api/v1/testing-environment", t, client.testing_environment().await)?;
            if env.environment_id != uuid {
                return Err(CliError::new(
                    ErrorCode::TestingSecretInvalid,
                    format!(
                        "That secret belongs to test environment \"{}\" ({}), not {id}, so it was not saved.",
                        env.name, env.environment_id
                    ),
                )
                .hint(format!(
                    "Add it under its own id (extend config test add {}), or pipe in the secret of {id}.",
                    env.environment_id
                )));
            }
            let mut saved = store::find_test(&id)?.unwrap_or_default();
            saved.secret = secret;
            saved.name = Some(env.name.clone());
            store::save_test(&id, &saved)?;
            ctx.emit(json!({"test_id": id, "environment": env}), || {
                format!(
                    "Added test environment {:?} ({id}). Use: extend --test {id} <command>",
                    env.name
                )
            });
        }
        "ls" => {
            a.at_most(0)?;
            let list = store::list_tests();
            ctx.emit(
                json!({"items": list.iter().map(|(id, e)| json!({"test_id": id, "name": e.name, "secret_saved": !e.secret.is_empty(), "signed_in_as": e.auth.as_ref().map(|a| a.member_id.clone())})).collect::<Vec<_>>()}),
                || {
                    if list.is_empty() {
                        "No test environments added. Add one with `extend config test add <test_id>` (secret on stdin).".into()
                    } else {
                        list.iter()
                            .map(|(id, e)| {
                                format!(
                                    "{id}  {}  {}{}",
                                    e.name.clone().unwrap_or_default(),
                                    e.auth
                                        .as_ref()
                                        .map(|a| a.member_id.clone())
                                        .unwrap_or_else(|| "not signed in".into()),
                                    if e.secret.is_empty() {
                                        "  (secret from EXTEND_TEST_SECRET)"
                                    } else {
                                        ""
                                    }
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                },
            );
        }
        _ => {
            a.at_most(1)?;
            let id = a.req(0, "test id")?;
            let removed = store::remove_test(&id);
            ctx.emit(json!({"removed": id, "was_added": removed}), || {
                if removed {
                    format!("Removed test environment {id} from this CLI.")
                } else {
                    format!("Test environment {id} was not added here; nothing to remove.")
                }
            });
        }
    }
    Ok(0)
}

async fn env_cmd(ctx: &mut Ctx, args: &[String]) -> R<i32> {
    let a = Args::parse(args, "env")?;
    a.at_most(1)?;
    if a.pos.first().is_some_and(|s| s != "show") {
        return Err(
            unknown_sub("env", &a.pos[0]).hint("`extend env` has: show. Usage: extend --test <test_id> env show")
        );
    }
    if !ctx.plane.is_test() {
        return Err(CliError::new(
            ErrorCode::TestOnly,
            "This action is only possible in a test environment.",
        )
        .hint("Add --test <test_id>, e.g. `extend --test <test_id> env show`."));
    }
    let client = ctx.client().await?;
    let t = Instant::now();
    let env = ctx.timed("GET /api/v1/testing-environment", t, client.testing_environment().await)?;
    let who = ctx.auth.as_ref().map(|a| a.member_id.clone());
    ctx.emit(json!({"environment": env, "signed_in_as": who}), || {
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

fn device_line(d: &Device, removed_column: bool) -> Vec<String> {
    let in_use = d.in_use.as_ref().map_or("—".to_owned(), |u| {
        format!("{} ({}, {})", u.silicon_id, u.session_id, ago(&u.since))
    });
    let mut row = vec![
        d.device_id.to_string(),
        d.name.clone(),
        d.os.as_str().to_owned(),
        if d.removed_at.is_some() {
            "removed".into()
        } else if d.online {
            "yes".into()
        } else {
            "no".into()
        },
        in_use,
        d.days_left.map_or("—".into(), |x| x.to_string()),
    ];
    if removed_column {
        row.push(d.removed_at.map_or("—".into(), |t| {
            format!(
                "{} ({})",
                fmt_time(&t),
                d.removed_reason.map_or("unknown", EndReason::as_str)
            )
        }));
    }
    row
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
            .call("GET /api/v1/devices", |c, t, team| {
                let q = q.clone();
                async move {
                    let a = c.authed(&t, team.as_deref());
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
            if removed && a.flag("--team-visible") {
                return Err(CliError::usage(
                    "--removed lists your own removed devices, so it can't be combined with --team-visible",
                    "Run them separately: `extend device ls --removed`, and `extend device ls --team-visible`.",
                ));
            }
            let online = a.flag("--online");
            let q = DeviceQuery {
                scope: a.flag("--team-visible").then(|| "team".to_owned()),
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
            ctx.emit(json!({"items": items, "next_cursor": stopped_at}), || {
                if items.is_empty() {
                    return if online || a.value("--os").is_some() {
                        "No devices match. Drop --online or --os to see all of them.".into()
                    } else {
                        "No devices. A Carbon pairs one at extend.teamofsilicons.com or with `extend device pair <code> --name <name>`; a Silicon needs its Carbon to grant access.".into()
                    };
                }
                let mut head = vec![
                    "ID".into(),
                    "NAME".into(),
                    "OS".into(),
                    "ONLINE".into(),
                    "IN USE BY".into(),
                    "DAYS LEFT".into(),
                ];
                if removed {
                    head.push("REMOVED".into());
                }
                let mut rows = vec![head];
                rows.extend(items.iter().map(|d| device_line(d, removed)));
                let mut s = table(rows);
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
                .call(&format!("GET /api/v1/devices/{id}"), |c, t, team| {
                    let id = id.clone();
                    async move { c.authed(&t, team.as_deref()).device(&id).await }
                })
                .await?;
            ctx.emit(to_json(&d), || device_text(&d));
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
            let visibility = match a.value("--visibility").as_deref() {
                None => None,
                Some("team") => Some(Visibility::Team),
                Some("personal") => Some(Visibility::Personal),
                Some(o) => {
                    return Err(CliError::usage(
                        format!("--visibility is team or personal, got {o:?}"),
                        "team (the default) lets other Carbons in the team see it exists; personal hides it.",
                    ));
                }
            };
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
                visibility,
                pair_ttl_days: ttl,
                silicon_ids: a.values("--access"),
            };
            let d = ctx
                .call("POST /api/v1/pairings", |c, t, team| {
                    let claim = claim.clone();
                    async move { c.authed(&t, team.as_deref()).pair(&claim).await }
                })
                .await?;
            ctx.emit(to_json(&d), || {
                format!(
                    "Paired {} \"{}\" ({}). Next: finish the device's own setup — watch it with `extend device setup {} --watch`.",
                    d.device_id,
                    d.name,
                    d.os.as_str(),
                    d.device_id
                )
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
                .call(&format!("POST /api/v1/devices/{host}/attachments"), |c, t, team| {
                    let (host, input) = (host.clone(), input.clone());
                    async move { c.authed(&t, team.as_deref()).attach(&host, &input).await }
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
            loop {
                let s = ctx
                    .call(&format!("GET /api/v1/devices/{id}/setup"), |c, t, team| {
                        let id = id.clone();
                        async move { c.authed(&t, team.as_deref()).setup(&id).await }
                    })
                    .await?;
                let done = s.state == SetupState::Complete;
                if !a.flag("--watch") || done || ctx.out.json {
                    let colors = ctx.out.colors;
                    ctx.emit(to_json(&s), || setup_text(&s, colors));
                    break;
                }
                out!(
                    "\x1b[2J\x1b[H{}\n(watching; Ctrl-C to stop)\n",
                    setup_text(&s, ctx.out.colors)
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
                .call(&format!("POST /api/v1/devices/{id}/setup/code"), |c, t, team| {
                    let (id, code) = (id.clone(), code.clone());
                    async move { c.authed(&t, team.as_deref()).setup_code(&id, &code).await }
                })
                .await?;
            let colors = ctx.out.colors;
            ctx.emit(to_json(&s), || format!("Code sent.\n{}", setup_text(&s, colors)));
        }
        "rename" | "visibility" | "ttl" => {
            a.at_most(2)?;
            let id = a.req(0, "device id")?;
            let v = a.req(1, if sub == "rename" { "name" } else { "value" })?;
            let patch = match sub.as_str() {
                "rename" => DevicePatch {
                    name: Some(v),
                    ..Default::default()
                },
                "visibility" => DevicePatch {
                    visibility: Some(match v.as_str() {
                        "team" => Visibility::Team,
                        "personal" => Visibility::Personal,
                        o => {
                            return Err(CliError::usage(
                                format!("visibility is team or personal, got {o:?}"),
                                format!("extend device visibility {id} team"),
                            ));
                        }
                    }),
                    ..Default::default()
                },
                _ => DevicePatch {
                    pair_ttl_days: Some(v.parse().map_err(|_| {
                        CliError::usage(
                            format!("ttl takes a number of days, 1–30, got {v:?}"),
                            format!("extend device ttl {id} 14"),
                        )
                    })?),
                    ..Default::default()
                },
            };
            let d = ctx
                .call(&format!("PATCH /api/v1/devices/{id}"), |c, t, team| {
                    let (id, patch) = (id.clone(), patch.clone());
                    async move { c.authed(&t, team.as_deref()).update_device(&id, None, &patch).await }
                })
                .await?;
            ctx.emit(to_json(&d), || match sub.as_str() {
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
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let s = ctx
                .call(&format!("POST /api/v1/devices/{id}/stop"), |c, t, team| {
                    let id = id.clone();
                    async move { c.authed(&t, team.as_deref()).stop_device(&id).await }
                })
                .await?;
            ctx.emit(to_json(&s), || {
                format!("Stopped {} (session {}).", s.silicon_id, s.session_id)
            });
        }
        "rm" => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let d = ctx
                .call(&format!("GET /api/v1/devices/{id}"), |c, t, team| {
                    let id = id.clone();
                    async move { c.authed(&t, team.as_deref()).device(&id).await }
                })
                .await?;
            if !a.flag("--yes") {
                return Err(CliError::new(
                    ErrorCode::ConfirmationRequired,
                    format!(
                        "This removes \"{}\" ({id}): it ends {}, removes access for {} Silicon(s), and unpairs it.",
                        d.name,
                        d.in_use.as_ref().map_or("no running session".to_owned(), |u| format!(
                            "{}'s session {}",
                            u.silicon_id, u.session_id
                        )),
                        d.access_count.unwrap_or(0)
                    ),
                )
                .hint(format!("Run `extend device rm {id} --yes` to confirm.")));
            }
            let version = d.version;
            ctx.call(&format!("DELETE /api/v1/devices/{id}"), |c, t, team| {
                let id = id.clone();
                async move { c.authed(&t, team.as_deref()).remove_device(&id, version).await }
            })
            .await?;
            ctx.emit(json!({"removed": id}), || {
                format!(
                    "Removed {id} (\"{}\"). Its activity stays readable: extend device activity {id}",
                    d.name
                )
            });
        }
        "access" => {
            let op = a.pos.first().cloned().unwrap_or_else(|| "ls".into());
            if !["ls", "grant", "revoke"].contains(&op.as_str()) {
                return Err(CliError::usage(
                    format!("unknown `extend device access {op}`"),
                    "Usage: extend device access ls <device_id> | grant <device_id> <silicon_id>... | revoke <device_id> <silicon_id>...",
                ));
            }
            let id = a.req(1, "device id")?;
            if op == "ls" {
                a.at_most(2)?;
                let list = ctx
                    .call(&format!("GET /api/v1/devices/{id}/access"), |c, t, team| {
                        let id = id.clone();
                        async move { c.authed(&t, team.as_deref()).access(&id).await }
                    })
                    .await?;
                ctx.emit(json!({"items": list}), || {
                    if list.is_empty() {
                        return format!(
                            "No Silicon has access yet. Grant it with `extend device access grant {id} <silicon_id>`."
                        );
                    }
                    let mut rows = vec![vec![
                        "SILICON".into(),
                        "GRANTED BY".into(),
                        "GRANTED".into(),
                        "LAST USED".into(),
                    ]];
                    rows.extend(list.iter().map(|g| {
                        vec![
                            g.silicon_id.clone(),
                            g.granted_by.clone(),
                            fmt_time(&g.granted_at),
                            g.last_used_at.map(|t| fmt_time(&t)).unwrap_or_else(|| "—".into()),
                        ]
                    }));
                    table(rows)
                });
            } else {
                let silicons: Vec<String> = a.pos[2..].to_vec();
                if silicons.is_empty() {
                    return Err(CliError::usage(
                        "name at least one Silicon",
                        format!("extend device access {op} {id} si:chef (see `extend team silicons`)"),
                    ));
                }
                for s in &silicons {
                    let label = format!(
                        "{} /api/v1/devices/{id}/access/{s}",
                        if op == "grant" { "PUT" } else { "DELETE" }
                    );
                    if op == "grant" {
                        ctx.call(&label, |c, t, team| {
                            let (id, s) = (id.clone(), s.clone());
                            async move { c.authed(&t, team.as_deref()).grant(&id, &s).await }
                        })
                        .await?;
                    } else {
                        ctx.call(&label, |c, t, team| {
                            let (id, s) = (id.clone(), s.clone());
                            async move { c.authed(&t, team.as_deref()).revoke(&id, &s).await }
                        })
                        .await?;
                    }
                }
                ctx.emit(json!({"device_id": id, op.clone(): silicons}), || {
                    if op == "grant" {
                        format!("Granted {} access to {id}.", silicons.join(", "))
                    } else {
                        format!(
                            "Revoked access for {} on {id}; any running session of theirs there has ended.",
                            silicons.join(", ")
                        )
                    }
                });
            }
        }
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
                .call(&format!("GET /api/v1/devices/{id}/activity"), |c, t, team| {
                    let (id, q) = (id.clone(), q.clone());
                    async move { c.authed(&t, team.as_deref()).activity(&id, q).await }
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
        _ => {
            a.at_most(1)?;
            let id = a.req(0, "device id")?;
            let page = ctx
                .call(&format!("GET /api/v1/devices/{id}/requests"), |c, t, team| {
                    let id = id.clone();
                    async move {
                        c.authed(&t, team.as_deref())
                            .device_requests(&id, ListQuery::default())
                            .await
                    }
                })
                .await?;
            ctx.emit(to_json(&page), || requests_table(&page.items));
        }
    }
    Ok(0)
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

fn device_text(d: &Device) -> String {
    let mut s = format!(
        "{} ({})\n  OS:        {}{}\n  Owner:     {}\n",
        d.name,
        d.device_id,
        d.os.as_str(),
        d.os_version.as_ref().map(|v| format!(" {v}")).unwrap_or_default(),
        d.owner.id,
    );
    if let Some(at) = &d.removed_at {
        s.push_str(&format!(
            "  Removed:   {} — {}. Nothing works on it any more; its activity stays readable with `extend device activity {}`.\n",
            fmt_time(at),
            d.removed_reason.map_or("reason unknown", EndReason::explain),
            d.device_id
        ));
    } else {
        s.push_str(&format!(
            "  Online:    {}\n  In use:    {}\n  Pairing:   {} day(s) left of {}\n",
            if d.online { "yes" } else { "no" },
            d.in_use.as_ref().map_or("no".to_owned(), |u| format!(
                "{} in session {} since {}{}",
                u.silicon_id,
                u.session_id,
                fmt_time(&u.since),
                if u.paused { " (paused for takeover)" } else { "" }
            )),
            d.days_left.unwrap_or(0),
            d.pair_ttl_days.unwrap_or(0),
        ));
    }
    if let Some(h) = &d.host_device_id {
        s.push_str(&format!("  Through:   {h}\n"));
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

fn setup_text(s: &Setup, colors: Colors) -> String {
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
    }
    out
}

fn requests_table(items: &[RequestInfo]) -> String {
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
            r.to.clone(),
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

// ───────────────────────────── Sessions ─────────────────────────────

/// Keeps what `extend --help` lists in step with the session as Extend last described it: saved
/// for the connected session (or one already cached, or `connect`), and forgotten once it ended.
fn remember_session(ctx: &Ctx, s: &Session, connect: bool) -> R<()> {
    let sid = s.session_id.to_string();
    if s.state == SessionState::Ended {
        forget_session(ctx, &sid);
        return Ok(());
    }
    let current = store::current_session(&ctx.plane);
    let cached = store::load_session_cache(&ctx.plane, &sid).is_some();
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
        test_id: match &ctx.plane {
            Plane::Test { id, .. } => Some(id.clone()),
            Plane::Production => None,
        },
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
    store::save_session_cache(&ctx.plane, &cache)?;
    if connect {
        store::set_current_session(&ctx.plane, Some(&sid))?;
    }
    Ok(())
}

/// The session ended or is gone: stop listing its device's commands, and disconnect from it.
fn forget_session(ctx: &Ctx, sid: &str) {
    store::remove_session_cache(&ctx.plane, sid);
    if store::current_session(&ctx.plane).as_deref() == Some(sid) {
        let _ = store::set_current_session(&ctx.plane, None);
    }
}

async fn read_session(ctx: &mut Ctx, id: &str) -> R<Session> {
    ctx.call(&format!("GET /api/v1/sessions/{id}"), |c, t, team| {
        let id = id.to_owned();
        async move { c.authed(&t, team.as_deref()).session(&id).await }
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
        .hint(format!("Start a new one: extend session new {}", s.device_id)));
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
                .call("POST /api/v1/sessions", |c, t, team| {
                    let did = did.clone();
                    async move { c.authed(&t, team.as_deref()).start_session(&did).await }
                })
                .await?;
            let sid = s.session_id.to_string();
            if a.flag("--connect") {
                connect_session(ctx, &sid).await?;
            }
            if ctx.out.json {
                ctx.emit(to_json(&s), String::new);
            } else {
                outln!("{sid}");
                errln!(
                    "Started session {sid} on {id}.{} It ends after 5 minutes without a command; end it with `extend session end {sid}`.",
                    if a.flag("--connect") {
                        " Connected — try `extend snapshot -i`.".to_owned()
                    } else {
                        format!(" Next: extend session connect {sid}.")
                    }
                );
            }
        }
        "connect" => {
            a.at_most(1)?;
            let id = a.req(0, "session id")?;
            let s = connect_session(ctx, &id).await?;
            let d = s.device.clone();
            ctx.emit(to_json(&s), || {
                format!(
                    "Connected to {id} on {} ({}). `extend --help` now lists only what works there. Try: extend snapshot -i",
                    d.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
                    d.as_ref().map(|d| d.os.as_str()).unwrap_or_default()
                )
            });
        }
        "disconnect" => {
            a.at_most(0)?;
            store::set_current_session(&ctx.plane, None)?;
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
            let q = ListQuery {
                device_id: a.value("--device"),
                state: a.value("--state"),
                limit: Some(50),
                ..Default::default()
            };
            let page = ctx
                .call("GET /api/v1/sessions", |c, t, team| {
                    let q = q.clone();
                    async move { c.authed(&t, team.as_deref()).sessions(q).await }
                })
                .await?;
            ctx.emit(to_json(&page), || {
                if page.items.is_empty() {
                    return "No sessions.".into();
                }
                let mut rows = vec![vec![
                    "SESSION".into(),
                    "DEVICE".into(),
                    "SILICON".into(),
                    "STATE".into(),
                    "STARTED".into(),
                    "COMMANDS".into(),
                    "ENDED BECAUSE".into(),
                ]];
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
        _ => {
            a.at_most(1)?;
            let id = a.pos.first().cloned().map_or_else(|| ctx.session_id(), Ok)?;
            let s = ctx
                .call(&format!("POST /api/v1/sessions/{id}/end"), |c, t, team| {
                    let id = id.clone();
                    async move { c.authed(&t, team.as_deref()).end_session(&id).await }
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
                    "Tell the Carbon what to do: extend takeover --reason \"Please approve the Face ID prompt\"",
                )
            })?;
            let t = ctx
                .call(&format!("POST /api/v1/sessions/{sid}/takeover"), |c, tok, team| {
                    let (sid, reason) = (sid.clone(), reason.clone());
                    async move { c.authed(&tok, team.as_deref()).takeover(&sid, &reason).await }
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
                .call(&format!("GET /api/v1/sessions/{sid}/takeover"), |c, tok, team| {
                    let sid = sid.clone();
                    async move { c.authed(&tok, team.as_deref()).takeover_status(&sid).await }
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
            ctx.call(&format!("DELETE /api/v1/sessions/{sid}/takeover"), |c, tok, team| {
                let sid = sid.clone();
                async move { c.authed(&tok, team.as_deref()).release_takeover(&sid).await }
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
                    format!("Say why you need it: extend request send {id} --reason \"Need 2 minutes to read an OTP\""),
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
                .call(&format!("POST /api/v1/devices/{id}/requests"), |c, t, team| {
                    let (id, reason) = (id.clone(), reason.clone());
                    async move { c.authed(&t, team.as_deref()).send_request(&id, &reason).await }
                })
                .await?;
            ctx.emit(to_json(&r), || {
                let mut s = format!(
                    "Sent to {} (using {}{}). Delivery: {}.",
                    r.to,
                    r.device_id,
                    r.session_id
                        .as_ref()
                        .map(|s| format!(" in session {s}"))
                        .unwrap_or_default(),
                    format!("{:?}", r.delivery).to_lowercase()
                );
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
                limit: Some(50),
                ..Default::default()
            };
            let page = ctx
                .call("GET /api/v1/requests", |c, t, team| {
                    let q = q.clone();
                    async move { c.authed(&t, team.as_deref()).requests(q).await }
                })
                .await?;
            ctx.emit(to_json(&page), || requests_table(&page.items));
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
                limit: Some(100),
                ..Default::default()
            };
            let page = ctx
                .call("GET /api/v1/files", |c, t, team| {
                    let q = q.clone();
                    async move { c.authed(&t, team.as_deref()).files(q).await }
                })
                .await?;
            ctx.emit(to_json(&page), || {
                if page.items.is_empty() {
                    return "No files.".into();
                }
                let mut rows = vec![vec![
                    "FILE".into(),
                    "KIND".into(),
                    "SIZE".into(),
                    "SELF-DESTRUCTS".into(),
                    "LINK".into(),
                ]];
                rows.extend(page.items.iter().map(|f| {
                    vec![
                        f.file_id.to_string(),
                        f.kind.as_str().into(),
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
                ctx.call(&format!("POST /api/v1/files/{id}/keep"), |c, t, team| {
                    let id = id.clone();
                    async move { c.authed(&t, team.as_deref()).keep_file(&id).await }
                })
                .await?
            } else {
                ctx.call(&format!("GET /api/v1/files/{id}"), |c, t, team| {
                    let id = id.clone();
                    async move { c.authed(&t, team.as_deref()).file(&id).await }
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

/// Downloads a file through Extend (`GET /api/v1/files/{file_id}/content`), which reads it from
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
        .call(&format!("GET /api/v1/files/{id}/content"), |c, tok, team| {
            let id = id.clone();
            async move { c.authed(&tok, team.as_deref()).file_download(&id, None).await }
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
        "session": store::current_session(&ctx.plane),
        "test": ctx.plane.is_test(),
    });
    let input = ReportInput {
        message,
        pr: a.value("--pr"),
        client_version: format!("extend-cli {}", env!("CARGO_PKG_VERSION")),
        context,
    };
    let r = ctx
        .call("POST /api/v1/reports", |c, t, team| {
            let input = input.clone();
            async move { c.authed(&t, team.as_deref()).report(&input).await }
        })
        .await?;
    let pr = input.pr.is_some();
    ctx.emit(to_json(&r), || {
        let mut s = format!(
            "Report {} sent to the Extend team (email {}).",
            r.report_id, r.notification
        );
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
    let watch = store::current_session(&ctx.plane).as_deref() == Some(sid.as_str())
        || store::load_session_cache(&ctx.plane, &sid).is_some();
    let peek = {
        let client = ctx.client().await?;
        let token = ctx.require_auth()?.access_token;
        let (team, sid) = (ctx.team(), sid.clone());
        async move {
            if watch {
                client.authed(&token, team.as_deref()).session(&sid).await.ok()
            } else {
                None
            }
        }
    };
    let label = format!("POST /api/v1/sessions/{sid}/commands");
    let run = ctx.call(&label, |c, t, team| {
        let (sid, req) = (sid.clone(), req.clone());
        async move { c.authed(&t, team.as_deref()).run(&sid, &req).await }
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
            "--test".into(),
            "abc".into(),
            "snapshot".into(),
            "-i".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(g.test.as_deref(), Some("abc"));
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
                g.timeout.is_none() && g.session.is_none() && g.team.is_none() && g.test.is_none(),
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
            ("team", "has space"),
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
            ("team", "acme"),
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
        };
        assert_eq!(safe_name(&f("shot.png")), "shot.png");
        assert_eq!(safe_name(&f("../../etc/passwd")), "passwd");
        assert_eq!(safe_name(&f("..")), uuid::Uuid::nil().to_string());
        assert_eq!(numbered(Path::new("out/shot.png"), 2), PathBuf::from("out/shot-2.png"));
        assert_eq!(numbered(Path::new("shot"), 3), PathBuf::from("shot-3"));
    }
}
