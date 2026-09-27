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
    Sdk {
        base_url: String,
        app_id: String,
        app_secret: String,
    },
    /// Local development and tests: members are named in `EXTEND_LOCAL_MEMBERS`. Refused in production.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TingMode {
    Ting {
        base_url: String,
    },
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
    /// Reverse proxies whose `X-Forwarded-For` is believed (`EXTEND_TRUSTED_PROXY_CIDRS`). Empty:
    /// the TCP peer is the client.
    pub trusted_proxies: Vec<Cidr>,
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
        // the local IAM, Briefcase and Ting stand-ins (and member-id logins) on a real deployment.
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
                 EXTEND_ENVIRONMENT=development to run with the local IAM, Briefcase and Ting stand-ins \
                 (e2e/dev.env does)."
            ),
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
            "ting" => TingMode::Ting {
                base_url: var_or("EXTEND_TING_URL", "https://backend.ting.teamofsilicons.com"),
            },
            "local" if production => bail!("EXTEND_TING_MODE=local is refused in production"),
            "local" => TingMode::Local,
            other => bail!("EXTEND_TING_MODE must be ting or local, got {other:?}"),
        };
        let secret = |name: &str, version: &str| -> anyhow::Result<Option<(i64, String)>> {
            match var(name) {
                None => Ok(None),
                Some(s) => {
                    let v = var_or(version, "1")
                        .parse::<i64>()
                        .with_context(|| format!("{version} must be an integer"))?;
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
        let webhook_secret = secret("EXTEND_IAM_WEBHOOK_SECRET", "EXTEND_IAM_WEBHOOK_SECRET_VERSION")?;
        if production && webhook_secret.is_none() {
            bail!(
                "EXTEND_IAM_WEBHOOK_SECRET is required in production: without it Extend refuses every signed \
                 IAM event, so logouts and Team removals would never end access. Set it to the webhook \
                 signing secret IAM shows for Extend's webhook endpoint (and EXTEND_IAM_WEBHOOK_SECRET_VERSION \
                 to its key version)."
            );
        }
        let bind = var_or("EXTEND_BIND", "127.0.0.1:8480")
            .parse()
            .context("EXTEND_BIND must be host:port")?;
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
            repository_url: var_or(
                "EXTEND_REPOSITORY_URL",
                "https://github.com/teamofsilicons/silicon-extend",
            ),
            data_dir: PathBuf::from(var_or("EXTEND_DATA_DIR", "./data")),
            iam_public_url: match &iam {
                IamMode::Sdk { base_url, .. } => var("EXTEND_IAM_PUBLIC_URL").unwrap_or_else(|| base_url.clone()),
                IamMode::Local => var_or("EXTEND_IAM_PUBLIC_URL", "http://127.0.0.1:8480/dev/iam"),
            },
            iam_login_url: match &iam {
                IamMode::Sdk { .. } => var_or("EXTEND_IAM_LOGIN_URL", "https://auth.iam.teamofsilicons.com/login"),
                IamMode::Local => var("EXTEND_IAM_LOGIN_URL").unwrap_or_else(|| {
                    format!("{}/dev/iam/login", var_or("EXTEND_PUBLIC_URL", "http://127.0.0.1:8480"))
                }),
            },
            iam,
            webhook_secret,
            webhook_previous_secret: secret(
                "EXTEND_IAM_WEBHOOK_PREVIOUS_SECRET",
                "EXTEND_IAM_WEBHOOK_PREVIOUS_SECRET_VERSION",
            )?,
            files,
            ting,
            honeycomb_service_token: var("EXTEND_HONEYCOMB_SERVICE_TOKEN"),
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
            local_members,
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
            ("EXTEND_PUBLIC_URL", "https://backend.extend.teamofsilicons.com"),
            ("EXTEND_IAM_APP_ID", "extend"),
            ("EXTEND_IAM_APP_SECRET", "ask_x"),
        ];
        vars.extend_from_slice(extra);
        cfg(&vars)
    }

    #[test]
    fn production_needs_the_iam_webhook_secret() {
        let err = production(&[]).unwrap_err().to_string();
        assert!(
            err.contains("EXTEND_IAM_WEBHOOK_SECRET is required in production"),
            "{err}"
        );
        let ok = production(&[
            ("EXTEND_IAM_WEBHOOK_SECRET", "whsec"),
            ("EXTEND_IAM_WEBHOOK_SECRET_VERSION", "3"),
        ])
        .unwrap();
        assert_eq!(ok.webhook_secret, Some((3, "whsec".into())));
        assert!(matches!(ok.iam, IamMode::Sdk { .. }));
    }

    #[test]
    fn production_refuses_every_local_stand_in() {
        let secret = ("EXTEND_IAM_WEBHOOK_SECRET", "whsec");
        for (name, value) in [
            ("EXTEND_IAM_MODE", "local"),
            ("EXTEND_FILES_MODE", "local"),
            ("EXTEND_TING_MODE", "local"),
        ] {
            let err = production(&[secret, (name, value)]).unwrap_err().to_string();
            assert!(err.contains("refused in production"), "{name}: {err}");
        }
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
