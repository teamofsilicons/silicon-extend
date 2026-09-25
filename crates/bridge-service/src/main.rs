//! `bridge-service [serve|migrate]`.

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let json = std::env::var("BRIDGE_LOG_FORMAT").is_ok_and(|v| v == "json");
    let filter = EnvFilter::try_from_env("BRIDGE_LOG").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=info"));
    if json {
        tracing_subscriber::fmt().with_env_filter(filter).json().init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let cfg = bridge_service::config::Config::from_env()?;
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => {
            let state = bridge_service::build(cfg).await?;
            bridge_service::serve(state).await
        }
        Some("migrate") => {
            let pool = bridge_service::db::connect(&cfg.database_url).await?;
            bridge_service::db::migrate_global(&pool).await?;
            println!("migrations applied");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other:?}; use `serve` or `migrate`"),
    }
}
