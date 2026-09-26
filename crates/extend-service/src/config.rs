//! Configuration from `EXTEND_*` environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Development,
    Test,
    Production,
}

/// Where login and authorization come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IamMode {
    /// The official Silicon IAM client against a real IAM.
    Sdk { base_url: String, app_id: String, app_secret: String },
    /// Local development and tests: members are named in `EXTEND_LOCAL_MEMBERS`. Refused in production.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesMode {
    Briefcase { api_url: String, web_url: String },
    /// Files kept on local disk and served from `/dev/files/{id}`. Refused in production.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TingMode {
    Ting { base_url: String },
    /// Requests are recorded and marked delivered. Refused in production.
    Local,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub environment: Environment,
    pub bind: SocketAddr,
    pub database_url: String,
    pub public_url: String,
    pub website_url: String,
    pub docs_url: String,
    pub repository_url: String,
    pub data_dir: PathBuf,
    pub iam: IamMode,
    pub iam_public_url: String,
    pub iam_login_url: String,
    pub webhook_secret: Option<(i64, String)>,
    pub webhook_previous_secret: Option<(i64, String)>,
    pub files: FilesMode,
    pub ting: TingMode,
    pub honeycomb_service_token: Option<String>,
    pub postmark_token: Option<String>,
    pub report_recipients: Vec<String>,
    pub device_app_min_version: String,
    /// Members local IAM knows: `c:alice@acme,si:chef@acme+labs`.
    pub local_members: Vec<(String, Vec<String>)>,
    /// Serve the built website from this directory when set.
    pub web_dir: Option<PathBuf>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn var_or(name: &str, default: &str) -> String {
    var(name).unwrap_or_else(|| default.to_owned())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let environment = match var_or("EXTEND_ENVIRONMENT", "development").as_str() {
            "development" => Environment::Development,
            "test" => Environment::Test,
            "production" => Environment::Production,
            other => bail!("EXTEND_ENVIRONMENT must be development, test or production, got {other:?}"),
        };
        let production = environment == Environment::Production;

        let iam = match var_or("EXTEND_IAM_MODE", if production { "sdk" } else { "local" }).as_str() {
            "sdk" => IamMode::Sdk {
                base_url: var_or("EXTEND_IAM_BASE_URL", "https://backend.iam.teamofsilicons.com"),
                app_id: var("EXTEND_IAM_APP_ID").context("EXTEND_IAM_APP_ID is required with EXTEND_IAM_MODE=sdk")?,
                app_secret: var("EXTEND_IAM_APP_SECRET")
                    .context("EXTEND_IAM_APP_SECRET is required with EXTEND_IAM_MODE=sdk")?,
            },
            "local" if production => bail!("EXTEND_IAM_MODE=local is refused in production"),
            "local" => IamMode::Local,
            other => bail!("EXTEND_IAM_MODE must be sdk or local, got {other:?}"),
        };
        let files = match var_or("EXTEND_FILES_MODE", if production { "briefcase" } else { "local" }).as_str() {
            "briefcase" => FilesMode::Briefcase {
                api_url: var_or("EXTEND_BRIEFCASE_URL", "https://backend.briefcase.teamofsilicons.com"),
                web_url: var_or("EXTEND_BRIEFCASE_WEB_URL", "https://briefcase.teamofsilicons.com"),
            },
            "local" if production => bail!("EXTEND_FILES_MODE=local is refused in production"),
            "local" => FilesMode::Local,
            other => bail!("EXTEND_FILES_MODE must be briefcase or local, got {other:?}"),
        };
        let ting = match var_or("EXTEND_TING_MODE", if production { "ting" } else { "local" }).as_str() {
            "ting" => TingMode::Ting { base_url: var_or("EXTEND_TING_URL", "https://backend.ting.teamofsilicons.com") },
            "local" if production => bail!("EXTEND_TING_MODE=local is refused in production"),
            "local" => TingMode::Local,
            other => bail!("EXTEND_TING_MODE must be ting or local, got {other:?}"),
        };
        let secret = |name: &str, version: &str| -> anyhow::Result<Option<(i64, String)>> {
            match var(name) {
                None => Ok(None),
                Some(s) => {
                    let v = var_or(version, "1").parse::<i64>().with_context(|| format!("{version} must be an integer"))?;
                    Ok(Some((v, s)))
                }
            }
        };
        let local_members = var("EXTEND_LOCAL_MEMBERS")
            .map(|raw| {
                raw.split(',')
                    .filter_map(|entry| {
                        let entry = entry.trim();
                        let (id, teams) = entry.split_once('@').unwrap_or((entry, "acme"));
                        (!id.is_empty()).then(|| (id.to_owned(), teams.split('+').map(str::to_owned).collect()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let bind = var_or("EXTEND_BIND", "127.0.0.1:8480").parse().context("EXTEND_BIND must be host:port")?;
        let public_url = var_or("EXTEND_PUBLIC_URL", &format!("http://{bind}"));
        if production && !public_url.starts_with("https://") {
            bail!("EXTEND_PUBLIC_URL must be https in production");
        }
        Ok(Self {
            environment,
            bind,
            database_url: var("EXTEND_DATABASE_URL").context("EXTEND_DATABASE_URL is required")?,
            public_url,
            website_url: var_or("EXTEND_WEBSITE_URL", "https://extend.teamofsilicons.com"),
            docs_url: var_or("EXTEND_DOCS_URL", "https://extend.teamofsilicons.com/docs"),
            repository_url: var_or("EXTEND_REPOSITORY_URL", "https://github.com/teamofsilicons/silicon-extend"),
            data_dir: PathBuf::from(var_or("EXTEND_DATA_DIR", "./data")),
            iam_public_url: match &iam {
                IamMode::Sdk { base_url, .. } => var("EXTEND_IAM_PUBLIC_URL").unwrap_or_else(|| base_url.clone()),
                IamMode::Local => var_or("EXTEND_IAM_PUBLIC_URL", "http://127.0.0.1:8480/dev/iam"),
            },
            iam_login_url: match &iam {
                IamMode::Sdk { .. } => var_or("EXTEND_IAM_LOGIN_URL", "https://auth.iam.teamofsilicons.com/login"),
                IamMode::Local => var("EXTEND_IAM_LOGIN_URL").unwrap_or_else(|| format!("{}/dev/iam/login", var_or("EXTEND_PUBLIC_URL", "http://127.0.0.1:8480"))),
            },
            iam,
            webhook_secret: secret("EXTEND_IAM_WEBHOOK_SECRET", "EXTEND_IAM_WEBHOOK_SECRET_VERSION")?,
            webhook_previous_secret: secret("EXTEND_IAM_WEBHOOK_PREVIOUS_SECRET", "EXTEND_IAM_WEBHOOK_PREVIOUS_SECRET_VERSION")?,
            files,
            ting,
            honeycomb_service_token: var("EXTEND_HONEYCOMB_SERVICE_TOKEN"),
            postmark_token: var("EXTEND_POSTMARK_SERVER_TOKEN"),
            report_recipients: var_or(
                "EXTEND_REPORT_RECIPIENTS",
                "saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com",
            )
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
            device_app_min_version: var_or("EXTEND_DEVICE_APP_MIN_VERSION", "1.0.0"),
            local_members,
            web_dir: var("EXTEND_WEB_DIR").map(PathBuf::from),
        })
    }

    pub fn is_production(&self) -> bool {
        self.environment == Environment::Production
    }
}
