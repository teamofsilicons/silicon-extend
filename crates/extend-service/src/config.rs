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

/// Where sign-in and accounts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountsMode {
    /// The official `silicon-accounts-client` against a real Silicon Accounts, with Extend's app
    /// secret (`EXTEND_APP_SECRET`).
    Sdk { app_secret: String },
    /// Development and tests: an in-process stand-in that signs its own access tokens
    /// (`POST /dev/accounts/token`). Refused in production.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesMode {
    Briefcase {
        api_url: String,
        web_url: String,
    },
    /// Files kept on local disk and served from `/dev/files/{id}`. Refused in production.
    Local,
}

/// How Extend's notifications (a device requested, wake requests and their answers) reach people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TingMode {
    /// Not delivered: `EXTEND_TING_URL` is unset. Requests and wake requests are on the website,
    /// in the CLI and on the device only.
    Off,
    /// Through Ting at `EXTEND_TING_URL`, with Silicon Accounts proofs.
    Ting { base_url: String },
    /// Recorded and marked delivered (`EXTEND_TING_MODE=local`). Refused in production.
    Local,
}

impl TingMode {
    pub fn enabled(&self) -> bool {
        !matches!(self, Self::Off)
    }
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
    /// `ACCOUNTS_URL`: Silicon Accounts' public origin, the `iss` of every accepted token.
    pub accounts_url: String,
    /// `ACCOUNTS_API_URL`: where Extend reaches Silicon Accounts server to server (defaults to
    /// `ACCOUNTS_URL`).
    pub accounts_api_url: String,
    /// `EXTEND_APP_ID`: Extend's app id in Silicon Accounts (`extend`).
    pub app_id: String,
    pub accounts: AccountsMode,
    /// `EXTEND_ACCOUNTS_WEBHOOK_SECRET` (and `_PREVIOUS_SECRET` during a rotation).
    pub webhook_secret: Option<String>,
    pub webhook_previous_secret: Option<String>,
    /// `EXTEND_DELEGATION_ENCRYPTION_KEY`: seals the proof refresh tokens Extend keeps.
    pub delegation_key: Option<crate::proofs::GrantKey>,
    pub files: FilesMode,
    pub ting: TingMode,
    pub postmark_token: Option<String>,
    pub report_recipients: Vec<String>,
    pub device_app_min_version: String,
    /// Serve the built website from this directory when set.
    pub web_dir: Option<PathBuf>,
    /// Reverse proxies whose `X-Forwarded-For` is believed (`EXTEND_TRUSTED_PROXY_CIDRS`). Empty:
    /// the TCP peer is the client.
    pub trusted_proxies: Vec<Cidr>,
    /// `EXTEND_CORS_ORIGINS`: browser origins allowed to call the API directly. Empty (the
    /// default): none; the website calls the API from its server.
    pub cors_origins: Vec<String>,
    pub tuning: Tuning,
    /// Variables from before Silicon Accounts that are set but no longer read (logged at start).
    pub obsolete: Vec<String>,
}

/// Limits with an `EXTEND_*` variable each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuning {
    /// `EXTEND_MAX_PAIRS_PER_DEVICE`: most Carbons one device may be paired to. A guard on
    /// connections per device (each pair is one socket), not a product rule.
    pub max_pairs_per_device: i64,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            max_pairs_per_device: 8,
        }
    }
}

/// Variables Extend read before it moved to Silicon Accounts and Silicon Apps.
pub const OBSOLETE_VARIABLES: &[&str] = &[
    "EXTEND_IAM_MODE",
    "EXTEND_IAM_BASE_URL",
    "EXTEND_IAM_APP_ID",
    "EXTEND_IAM_APP_SECRET",
    "EXTEND_IAM_PUBLIC_URL",
    "EXTEND_IAM_LOGIN_URL",
    "EXTEND_IAM_WEBHOOK_SECRET",
    "EXTEND_IAM_WEBHOOK_SECRET_VERSION",
    "EXTEND_IAM_WEBHOOK_PREVIOUS_SECRET",
    "EXTEND_IAM_WEBHOOK_PREVIOUS_SECRET_VERSION",
    "EXTEND_HONEYCOMB_SERVICE_TOKEN",
    "EXTEND_LOCAL_MEMBERS",
    "EXTEND_LOCAL_IAM_READERS",
    "EXTEND_MEMBERSHIP_SWEEP_HOURS",
    "EXTEND_OWNER_CHECK_CACHE_S",
    "EXTEND_OWNER_CHECK_AT_USE",
    "EXTEND_TEST_LINK_WINDOW_S",
    "EXTEND_TEST_TELEMETRY_KEYS",
];

/// Checks a Silicon Accounts URL: an absolute https URL, or http for this machine only (the local
/// stack). Returns it without a trailing slash.
pub fn accounts_origin(name: &str, raw: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(raw.trim()).map_err(|e| {
        anyhow::anyhow!("{name} must be an absolute URL like https://accounts.teamofsilicons.com, got {raw:?} ({e})")
    })?;
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => bail!(
            "{name} is {raw:?}: plain http is allowed only for this machine (localhost, 127.0.0.1, ::1), \
             because access tokens and app credentials would travel unencrypted. Use https."
        ),
        other => bail!("{name} must be an https URL, got the {other:?} scheme in {raw:?}"),
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        bail!("{name} must be an origin with no query, fragment or credentials, got {raw:?}");
    }
    Ok(raw.trim().trim_end_matches('/').to_owned())
}

/// An address block such as `172.30.87.0/24`, `10.0.0.5` (one address) or `fd00::/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    net: std::net::IpAddr,
    prefix: u8,
}

impl std::str::FromStr for Cidr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = s.trim().split_once('/').map_or((s.trim(), None), |(a, p)| (a, Some(p)));
        let net: std::net::IpAddr = addr
            .parse()
            .map_err(|_| format!("{s:?} is not an address or CIDR block"))?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or(format!("{s:?} has an invalid prefix length"))?,
        };
        Ok(Self { net, prefix })
    }
}

impl Cidr {
    pub fn contains(&self, ip: std::net::IpAddr) -> bool {
        let ip = match ip {
            std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, std::net::IpAddr::V4),
            v4 => v4,
        };
        let (net, ip, bits) = match (self.net, ip) {
            (std::net::IpAddr::V4(n), std::net::IpAddr::V4(i)) => (u32::from(n) as u128, u32::from(i) as u128, 32),
            (std::net::IpAddr::V6(n), std::net::IpAddr::V6(i)) => (u128::from(n), u128::from(i), 128),
            _ => return false,
        };
        let host_bits = bits - u32::from(self.prefix);
        host_bits >= bits || (net >> host_bits) == (ip >> host_bits)
    }
}

/// The client's address: the TCP peer, unless the peer is a trusted proxy, in which case the
/// right-most `X-Forwarded-For` entry that is not itself a trusted proxy. Entries left of that are
/// written by the client and never believed.
pub fn client_ip(peer: std::net::IpAddr, headers: &axum::http::HeaderMap, trusted: &[Cidr]) -> std::net::IpAddr {
    let is_trusted = |ip: std::net::IpAddr| trusted.iter().any(|c| c.contains(ip));
    if !is_trusted(peer) {
        return peer;
    }
    let forwarded: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    for entry in forwarded.iter().rev() {
        match entry.parse::<std::net::IpAddr>() {
            Ok(ip) if is_trusted(ip) => continue,
            Ok(ip) => return ip,
            // A malformed hop means the chain can't be read past it; the proxy is all we know.
            Err(_) => return peer,
        }
    }
    peer
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup` (the process environment in [`Config::from_env`];
    /// a map in tests). Blank values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let var = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
        let var_or = |name: &str, default: &str| var(name).unwrap_or_else(|| default.to_owned());
        // No default: an unset mode must never quietly become development, which would accept
        // the local Silicon Accounts, Briefcase and Ting stand-ins on a real deployment.
        let environment = match var("EXTEND_ENVIRONMENT").as_deref().map(str::trim) {
            Some("development") => Environment::Development,
            Some("test") => Environment::Test,
            Some("production") => Environment::Production,
            Some(other) => bail!(
                "EXTEND_ENVIRONMENT must be development, test or production, got {other:?}. \
                 Use production for a deployment; development or test run with local stand-ins."
            ),
            None => bail!(
                "EXTEND_ENVIRONMENT is not set, so Extend won't guess whether this is a deployment. \
                 Set EXTEND_ENVIRONMENT=production for a deployment (the Docker image does), or \
                 EXTEND_ENVIRONMENT=development to run with the local Silicon Accounts, Briefcase and Ting stand-ins \
                 (e2e/dev.env does)."
            ),
        };
        let production = environment == Environment::Production;
        let bind: SocketAddr = var_or("EXTEND_BIND", "127.0.0.1:8480")
            .parse()
            .context("EXTEND_BIND must be host:port")?;
        let public_url = var_or("EXTEND_PUBLIC_URL", &format!("http://{bind}"))
            .trim_end_matches('/')
            .to_owned();
        if production && !public_url.starts_with("https://") {
            bail!("EXTEND_PUBLIC_URL must be https in production, got {public_url:?}");
        }

        let mode_default = if production || var("ACCOUNTS_URL").is_some() {
            "sdk"
        } else {
            "local"
        };
        let accounts = match var_or("EXTEND_ACCOUNTS_MODE", mode_default).trim() {
            "sdk" => AccountsMode::Sdk {
                app_secret: var("EXTEND_APP_SECRET").context(
                    "EXTEND_APP_SECRET is required: it is Extend's app secret from Silicon Apps, which Extend uses to \
                     introspect sign-ins, look up accounts and get proofs from Silicon Accounts",
                )?,
            },
            "local" if production => bail!(
                "EXTEND_ACCOUNTS_MODE=local is refused in production: the local stand-in signs its own access tokens. \
                 Set ACCOUNTS_URL, EXTEND_APP_SECRET and EXTEND_ACCOUNTS_WEBHOOK_SECRET instead."
            ),
            "local" => AccountsMode::Local,
            other => bail!("EXTEND_ACCOUNTS_MODE must be sdk or local, got {other:?}"),
        };
        let accounts_url = match (&accounts, var("ACCOUNTS_URL")) {
            (_, Some(u)) => accounts_origin("ACCOUNTS_URL", &u)?,
            (AccountsMode::Sdk { .. }, None) => bail!(
                "ACCOUNTS_URL is required: the Silicon Accounts public origin (https://accounts.teamofsilicons.com in \
                 production, http://localhost:9590 for the local stack). Every access token Extend accepts must name it as `iss`."
            ),
            (AccountsMode::Local, None) => format!("{public_url}/dev/accounts"),
        };
        let accounts_api_url = match var("ACCOUNTS_API_URL") {
            Some(u) => accounts_origin("ACCOUNTS_API_URL", &u)?,
            None => accounts_url.clone(),
        };
        let app_id = var_or("EXTEND_APP_ID", extend_protocol::APP_ID).trim().to_owned();
        if app_id.is_empty()
            || !app_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("EXTEND_APP_ID must be an app id like extend, got {app_id:?}");
        }
        let webhook_secret = var("EXTEND_ACCOUNTS_WEBHOOK_SECRET");
        if production && webhook_secret.is_none() {
            bail!(
                "EXTEND_ACCOUNTS_WEBHOOK_SECRET is required in production: without it Extend refuses every Silicon Accounts \
                 webhook, so sign-outs, removed access, deleted accounts and custodian changes would never end access. \
                 Set it to the signing secret (whsec_…) of Extend's webhook in Silicon Accounts."
            );
        }
        let files = match var_or("EXTEND_FILES_MODE", if production { "briefcase" } else { "local" }).as_str() {
            "briefcase" => FilesMode::Briefcase {
                api_url: var_or("EXTEND_BRIEFCASE_URL", "https://api.briefcase.teamofsilicons.com"),
                web_url: var_or("EXTEND_BRIEFCASE_WEB_URL", "https://briefcase.teamofsilicons.com"),
            },
            "local" if production => bail!("EXTEND_FILES_MODE=local is refused in production"),
            "local" => FilesMode::Local,
            other => bail!("EXTEND_FILES_MODE must be briefcase or local, got {other:?}"),
        };
        let ting = match (
            var("EXTEND_TING_MODE").as_deref().map(str::trim),
            var("EXTEND_TING_URL"),
        ) {
            (Some("local"), _) if production => bail!("EXTEND_TING_MODE=local is refused in production"),
            (Some("local"), _) => TingMode::Local,
            (Some("off"), _) => TingMode::Off,
            (Some("ting") | None, Some(url)) => TingMode::Ting {
                base_url: url.trim().trim_end_matches('/').to_owned(),
            },
            (Some("ting"), None) => bail!("EXTEND_TING_MODE=ting needs EXTEND_TING_URL (Ting's API origin)"),
            (None, None) => TingMode::Off,
            (Some(other), _) => bail!("EXTEND_TING_MODE must be ting, local or off, got {other:?}"),
        };
        let delegation_key = var("EXTEND_DELEGATION_ENCRYPTION_KEY")
            .map(|v| crate::proofs::GrantKey::parse(&v))
            .transpose()?;
        if production && delegation_key.is_none() {
            bail!(
                "EXTEND_DELEGATION_ENCRYPTION_KEY is required in production: Extend seals the Briefcase proof refresh tokens \
                 it keeps (so a file still self-destructs after a restart) with it. Set it to 32 random bytes as unpadded base64url."
            );
        }
        if production && var("EXTEND_POSTMARK_SERVER_TOKEN").is_none() {
            bail!(
                "EXTEND_POSTMARK_SERVER_TOKEN is required in production: without it `extend report` stores \
                 bug reports that nobody is ever emailed about. Set it to a Postmark server token that may \
                 send from bugs@teamofsilicons.com."
            );
        }
        let number = |name: &str, default: i64, min: i64| -> anyhow::Result<i64> {
            match var(name) {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n >= min)
                    .with_context(|| format!("{name} must be a whole number of at least {min}, got {v:?}")),
            }
        };
        let tuning = Tuning {
            max_pairs_per_device: number("EXTEND_MAX_PAIRS_PER_DEVICE", Tuning::default().max_pairs_per_device, 1)?,
        };
        let website_url = var_or("EXTEND_WEBSITE_URL", "https://extend.teamofsilicons.com");
        let cors_origins = var("EXTEND_CORS_ORIGINS")
            .map(|raw| {
                raw.split(',')
                    .map(|o| o.trim().trim_end_matches('/').to_owned())
                    .filter(|o| !o.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            environment,
            bind,
            database_url: var("EXTEND_DATABASE_URL").context("EXTEND_DATABASE_URL is required")?,
            public_url,
            website_url,
            docs_url: var_or("EXTEND_DOCS_URL", "https://extend.teamofsilicons.com/docs"),
            repository_url: var_or(
                "EXTEND_REPOSITORY_URL",
                "https://github.com/teamofsilicons/silicon-extend",
            ),
            data_dir: PathBuf::from(var_or("EXTEND_DATA_DIR", "./data")),
            accounts_url,
            accounts_api_url,
            app_id,
            accounts,
            webhook_secret,
            webhook_previous_secret: var("EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET"),
            delegation_key,
            files,
            ting,
            postmark_token: var("EXTEND_POSTMARK_SERVER_TOKEN"),
            report_recipients: var_or(
                "EXTEND_REPORT_RECIPIENTS",
                "saketdev12@gmail.com,shubhastro2@gmail.com,bugs@teamofsilicons.com",
            )
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
            device_app_min_version: var_or("EXTEND_DEVICE_APP_MIN_VERSION", "1.0.0"),
            web_dir: var("EXTEND_WEB_DIR").map(PathBuf::from),
            trusted_proxies: var("EXTEND_TRUSTED_PROXY_CIDRS")
                .map(|raw| {
                    raw.split(',')
                        .filter(|c| !c.trim().is_empty())
                        .map(|c| {
                            c.parse::<Cidr>()
                                .map_err(|e| anyhow::anyhow!("EXTEND_TRUSTED_PROXY_CIDRS: {e}"))
                        })
                        .collect::<anyhow::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            cors_origins,
            tuning,
            obsolete: OBSOLETE_VARIABLES
                .iter()
                .filter(|v| var(v).is_some())
                .map(|v| (*v).to_owned())
                .collect(),
        })
    }

    pub fn is_production(&self) -> bool {
        self.environment == Environment::Production
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn cfg(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    const DB: (&str, &str) = ("EXTEND_DATABASE_URL", "postgres://x/y");

    #[test]
    fn an_unset_mode_is_refused_instead_of_becoming_development() {
        let err = cfg(&[DB]).unwrap_err().to_string();
        assert!(err.contains("EXTEND_ENVIRONMENT is not set"), "{err}");
        assert!(err.contains("EXTEND_ENVIRONMENT=production"), "{err}");
        let err = cfg(&[DB, ("EXTEND_ENVIRONMENT", "   ")]).unwrap_err().to_string();
        assert!(err.contains("is not set"), "{err}");
        let err = cfg(&[DB, ("EXTEND_ENVIRONMENT", "prod")]).unwrap_err().to_string();
        assert!(err.contains("must be development, test or production"), "{err}");
        assert_eq!(
            cfg(&[DB, ("EXTEND_ENVIRONMENT", "development")]).unwrap().environment,
            Environment::Development
        );
    }

    fn production(extra: &[(&'static str, &'static str)]) -> anyhow::Result<Config> {
        let mut vars = vec![
            DB,
            ("EXTEND_ENVIRONMENT", "production"),
            (
                "EXTEND_DELEGATION_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            ("EXTEND_PUBLIC_URL", "https://api.extend.teamofsilicons.com"),
            ("ACCOUNTS_URL", "https://accounts.teamofsilicons.com"),
            ("EXTEND_APP_SECRET", "sa_app_extend_x"),
            ("EXTEND_POSTMARK_SERVER_TOKEN", "pm_x"),
        ];
        vars.extend_from_slice(extra);
        cfg(&vars)
    }

    const WEBHOOK: (&str, &str) = ("EXTEND_ACCOUNTS_WEBHOOK_SECRET", "whsec_x");

    #[test]
    fn production_needs_the_accounts_webhook_secret() {
        let err = production(&[]).unwrap_err().to_string();
        assert!(
            err.contains("EXTEND_ACCOUNTS_WEBHOOK_SECRET is required in production"),
            "{err}"
        );
        let ok = production(&[WEBHOOK, ("EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET", "whsec_old")]).unwrap();
        assert_eq!(ok.webhook_secret.as_deref(), Some("whsec_x"));
        assert_eq!(ok.webhook_previous_secret.as_deref(), Some("whsec_old"));
        assert!(matches!(ok.accounts, AccountsMode::Sdk { .. }));
        assert_eq!(ok.app_id, "extend");
        assert_eq!(ok.accounts_url, "https://accounts.teamofsilicons.com");
        assert_eq!(ok.accounts_api_url, ok.accounts_url);
        assert!(matches!(
            ok.files,
            FilesMode::Briefcase { ref api_url, .. }
                if api_url == "https://api.briefcase.teamofsilicons.com"
        ));
        // Ting is off unless its URL is set.
        assert_eq!(ok.ting, TingMode::Off);
        let on = production(&[WEBHOOK, ("EXTEND_TING_URL", "https://backend.ting.teamofsilicons.com/")]).unwrap();
        assert_eq!(
            on.ting,
            TingMode::Ting {
                base_url: "https://backend.ting.teamofsilicons.com".into()
            }
        );
    }

    #[test]
    fn production_needs_accounts_and_its_app_secret() {
        let err = cfg(&[
            DB,
            ("EXTEND_ENVIRONMENT", "production"),
            ("EXTEND_PUBLIC_URL", "https://api.extend.teamofsilicons.com"),
            ("ACCOUNTS_URL", "https://accounts.teamofsilicons.com"),
        ])
        .unwrap_err()
        .to_string();
        assert!(err.contains("EXTEND_APP_SECRET is required"), "{err}");
        let err = cfg(&[
            DB,
            ("EXTEND_ENVIRONMENT", "production"),
            ("EXTEND_PUBLIC_URL", "https://api.extend.teamofsilicons.com"),
            ("EXTEND_APP_SECRET", "sa_app_extend_x"),
        ])
        .unwrap_err()
        .to_string();
        assert!(err.contains("ACCOUNTS_URL is required"), "{err}");
        let err = production(&[WEBHOOK, ("ACCOUNTS_URL", "http://accounts.example.com")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("plain http is allowed only for this machine"), "{err}");
        // The local stack: http for loopback hosts, and a separate server-to-server origin.
        let local = production(&[
            WEBHOOK,
            ("ACCOUNTS_URL", "http://localhost:9590/"),
            ("ACCOUNTS_API_URL", "http://127.0.0.1:9589"),
        ])
        .unwrap();
        assert_eq!(local.accounts_url, "http://localhost:9590");
        assert_eq!(local.accounts_api_url, "http://127.0.0.1:9589");
    }

    #[test]
    fn production_needs_the_postmark_token_and_the_delegation_key() {
        let err = cfg(&[
            DB,
            ("EXTEND_ENVIRONMENT", "production"),
            (
                "EXTEND_DELEGATION_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            ("EXTEND_PUBLIC_URL", "https://api.extend.teamofsilicons.com"),
            ("ACCOUNTS_URL", "https://accounts.teamofsilicons.com"),
            ("EXTEND_APP_SECRET", "sa_app_extend_x"),
            WEBHOOK,
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("EXTEND_POSTMARK_SERVER_TOKEN is required in production"),
            "{err}"
        );
        let err = cfg(&[
            DB,
            ("EXTEND_ENVIRONMENT", "production"),
            ("EXTEND_PUBLIC_URL", "https://api.extend.teamofsilicons.com"),
            ("ACCOUNTS_URL", "https://accounts.teamofsilicons.com"),
            ("EXTEND_APP_SECRET", "sa_app_extend_x"),
            ("EXTEND_POSTMARK_SERVER_TOKEN", "pm_x"),
            WEBHOOK,
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("EXTEND_DELEGATION_ENCRYPTION_KEY is required in production"),
            "{err}"
        );
        let dev = cfg(&[DB, ("EXTEND_ENVIRONMENT", "development")]).unwrap();
        assert!(dev.postmark_token.is_none());
        assert_eq!(dev.accounts, AccountsMode::Local);
        assert_eq!(dev.accounts_url, "http://127.0.0.1:8480/dev/accounts");
    }

    #[test]
    fn the_docker_image_defaults_to_production() {
        let dockerfile = include_str!("../../../Dockerfile");
        let runtime = dockerfile.split("AS runtime").nth(1).expect("a runtime stage");
        assert!(runtime.contains("EXTEND_ENVIRONMENT=production"), "{runtime}");
        assert!(
            !dockerfile.contains("vendor/silicon-iam-client"),
            "the image builds without the IAM client"
        );
    }

    #[test]
    fn production_refuses_every_local_stand_in() {
        for (name, value) in [
            ("EXTEND_ACCOUNTS_MODE", "local"),
            ("EXTEND_FILES_MODE", "local"),
            ("EXTEND_TING_MODE", "local"),
        ] {
            let err = production(&[WEBHOOK, (name, value)]).unwrap_err().to_string();
            assert!(err.contains("refused in production"), "{name}: {err}");
        }
    }

    #[test]
    fn variables_from_before_silicon_accounts_are_reported() {
        let c = cfg(&[
            DB,
            ("EXTEND_ENVIRONMENT", "development"),
            ("EXTEND_IAM_APP_SECRET", "ask_x"),
            ("EXTEND_HONEYCOMB_SERVICE_TOKEN", "hck_x"),
        ])
        .unwrap();
        assert_eq!(
            c.obsolete,
            vec![
                "EXTEND_IAM_APP_SECRET".to_owned(),
                "EXTEND_HONEYCOMB_SERVICE_TOKEN".to_owned()
            ]
        );
    }

    fn forwarded(values: &[&str]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn cidr_blocks_parse_and_match() {
        let net: Cidr = "172.30.87.0/24".parse().unwrap();
        assert!(net.contains("172.30.87.2".parse().unwrap()));
        assert!(net.contains("::ffff:172.30.87.9".parse().unwrap()));
        assert!(!net.contains("172.30.88.2".parse().unwrap()));
        let one: Cidr = "10.0.0.5".parse().unwrap();
        assert!(one.contains("10.0.0.5".parse().unwrap()) && !one.contains("10.0.0.6".parse().unwrap()));
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
        let v6: Cidr = "fd00::/8".parse().unwrap();
        assert!(v6.contains("fd12::1".parse().unwrap()) && !v6.contains("10.0.0.1".parse().unwrap()));
        for bad in ["172.30.87.0/33", "nope", "fd00::/129", "1.2.3.4/x"] {
            assert!(bad.parse::<Cidr>().is_err(), "{bad}");
        }
    }

    #[test]
    fn forwarded_for_is_believed_only_from_a_trusted_proxy() {
        let caddy: std::net::IpAddr = "172.30.87.3".parse().unwrap();
        let trusted = vec!["172.30.87.0/24".parse::<Cidr>().unwrap()];
        let spoofed = forwarded(&["1.1.1.1, 203.0.113.9"]);
        // No proxies configured: the peer is the client whatever the header says.
        assert_eq!(client_ip(caddy, &spoofed, &[]), caddy);
        // A peer outside the trusted block can't choose its address.
        let direct: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        assert_eq!(client_ip(direct, &spoofed, &trusted), direct);
        // Behind Caddy the right-most untrusted hop is the client; the client-written part is ignored.
        assert_eq!(
            client_ip(caddy, &spoofed, &trusted),
            "203.0.113.9".parse::<std::net::IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(caddy, &forwarded(&["1.1.1.1", "203.0.113.9, 172.30.87.4"]), &trusted),
            "203.0.113.9".parse::<std::net::IpAddr>().unwrap()
        );
        // Missing or unreadable header: fall back to the peer.
        assert_eq!(client_ip(caddy, &axum::http::HeaderMap::new(), &trusted), caddy);
        assert_eq!(client_ip(caddy, &forwarded(&["garbage"]), &trusted), caddy);
    }

    #[test]
    fn tuning_has_the_accepted_defaults_and_reads_the_environment() {
        let dev = [DB, ("EXTEND_ENVIRONMENT", "development")];
        let t = cfg(&dev).unwrap().tuning;
        assert_eq!(t, Tuning::default());
        assert_eq!(t.max_pairs_per_device, 8);
        let t = cfg(&[dev[0], dev[1], ("EXTEND_MAX_PAIRS_PER_DEVICE", "3")])
            .unwrap()
            .tuning;
        assert_eq!(t.max_pairs_per_device, 3);
        let err = cfg(&[dev[0], dev[1], ("EXTEND_MAX_PAIRS_PER_DEVICE", "0")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("EXTEND_MAX_PAIRS_PER_DEVICE"), "{err}");
    }

    #[test]
    fn trusted_proxies_come_from_the_environment() {
        let dev = [DB, ("EXTEND_ENVIRONMENT", "development")];
        assert!(cfg(&dev).unwrap().trusted_proxies.is_empty());
        let c = cfg(&[
            dev[0],
            dev[1],
            ("EXTEND_TRUSTED_PROXY_CIDRS", "172.30.87.0/24, 10.0.0.5"),
        ])
        .unwrap();
        assert_eq!(c.trusted_proxies.len(), 2);
        let err = cfg(&[dev[0], dev[1], ("EXTEND_TRUSTED_PROXY_CIDRS", "172.30.87.0/40")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("EXTEND_TRUSTED_PROXY_CIDRS"), "{err}");
    }
}
