//! `extend-service [serve|migrate]`.

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let json = std::env::var("EXTEND_LOG_FORMAT").is_ok_and(|v| v == "json");
    let filter =
        EnvFilter::try_from_env("EXTEND_LOG").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=info"));
    if json {
        tracing_subscriber::fmt().with_env_filter(filter).json().init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let cfg = extend_service::config::Config::from_env()?;
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => {
            let state = extend_service::build(cfg).await?;
            extend_service::serve(state).await
        }
        Some("migrate") => {
            let pool = extend_service::db::connect(&cfg.database_url).await?;
            extend_service::db::migrate_global(&pool).await?;
            let test_worlds = extend_service::db::ensure_test_worlds(&pool).await?;
            extend_service::versions::migrate(&pool).await?;
            println!(
                "migrations applied (production and {} test environments)",
                test_worlds.len()
            );
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other:?}; use `serve` or `migrate`"),
    }
}
