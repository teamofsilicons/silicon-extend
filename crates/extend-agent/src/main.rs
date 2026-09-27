//! `extend-agent`: the Silicon Extend app for Mac, Windows and Linux.

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use extend_agent::agent::{Agent, AgentDeps, AgentHandle};
use extend_agent::config::{Config, CredentialStoreKind, Overrides};
use extend_agent::credential::{self, load_for};
use extend_agent::drivers::local::LocalDriver;
use extend_agent::hosted::DriverFactory;
use extend_agent::status::{self, AgentStatus, Phase};
use extend_driver::{Driver, Invocation};

#[derive(Parser)]
#[command(
    name = "extend-agent",
    version,
    about = "Silicon Extend for Mac, Windows and Linux: lets the Silicons a Carbon chooses use this computer."
)]
struct Cli {
    /// Extend service URL (default https://backend.extend.teamofsilicons.com; env EXTEND_API_URL).
    #[arg(long, global = true)]
    service_url: Option<String>,
    /// Where the device credential is kept: auto, keyring or file (env EXTEND_AGENT_CREDENTIAL_STORE).
    #[arg(long, global = true)]
    credential_store: Option<CredentialStoreKind>,
    /// Base directory; state lives in <home>/.extend-agent (env SILICON_HOME, default ~).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Where "Download the update" goes when Extend needs a newer app (env EXTEND_DOWNLOAD_URL,
    /// default the website's download page for this OS).
    #[arg(long, global = true)]
    download_url: Option<String>,
    /// More detail in the log.
    #[arg(long, short, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the app (the default): pair, stay connected, carry out commands. Once this computer is
    /// paired, the app with a window starts at login from then on, unless you turn that off.
    Run {
        /// No tray icon or windows; status goes to stdout. For servers and CI. Doesn't turn on
        /// start at login unless --autostart is given.
        #[arg(long)]
        headless: bool,
        /// Start at login from now on (also for --headless; on Linux --headless uses a systemd
        /// user unit). Remembered.
        #[arg(long, conflicts_with = "no_autostart")]
        autostart: bool,
        /// Don't start at login: removes the entry if there is one, and the app won't turn it on
        /// again after pairing. Remembered; undo with --autostart or install-autostart.
        #[arg(long)]
        no_autostart: bool,
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
    /// Start Silicon Extend when you log in.
    InstallAutostart {
        /// Start without the tray icon.
        #[arg(long)]
        headless: bool,
        /// Linux: install a systemd user unit instead of a desktop autostart entry.
        #[arg(long)]
        systemd: bool,
    },
    /// Stop starting Silicon Extend at login.
    UninstallAutostart,
    /// Run one command on this computer the way a Silicon's command runs, without Extend
    /// (for checking setup). Prints the result as JSON; files stay in --out.
    Exec {
        /// Session id to run in (agent-device session `extend-<id>`).
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
            eprintln!("extend-agent: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn real_main(cli: Cli) -> Result<i32> {
    let overrides = Overrides {
        service_url: cli.service_url.clone(),
        credential_store: cli.credential_store,
        home: cli.home.clone(),
        download_url: cli.download_url.clone(),
    };
    let config = Config::load(&overrides)?;
    let default_run = Command::Run {
        headless: false,
        autostart: false,
        no_autostart: false,
    };
    match cli.command.unwrap_or(default_run) {
        Command::Run {
            headless,
            autostart,
            no_autostart,
        } => {
            let flag = match (autostart, no_autostart) {
                (true, _) => extend_agent::autostart::RunFlag::On,
                (_, true) => extend_agent::autostart::RunFlag::Off,
                _ => extend_agent::autostart::RunFlag::Unset,
            };
            run(config, headless, flag, cli.verbose)
        }
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
            let hello = extend_agent::agent::hello_from(&p);
            if json {
                println!("{}", serde_json::to_string_pretty(&hello)?);
            } else {
                let s = AgentStatus {
                    app_version: extend_agent::config::APP_VERSION.into(),
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
            let service = extend_agent::service::ServiceClient::new(config.service_url.clone());
            rt.block_on(service.stop(&c.device_credential))
                .map_err(|e| anyhow::anyhow!(e.message))?;
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
            let service = extend_agent::service::ServiceClient::new(config.service_url.clone());
            match rt.block_on(service.revoke_pair(&c.device_credential)) {
                Ok(()) => {}
                Err(e) if e.is_auth() => println!("This computer was already unpaired."),
                Err(e) => anyhow::bail!("couldn't revoke the pair: {}", e.message),
            }
            store.clear()?;
            println!("Revoked. Silicon Extend will show a new pairing code.");
            Ok(0)
        }
        Command::InstallAutostart { headless, systemd } => {
            let opts = extend_agent::autostart::AutostartOptions { headless, systemd };
            let at = extend_agent::autostart::set_by_carbon(&config.state_dir, true, &opts)?;
            println!("Silicon Extend will start at login ({at}).");
            Ok(0)
        }
        Command::UninstallAutostart => {
            let removed = extend_agent::autostart::set_by_carbon(&config.state_dir, false, &Default::default())?;
            println!(
                "Silicon Extend won't start at login ({removed}), and won't turn it on again by itself. Turn it back on with `extend-agent install-autostart`."
            );
            Ok(0)
        }
        Command::Exec {
            session,
            timeout_ms,
            out,
            end_session,
            command,
        } => exec(config, cli.verbose, session, timeout_ms, out, end_session, command),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("couldn't start the async runtime")
}

/// Logs to stderr (when it's a terminal or headless) and to `{state}/logs/extend-agent.log`.
fn init_logging(config: &Config, verbose: bool, to_file: bool) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        if verbose {
            "info,extend_agent=debug,extend_hosted=debug".into()
        } else {
            "warn,extend_agent=info,extend_hosted=info".into()
        }
    });
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);
    let file_layer = to_file.then(|| {
        let dir = config.log_dir();
        let _ = extend_agent::config::ensure_private_dir(&dir);
        let path = dir.join("extend-agent.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
            let _ = std::fs::rename(&path, dir.join("extend-agent.log.1"));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(|f| {
                tracing_subscriber::fmt::layer()
                    .with_writer(std::sync::Mutex::new(f))
                    .with_ansi(false)
                    .with_target(false)
            })
    });
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter))
        .with(stderr)
        .with(file_layer.flatten())
        .try_init();
}

fn hosted_factory() -> DriverFactory {
    Arc::new(extend_hosted::driver_for)
}

/// Holds `{state}/agent.lock` so two copies never fight over one credential.
fn single_instance(config: &Config) -> Result<Option<std::fs::File>> {
    let path = config.state_dir.join("agent.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("couldn't take the single-instance lock"),
    }
}

/// `run --autostart` / `--no-autostart`: carried out, and remembered, before anything else.
fn apply_autostart_flag(config: &Config, flag: extend_agent::autostart::RunFlag, headless: bool) {
    use extend_agent::autostart::{self, AutostartOptions, Decision};
    let on = match autostart::at_start(flag, autostart::is_installed()) {
        Decision::Install => true,
        Decision::Remove => false,
        Decision::Leave(_) if flag == autostart::RunFlag::Off => false,
        Decision::Leave(_) => return,
    };
    let opts = AutostartOptions {
        headless,
        systemd: headless && cfg!(target_os = "linux"),
    };
    match autostart::set_by_carbon(&config.state_dir, on, &opts) {
        Ok(done) if on => {
            eprintln!("Silicon Extend will start at login ({done}).");
            if opts.systemd {
                eprintln!(
                    "It starts when you log in. To start it when the computer starts, before anyone logs in, run `loginctl enable-linger` once."
                );
            }
        }
        Ok(_) => eprintln!("Silicon Extend won't start at login, and won't turn it on again by itself."),
        Err(e) => eprintln!(
            "Couldn't change start at login: {e:#}. Silicon Extend runs anyway; try again with `extend-agent install-autostart` or `uninstall-autostart`."
        ),
    }
}

fn run(config: Config, headless: bool, autostart: extend_agent::autostart::RunFlag, verbose: bool) -> Result<i32> {
    init_logging(&config, verbose, true);
    let Some(_lock) = single_instance(&config)? else {
        let s = status::read_status_file(&config.status_path());
        eprintln!("Silicon Extend is already running (pid {}): {}", s.pid, s.headline());
        return Ok(0);
    };
    apply_autostart_flag(&config, autostart, headless);
    let local: Arc<dyn Driver> = Arc::new(LocalDriver::for_the_agent(&config));
    let deps = AgentDeps {
        config: config.clone(),
        local,
        hosted_factory: hosted_factory(),
        credentials: credential::store_for(&config),
        probe_interval: Duration::from_secs(30),
        screen_watch: Some(Arc::new(extend_agent::drivers::screen_lock::current)),
    };
    let (agent, handle) = Agent::new(deps);
    tracing::info!(
        "Silicon Extend {} starting; service {}",
        extend_agent::config::APP_VERSION,
        config.service_url
    );

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
    run_with_ui(agent, handle, &config)
}

#[cfg(feature = "tray")]
fn run_with_ui(agent: Agent, handle: AgentHandle, config: &Config) -> Result<i32> {
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
        .name("extend-agent".into())
        .spawn(move || {
            rt.block_on(agent.run());
            // Let in-flight uploads finish their last bytes.
            rt.shutdown_timeout(Duration::from_secs(2));
        })
        .context("couldn't start the agent thread")?;
    let context = extend_agent::ui::Context {
        state_dir: config.state_dir.clone(),
        download_url: config.download_url.to_string(),
    };
    extend_agent::ui::run(handle, rt_handle, agent_thread, context)
}

#[cfg(not(feature = "tray"))]
fn run_with_ui(_agent: Agent, _handle: AgentHandle, _config: &Config) -> Result<i32> {
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
                "Pairing code: {} (enter it at extend.teamofsilicons.com › Add a device; it changes at {})",
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
            line.push_str(" — stop it with `extend-agent stop`");
        }
        if line != last {
            println!("[silicon-extend] {line}");
            if s.phase == Phase::Online && !s.missing.is_empty() && last.is_empty() {
                for m in &s.missing {
                    println!("[silicon-extend]   missing {}: {}", m.capability.as_str(), m.reason);
                }
            }
            last = line;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

fn exec(
    config: Config,
    verbose: bool,
    session: String,
    timeout_ms: u64,
    out: Option<PathBuf>,
    end_session: bool,
    command: Vec<String>,
) -> Result<i32> {
    init_logging(&config, verbose, false);
    let (name, args) = command.split_first().context("no command given")?;
    let frame = extend_protocol::frames::CommandFrame {
        id: uuid::Uuid::new_v4(),
        session_id: session.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        target: None,
        command: name.clone(),
        args: args.to_vec(),
        attachments: vec![],
        timeout_ms,
        upload_ids: vec![],
    };
    if let Err(e) = extend_agent::dispatch::validate(&frame) {
        println!(
            "{}",
            serde_json::json!({"ok": false, "output": null, "text": e.message, "error": e, "files": []})
        );
        return Ok(2);
    }
    let workdir = out.unwrap_or_else(|| config.work_dir().join(format!("exec-{}", frame.id)));
    std::fs::create_dir_all(&workdir)?;
    let rt = runtime()?;
    let driver = LocalDriver::for_this_computer(&config);
    let cancel = extend_driver::cancel::CancelToken::new();
    let budget = extend_agent::dispatch::deadline(timeout_ms);
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
        let o = extend_agent::dispatch::run_with_deadline(&driver, inv, budget, &cancel).await;
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
