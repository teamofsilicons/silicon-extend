//! The Silicon Extend service: pairing, access, sessions and the relay between Silicons and devices.

// Row tuples from sqlx and handlers that thread world/principal/selection through are clearer
// inline than behind one-off aliases or parameter structs.
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

/// Formats a query whose only dynamic parts are schema-qualified table names built by
/// [`db::World::t`] (from UUIDs, never from caller input) and fixed column lists.
macro_rules! sql {
    ($($t:tt)*) => { sqlx::AssertSqlSafe(format!($($t)*)) };
}

pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod files;
pub mod hub;
pub mod iam;
pub mod revocation;
pub mod routes;
pub mod scheduler;
pub mod state;
pub mod telemetry;
pub mod ting;
pub mod versions;

use std::sync::Arc;

use config::{Config, FilesMode, IamMode, TingMode};
use state::{AppState, Shared};

/// Builds the shared state from configuration: connects the database, runs migrations, and wires
/// IAM, Briefcase and Ting (or their local stand-ins).
pub async fn build(cfg: Config) -> anyhow::Result<Shared> {
    // Space Station's HTTP stack needs a process-wide TLS provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pool = db::connect(&cfg.database_url).await?;
    db::migrate_global(&pool).await?;
    std::fs::create_dir_all(cfg.data_dir.join("uploads"))?;
    let (iam, local_iam): (iam::DynIam, Option<Arc<iam::LocalIam>>) = match &cfg.iam {
        IamMode::Sdk {
            base_url,
            app_id,
            app_secret,
        } => (
            Arc::new(
                iam::SdkIam::connect(
                    base_url,
                    app_id,
                    app_secret,
                    cfg.webhook_secret.clone(),
                    cfg.webhook_previous_secret.clone(),
                )
                .await?,
            ),
            None,
        ),
        IamMode::Local => {
            let local = Arc::new(iam::LocalIam::new(cfg.local_members.clone(), pool.clone()));
            (local.clone(), Some(local))
        }
    };
    let files: files::DynFiles = match &cfg.files {
        FilesMode::Briefcase { api_url, web_url } => Arc::new(files::BriefcaseFiles::new(
            api_url.clone(),
            web_url.clone(),
            iam.clone(),
        )),
        FilesMode::Local => Arc::new(files::LocalFiles::new(&cfg.data_dir.join("files"), &cfg.public_url)?),
    };
    let (notifier, local_ting): (ting::DynNotifier, _) = match &cfg.ting {
        TingMode::Ting { base_url } => (Arc::new(ting::TingNotifier::new(base_url.clone(), iam.clone())), None),
        TingMode::Local => {
            let t = Arc::new(ting::LocalNotifier::default());
            (t.clone(), Some(t))
        }
    };
    Ok(Arc::new(AppState {
        cfg,
        pool,
        iam,
        local_iam,
        files,
        notifier,
        local_ting,
        hub: hub::Hub::default(),
        auth_cache: iam::AuthCache::default(),
        http: reqwest::Client::new(),
        ready_worlds: Default::default(),
        selections: Default::default(),
        selection_revisions: Default::default(),
        fences: Default::default(),
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
