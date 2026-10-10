//! `extend-service [serve|migrate|identity …|link-identities …]`.

use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  extend-service [serve]                                   run the service
  extend-service migrate                                   bring the database schema up to date, then exit
  extend-service identity suggest [--out mapping.csv]      write a candidate mapping of old ids to Silicon Accounts uuids
  extend-service identity apply --file mapping.csv [--dry-run]
                                                           re-key old ids to Silicon Accounts uuids (one transaction)
  extend-service link-identities --file mapping.csv [--dry-run]
                                                           the same as identity apply";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--help" | "-h" | "help")) {
        println!("{USAGE}");
        return Ok(());
    }
    let cfg = extend_service::config::Config::from_env()?;
    match args.first().map(String::as_str) {
        None | Some("serve") => {
            let state = extend_service::build(cfg).await?;
            extend_service::serve(state).await
        }
        Some("migrate") => {
            let pool = extend_service::db::connect(&cfg.database_url).await?;
            extend_service::db::migrate_global(&pool).await?;
            extend_service::versions::migrate(&pool).await?;
            println!(
                "migrations applied (schema extend at version {})",
                extend_service::db::WORLD_VERSION
            );
            Ok(())
        }
        Some(cmd @ ("identity" | "link-identities")) => {
            let sub = if cmd == "link-identities" {
                "apply"
            } else {
                args.get(1).map(String::as_str).unwrap_or("")
            };
            let pool = extend_service::db::connect(&cfg.database_url).await?;
            extend_service::db::migrate_global(&pool).await?;
            match sub {
                "apply" => {
                    let file = flag(&args, "--file")
                        .ok_or_else(|| anyhow::anyhow!("--file mapping.csv is required\n{USAGE}"))?;
                    let text = std::fs::read_to_string(&file).map_err(|e| anyhow::anyhow!("reading {file}: {e}"))?;
                    let links = extend_service::identity::parse_mapping(&text)?;
                    let dry_run = args.iter().any(|a| a == "--dry-run");
                    let report = extend_service::identity::apply(&pool, &links, &file, dry_run).await?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    Ok(())
                }
                "suggest" => {
                    let api: Box<dyn extend_service::accounts::api::AccountsApi> = match &cfg.accounts {
                        extend_service::config::AccountsMode::Sdk { app_secret } => Box::new(
                            extend_service::accounts::api::SdkApi::new(&cfg.accounts_api_url, &cfg.app_id, app_secret)?,
                        ),
                        extend_service::config::AccountsMode::Local => {
                            anyhow::bail!(
                                "identity suggest asks the real Silicon Accounts: set ACCOUNTS_URL and EXTEND_APP_SECRET"
                            )
                        }
                    };
                    let csv = extend_service::identity::suggest(&pool, api.as_ref()).await?;
                    match flag(&args, "--out") {
                        Some(path) => {
                            std::fs::write(&path, &csv)?;
                            eprintln!(
                                "wrote {path}; review it, then run `extend-service identity apply --file {path} --dry-run`"
                            );
                        }
                        None => print!("{csv}"),
                    }
                    Ok(())
                }
                other => anyhow::bail!("unknown identity command {other:?}\n{USAGE}"),
            }
        }
        Some(other) => anyhow::bail!("unknown command {other:?}\n{USAGE}"),
    }
}
