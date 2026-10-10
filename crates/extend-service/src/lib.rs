//! The Silicon Extend service: pairing, access, sessions and the relay between Silicons and devices.

// Row tuples from sqlx and handlers that thread world/principal/selection through are clearer
// inline than behind one-off aliases or parameter structs.
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

/// Formats a query whose only dynamic parts are schema-qualified table names built by
/// [`db::World::t`] (from UUIDs, never from caller input) and fixed column lists.
macro_rules! sql {
    ($($t:tt)*) => { sqlx::AssertSqlSafe(format!($($t)*)) };
}

pub mod accounts;
pub mod config;
pub mod db;
pub mod delivery;
pub mod domain;
pub mod error;
pub mod files;
pub mod hub;
pub mod identity;
pub mod lifecycle;
pub mod proofs;
pub mod routes;
pub mod scheduler;
pub mod state;
pub mod telemetry;
pub mod ting;
pub mod versions;
pub mod wake;

use std::sync::Arc;

use config::{AccountsMode, Config, FilesMode, TingMode};
use state::{AppState, Shared};

/// Builds the shared state from configuration: connects the database, runs migrations, and wires
/// Silicon Accounts, Briefcase and Ting (or their local stand-ins).
pub async fn build(cfg: Config) -> anyhow::Result<Shared> {
    // Space Station's HTTP stack needs a process-wide TLS provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    for v in &cfg.obsolete {
        tracing::warn!(variable = %v, "this variable belonged to Silicon IAM or Honeycomb and is no longer read; remove it");
    }
    let pool = db::connect(&cfg.database_url).await?;
    db::migrate_global(&pool).await?;
    std::fs::create_dir_all(cfg.data_dir.join("uploads"))?;
    let (api, local): (Arc<dyn accounts::api::AccountsApi>, _) = match &cfg.accounts {
        AccountsMode::Sdk { app_secret } => (
            Arc::new(accounts::api::SdkApi::new(
                &cfg.accounts_api_url,
                &cfg.app_id,
                app_secret,
            )?),
            None,
        ),
        AccountsMode::Local => {
            let local = Arc::new(accounts::local::LocalAccounts::new(&cfg.accounts_url, &cfg.app_id));
            (local.clone() as Arc<dyn accounts::api::AccountsApi>, Some(local))
        }
    };
    let accounts = accounts::Accounts::new(&cfg.app_id, &cfg.accounts_url, api.clone(), local, pool.clone());
    accounts.prefetch().await;
    let proofs = Arc::new(proofs::ProofStore::new(pool.clone(), cfg.delegation_key.clone(), api));
    let files: files::DynFiles = match &cfg.files {
        FilesMode::Briefcase { api_url, web_url } => Arc::new(files::BriefcaseFiles::new(
            api_url.clone(),
            web_url.clone(),
            cfg.app_id.clone(),
            proofs.clone(),
        )),
        FilesMode::Local => Arc::new(files::LocalFiles::new(&cfg.data_dir.join("files"), &cfg.public_url)?),
    };
    let (notifier, local_ting): (ting::DynNotifier, _) = match &cfg.ting {
        TingMode::Off => {
            tracing::warn!(
                "notifications through Ting are off (EXTEND_TING_URL is unset): requests and wake requests are shown on the \
                 website, in the CLI and on the device only"
            );
            (
                Arc::new(ting::OffNotifier {
                    app_id: cfg.app_id.clone(),
                }),
                None,
            )
        }
        TingMode::Ting { base_url } => (
            Arc::new(ting::TingNotifier::new(
                base_url.clone(),
                cfg.app_id.clone(),
                proofs.clone(),
            )),
            None,
        ),
        TingMode::Local => {
            let t = Arc::new(ting::LocalNotifier::default());
            (t.clone(), Some(t))
        }
    };
    tracing::info!(
        accounts = %cfg.accounts_url,
        app_id = %cfg.app_id,
        mode = if matches!(cfg.accounts, AccountsMode::Local) { "local stand-in" } else { "silicon accounts" },
        "signing in with Silicon Accounts"
    );
    Ok(Arc::new(AppState {
        cfg,
        pool,
        accounts,
        proofs,
        files,
        notifier,
        local_ting,
        hub: hub::Hub::default(),
        http: reqwest::Client::new(),
        session_principals: Default::default(),
        limits: Default::default(),
    }))
}

/// Serves until the process is told to stop.
pub async fn serve(state: Shared) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(state.cfg.bind).await?;
    serve_on(listener, state).await
}

/// Serves on an already-bound listener (tests bind port 0).
pub async fn serve_on(listener: tokio::net::TcpListener, state: Shared) -> anyhow::Result<()> {
    let versions = versions::Registry::start(state.pool.clone(), versions::Policy::from_env()?).await?;
    serve_versioned(listener, state, versions).await
}

/// [`serve_on`] with an API version registry the caller started (tests inject a deprecation
/// policy, extra majors and a clock).
pub async fn serve_versioned(
    listener: tokio::net::TcpListener,
    state: Shared,
    versions: Arc<versions::Registry>,
) -> anyhow::Result<()> {
    scheduler::spawn(state.clone());
    versions.spawn_upkeep();
    tracing::info!(addr = %state.cfg.bind, environment = ?state.cfg.environment, "Silicon Extend service listening");
    let app = routes::router(state, versions).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
