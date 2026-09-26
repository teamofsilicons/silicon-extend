//! `bridge-agent`: the Silicon Bridge app for Mac, Windows and Linux.

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use bridge_agent::agent::{Agent, AgentDeps, AgentHandle};
use bridge_agent::config::{Config, CredentialStoreKind, Overrides};
use bridge_agent::credential::{self, load_for};
use bridge_agent::drivers::local::LocalDriver;
use bridge_agent::hosted::DriverFactory;
use bridge_agent::status::{self, AgentStatus, Phase};
use bridge_driver::{Driver, Invocation};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bridge-agent", version, about = "Silicon Bridge for Mac, Windows and Linux: lets the Silicons a Carbon chooses use this computer.")]
struct Cli {
    /// Bridge service URL (default https://backend.bridge.teamofsilicons.com; env BRIDGE_API_URL).
    #[arg(long, global = true)]
    service_url: Option<String>,
    /// Where the device credential is kept: auto, keyring or file (env BRIDGE_AGENT_CREDENTIAL_STORE).
    #[arg(long, global = true)]
    credential_store: Option<CredentialStoreKind>,
    /// Base directory; state lives in <home>/.bridge-agent (env SILICON_HOME, default ~).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// More detail in the log.
    #[arg(long, short, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the app (the default): pair, stay connected, carry out commands.
    Run {
        /// No tray icon or windows; status goes to stdout. For servers and CI.
        #[arg(long)]
        headless: bool,
    },
    /// Show what the running app is doing.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Show what this computer can do right now (permissions, helpers, capabilities).
    Probe {
        #[arg(long)]
        json: bool,
    },
    /// Stop the Silicon using this computer.
    Stop,
    /// Revoke pair: remove this computer from its Carbon's account and end every Silicon's access.
    Revoke {
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Start Silicon Bridge when you log in.
    InstallAutostart {
        /// Start without the tray icon.
        #[arg(long)]
        headless: bool,
        /// Linux: install a systemd user unit instead of a desktop autostart entry.
        #[arg(long)]
        systemd: bool,
    },
    /// Stop starting Silicon Bridge at login.
    UninstallAutostart,
    /// Run one command on this computer the way a Silicon's command runs, without Bridge
    /// (for checking setup). Prints the result as JSON; files stay in --out.
    Exec {
        /// Session id to run in (agent-device session `bridge-<id>`).
        #[arg(long, default_value = "000")]
        session: String,
        /// Deadline in milliseconds.
        #[arg(long, default_value_t = 60_000)]
        timeout_ms: u64,
        /// Directory to keep produced files in (default: a new directory under the state dir).
        #[arg(long)]
        out: Option<PathBuf>,
        /// End the session afterwards (closes agent-device's session).
        #[arg(long)]
        end_session: bool,
        /// The command and its arguments, e.g. `snapshot -i`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match real_main(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bridge-agent: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn real_main(cli: Cli) -> Result<i32> {
    let overrides = Overrides { service_url: cli.service_url.clone(), credential_store: cli.credential_store, home: cli.home.clone() };
    let config = Config::load(&overrides)?;
    match cli.command.unwrap_or(Command::Run { headless: false }) {
        Command::Run { headless } => run(config, headless, cli.verbose),
        Command::Status { json } => {
            let s = status::read_status_file(&config.status_path());
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!("{}", status::render_text(&s));
            }
            Ok(if s.phase == Phase::NotRunning { 3 } else { 0 })
        }
        Command::Probe { json } => {
            init_logging(&config, cli.verbose, false);
            let rt = runtime()?;
            let driver = LocalDriver::for_this_computer(&config);
            let p = rt.block_on(driver.probe());
            let hello = bridge_agent::agent::hello_from(&p);
            if json {
                println!("{}", serde_json::to_string_pretty(&hello)?);
            } else {
                let s = AgentStatus {
                    app_version: bridge_agent::config::APP_VERSION.into(),
                    phase: Phase::Starting,
                    capabilities: p.capabilities,
                    missing: p.missing,
                    setup: Some(p.setup),
                    service_url: config.service_url.to_string(),
                    ..Default::default()
                };
                let text = status::render_text(&s);
                // Skip the headline, which only describes a running app.
                println!("{}", text.lines().skip(1).collect::<Vec<_>>().join("\n"));
                if let Some(v) = hello.agent_device_version {
                    println!("agent-device {v}");
                }
            }
            Ok(0)
        }
        Command::Stop => {
            let rt = runtime()?;
            let store = credential::store_for(&config);
            let c = load_for(store.as_ref(), &config.service_url)?.context("this computer isn't paired")?;
            let service = bridge_agent::service::ServiceClient::new(config.service_url.clone());
            rt.block_on(service.stop(&c.device_credential)).map_err(|e| anyhow::anyhow!(e.message))?;
            println!("Stopped.");
            Ok(0)
        }
        Command::Revoke { yes } => {
            let store = credential::store_for(&config);
            let c = load_for(store.as_ref(), &config.service_url)?.context("this computer isn't paired")?;
            if !yes {
                anyhow::ensure!(
                    std::io::stdin().is_terminal(),
                    "revoking needs confirmation; run it in a terminal or pass --yes"
                );
                print!(
                    "Revoke pair? This removes this computer ({}) from its Carbon's account and ends every Silicon's access to it. Type yes to revoke: ",
                    c.device_id
                );
                std::io::stdout().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != "yes" {
                    println!("Nothing changed.");
                    return Ok(1);
                }
            }
            let rt = runtime()?;
            let service = bridge_agent::service::ServiceClient::new(config.service_url.clone());
            match rt.block_on(service.revoke_pair(&c.device_credential)) {
                Ok(()) => {}
                Err(e) if e.is_auth() => println!("This computer was already unpaired."),
                Err(e) => anyhow::bail!("couldn't revoke the pair: {}", e.message),
            }
            store.clear()?;
            println!("Revoked. Silicon Bridge will show a new pairing code.");
            Ok(0)
        }
        Command::InstallAutostart { headless, systemd } => {
            let at = bridge_agent::autostart::install(&bridge_agent::autostart::AutostartOptions { headless, systemd })?;
            println!("Silicon Bridge will start at login ({at}).");
            Ok(0)
        }
        Command::UninstallAutostart => {
            let removed = bridge_agent::autostart::uninstall()?;
            if removed.is_empty() {
                println!("Silicon Bridge wasn't set to start at login.");
            } else {
                println!("Removed {}.", removed.join(", "));
            }
            Ok(0)
        }
        Command::Exec { session, timeout_ms, out, end_session, command } => exec(config, cli.verbose, session, timeout_ms, out, end_session, command),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().context("couldn't start the async runtime")
}

/// Logs to stderr (when it's a terminal or headless) and to `{state}/logs/bridge-agent.log`.
fn init_logging(config: &Config, verbose: bool, to_file: bool) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        if verbose { "info,bridge_agent=debug,bridge_hosted=debug".into() } else { "warn,bridge_agent=info,bridge_hosted=info".into() }
    });
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_target(false);
    let file_layer = to_file.then(|| {
        let dir = config.log_dir();
        let _ = bridge_agent::config::ensure_private_dir(&dir);
        let path = dir.join("bridge-agent.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
            let _ = std::fs::rename(&path, dir.join("bridge-agent.log.1"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(&path).ok().map(|f| {
            tracing_subscriber::fmt::layer().with_writer(std::sync::Mutex::new(f)).with_ansi(false).with_target(false)
        })
    });
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter))
        .with(stderr)
        .with(file_layer.flatten())
        .try_init();
}

fn hosted_factory() -> DriverFactory {
    Arc::new(bridge_hosted::driver_for)
}

/// Holds `{state}/agent.lock` so two copies never fight over one credential.
fn single_instance(config: &Config) -> Result<Option<std::fs::File>> {
    let path = config.state_dir.join("agent.lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("couldn't take the single-instance lock"),
    }
}

fn run(config: Config, headless: bool, verbose: bool) -> Result<i32> {
    init_logging(&config, verbose, true);
    let Some(_lock) = single_instance(&config)? else {
        let s = status::read_status_file(&config.status_path());
        eprintln!("Silicon Bridge is already running (pid {}): {}", s.pid, s.headline());
        return Ok(0);
    };
    let local: Arc<dyn Driver> = Arc::new(LocalDriver::for_this_computer(&config));
    let deps = AgentDeps {
        config: config.clone(),
        local,
        hosted_factory: hosted_factory(),
        credentials: credential::store_for(&config),
        probe_interval: Duration::from_secs(30),
    };
    let (agent, handle) = Agent::new(deps);
    tracing::info!("Silicon Bridge {} starting; service {}", bridge_agent::config::APP_VERSION, config.service_url);

    let want_ui = !headless && ui_possible();
    if !want_ui {
        let rt = runtime()?;
        rt.block_on(async move {
            let printer = tokio::spawn(print_status(handle.clone()));
            let shutdown = handle.shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                eprintln!("Stopping…");
                shutdown.cancel();
            });
            #[cfg(unix)]
            {
                let shutdown = handle.shutdown.clone();
                tokio::spawn(async move {
                    if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                        term.recv().await;
                        shutdown.cancel();
                    }
                });
            }
            agent.run().await;
            printer.abort();
        });
        return Ok(0);
    }
    run_with_ui(agent, handle)
}

#[cfg(feature = "tray")]
fn run_with_ui(agent: Agent, handle: AgentHandle) -> Result<i32> {
    let rt = runtime()?;
    let rt_handle = rt.handle().clone();
    {
        let shutdown = handle.shutdown.clone();
        rt.spawn(async move {
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending::<()>().await } } => {}
                }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        });
    }
    let agent_thread = std::thread::Builder::new()
        .name("bridge-agent".into())
        .spawn(move || {
            rt.block_on(agent.run());
            // Let in-flight uploads finish their last bytes.
            rt.shutdown_timeout(Duration::from_secs(2));
        })
        .context("couldn't start the agent thread")?;
    bridge_agent::ui::run(handle, rt_handle, agent_thread)
}

#[cfg(not(feature = "tray"))]
fn run_with_ui(_agent: Agent, _handle: AgentHandle) -> Result<i32> {
    anyhow::bail!("this build has no tray icon; run with --headless")
}

/// Whether a tray icon can be shown here.
fn ui_possible() -> bool {
    if !cfg!(feature = "tray") {
        return false;
    }
    if cfg!(target_os = "linux") {
        let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
        if !set("DISPLAY") && !set("WAYLAND_DISPLAY") {
            eprintln!("No screen here; running headless.");
            return false;
        }
    }
    true
}

/// Headless mode: one line on stdout whenever something the Carbon cares about changes.
async fn print_status(handle: AgentHandle) {
    let mut rx = handle.status.subscribe();
    let mut last = String::new();
    loop {
        let s = rx.borrow_and_update().clone();
        let mut line = s.headline();
        if s.phase == Phase::Enrolling
            && let Some(p) = &s.pairing
        {
            line = format!(
                "Pairing code: {} (enter it at bridge.teamofsilicons.com › Add a device; it changes at {})",
                p.code, p.expires_at
            );
        }
        if let Some(env) = &s.environment {
            line.push_str(&format!(" [test environment: {}]", env.name));
        }
        if let Some(t) = &s.takeover {
            line.push_str(&format!(" — waiting for you: {}", t.reason));
        }
        if s.in_use.is_some() {
            line.push_str(" — stop it with `bridge-agent stop`");
        }
        if line != last {
            println!("[silicon-bridge] {line}");
            if s.phase == Phase::Online && !s.missing.is_empty() && last.is_empty() {
                for m in &s.missing {
                    println!("[silicon-bridge]   missing {}: {}", m.capability.as_str(), m.reason);
                }
            }
            last = line;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

fn exec(config: Config, verbose: bool, session: String, timeout_ms: u64, out: Option<PathBuf>, end_session: bool, command: Vec<String>) -> Result<i32> {
    init_logging(&config, verbose, false);
    let (name, args) = command.split_first().context("no command given")?;
    let frame = bridge_protocol::frames::CommandFrame {
        id: uuid::Uuid::new_v4(),
        session_id: session.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        target: None,
        command: name.clone(),
        args: args.to_vec(),
        attachments: vec![],
        timeout_ms,
        upload_ids: vec![],
    };
    if let Err(e) = bridge_agent::dispatch::validate(&frame) {
        println!("{}", serde_json::json!({"ok": false, "output": null, "text": e.message, "error": e, "files": []}));
        return Ok(2);
    }
    let workdir = out.unwrap_or_else(|| config.work_dir().join(format!("exec-{}", frame.id)));
    std::fs::create_dir_all(&workdir)?;
    let rt = runtime()?;
    let driver = LocalDriver::for_this_computer(&config);
    let cancel = bridge_driver::cancel::CancelToken::new();
    let budget = bridge_agent::dispatch::deadline(timeout_ms);
    let output = rt.block_on(async {
        let inv = Invocation {
            id: frame.id,
            session_id: &session,
            command: name,
            args,
            attachments: &[],
            workdir: &workdir,
            timeout: budget,
            cancel: cancel.clone(),
        };
        let o = bridge_agent::dispatch::run_with_deadline(&driver, inv, budget, &cancel).await;
        if end_session {
            driver.session_ended(&session).await;
        }
        o
    });
    let files: Vec<serde_json::Value> = output
        .files
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name, "path": f.path, "kind": f.kind, "content_type": f.content_type,
                "size_bytes": std::fs::metadata(&f.path).map(|m| m.len()).ok(),
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "ok": output.ok, "output": output.output, "text": output.text, "error": output.error, "files": files,
        }))?
    );
    Ok(if output.ok { 0 } else { 1 })
}
