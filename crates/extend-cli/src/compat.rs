//! Reading the compatibility matrix (`GET /api/v1/contracts`) for `extend version`: whether the API
//! this CLI speaks is current, deprecated or sunset, and whether this CLI's version is one it works
//! with.

use serde_json::{Value, json};

pub const UPDATE: &str = "honeycomb install 'extend'";

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    /// `current`, `deprecated`, `sunset`, `unsupported` (this CLI is outside the versions the API
    /// works with) or `unknown` (Extend couldn't be asked, or didn't say).
    pub status: &'static str,
    /// The API major's own state, as the matrix gives it.
    pub api_state: Option<String>,
    /// The CLI versions the API major works with, like `>=1.0.0, <2.0.0`.
    pub cli_range: Option<String>,
    pub deprecated_at: Option<String>,
    pub sunset_at: Option<String>,
    pub sunset_earliest_at: Option<String>,
    pub sunset_rule: Option<String>,
    /// One or two sentences: what the status is, why, and what to do.
    pub message: String,
}

impl Status {
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            status: "unknown",
            api_state: None,
            cli_range: None,
            deprecated_at: None,
            sunset_at: None,
            sunset_earliest_at: None,
            sunset_rule: None,
            message: message.into(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "api_state": self.api_state,
            "cli_range": self.cli_range,
            "deprecated_at": self.deprecated_at,
            "sunset_at": self.sunset_at,
            "sunset_earliest_at": self.sunset_earliest_at,
            "sunset_rule": self.sunset_rule,
            "message": self.message,
        })
    }
}

fn day(t: &str) -> &str {
    t.get(..10).unwrap_or(t)
}

/// What the matrix says about `api_version` and CLI version `cli`.
pub fn evaluate(matrix: &Value, api_version: u32, cli: &str) -> Status {
    let entry = matrix["versions"].as_array().and_then(|v| {
        v.iter()
            .find(|m| m["api_version"].as_u64() == Some(u64::from(api_version)))
    });
    let Some(m) = entry else {
        return Status::unknown(format!(
            "Extend's compatibility matrix doesn't list API v{api_version}, so this CLI can't tell whether it is current. \
             If commands fail with api_version errors, update with `{UPDATE}`."
        ));
    };
    let s = |v: &Value| v.as_str().map(str::to_owned);
    let api_state = s(&m["state"]);
    let cli_range = s(&m["compatible"]["cli"]);
    let deprecated_at = s(&m["deprecated_at"]);
    let sunset_at = s(&m["sunset_at"]);
    let sunset_earliest_at = s(&m["sunset_earliest_at"]);
    let sunset_rule = s(&m["sunset_rule"]).or_else(|| s(&matrix["sunset_rule"]));
    let fits = cli_range.as_deref().map(|r| satisfies(cli, r));
    let unreadable = match (&cli_range, fits) {
        (Some(r), Some(None)) => format!(" (this CLI can't read the version range {r:?} it gives)"),
        _ => String::new(),
    };
    let (status, message) = match (api_state.as_deref(), fits) {
        (Some("sunset"), _) => (
            "sunset",
            format!(
                "API v{api_version} was retired{} ({}), so this CLI can no longer use Extend. Update with `{UPDATE}`.",
                sunset_at
                    .as_deref()
                    .map(|t| format!(" on {}", day(t)))
                    .unwrap_or_default(),
                sunset_rule
                    .as_deref()
                    .unwrap_or("it went unused after it was deprecated")
            ),
        ),
        (_, Some(Some(false))) => (
            "unsupported",
            format!(
                "This CLI ({cli}) is outside the versions API v{api_version} works with ({}). Update with `{UPDATE}`.",
                cli_range.as_deref().unwrap_or_default()
            ),
        ),
        (Some("deprecated"), _) => (
            "deprecated",
            format!(
                "API v{api_version} is deprecated{}; Extend retires it after {}{}. Update with `{UPDATE}` before then.{unreadable}",
                deprecated_at
                    .as_deref()
                    .map(|t| format!(" since {}", day(t)))
                    .unwrap_or_default(),
                sunset_rule
                    .as_deref()
                    .map(|r| r.trim_start_matches("Sunset after ").to_owned())
                    .unwrap_or_else(|| "a quiet period".into()),
                sunset_earliest_at
                    .as_deref()
                    .map(|t| format!(", no earlier than {}", day(t)))
                    .unwrap_or_default()
            ),
        ),
        (Some("current"), _) => (
            "current",
            match (&cli_range, fits) {
                (Some(r), Some(Some(true))) => {
                    format!("API v{api_version} is current, and it works with CLI {r}, which includes {cli}.")
                }
                _ => format!("API v{api_version} is current.{unreadable}"),
            },
        ),
        (other, _) => (
            "unknown",
            format!(
                "Extend gives API v{api_version} the state {:?}, which this CLI doesn't know. Update with `{UPDATE}`.",
                other.unwrap_or("(none)")
            ),
        ),
    };
    Status {
        status,
        api_state,
        cli_range,
        deprecated_at,
        sunset_at,
        sunset_earliest_at,
        sunset_rule,
        message,
    }
}

type Version = (u64, u64, u64);

fn parse_version(v: &str) -> Option<(Version, usize)> {
    let core = v.trim().split(['-', '+']).next()?;
    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut n = [0u64; 3];
    for (i, p) in parts.iter().enumerate() {
        n[i] = p.parse().ok()?;
    }
    Some(((n[0], n[1], n[2]), parts.len()))
}

/// Whether `version` is in a Cargo-style range (`>=1.0.0, <2.0.0`, `^1.2`, `~1.2.3`, `=1.0.0`,
/// `*`). `None` when the range can't be read.
pub fn satisfies(version: &str, range: &str) -> Option<bool> {
    let (v, _) = parse_version(version)?;
    let mut all = true;
    for comparator in range.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        if comparator == "*" {
            continue;
        }
        let (op, rest) = [">=", "<=", ">", "<", "=", "^", "~"]
            .iter()
            .find_map(|op| comparator.strip_prefix(op).map(|r| (*op, r)))
            .unwrap_or(("^", comparator));
        let (b, parts) = parse_version(rest)?;
        let ok = match op {
            ">=" => v >= b,
            "<=" => v <= b,
            ">" => v > b,
            "<" => v < b,
            "=" => v == b,
            "~" => {
                let upper = if parts == 1 { (b.0 + 1, 0, 0) } else { (b.0, b.1 + 1, 0) };
                v >= b && v < upper
            }
            _ => {
                let upper = if b.0 > 0 || parts == 1 {
                    (b.0 + 1, 0, 0)
                } else if b.1 > 0 || parts == 2 {
                    (0, b.1 + 1, 0)
                } else {
                    (0, 0, b.2 + 1)
                };
                v >= b && v < upper
            }
        };
        all &= ok;
    }
    Some(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(state: &str, cli: &str) -> Value {
        json!({
            "service_version": "1.0.0", "current": 1, "supported": [1],
            "sunset_rule": "Sunset after 7 consecutive days with zero requests",
            "versions": [{
                "api_version": 1, "state": state,
                "deprecated_at": if state == "current" { Value::Null } else { json!("2026-09-01T00:00:00Z") },
                "sunset_at": if state == "sunset" { json!("2026-09-20T00:00:00Z") } else { Value::Null },
                "sunset_earliest_at": if state == "deprecated" { json!("2026-10-04T00:00:00Z") } else { Value::Null },
                "sunset_rule": "Sunset after 7 consecutive days with zero requests",
                "compatible": {"client_crate": cli, "cli": cli, "device_app_min": "1.0.0"},
            }],
        })
    }

    #[test]
    fn ranges() {
        assert_eq!(satisfies("1.0.0", ">=1.0.0, <2.0.0"), Some(true));
        assert_eq!(satisfies("2.0.0", ">=1.0.0, <2.0.0"), Some(false));
        assert_eq!(satisfies("0.9.9", ">=1.0.0, <2.0.0"), Some(false));
        assert_eq!(satisfies("1.4.2", "^1.2"), Some(true));
        assert_eq!(satisfies("0.3.1", "^0.2"), Some(false));
        assert_eq!(satisfies("1.2.9", "~1.2.3"), Some(true));
        assert_eq!(satisfies("1.3.0", "~1.2.3"), Some(false));
        assert_eq!(satisfies("1.0.0", "=1.0.0"), Some(true));
        assert_eq!(satisfies("1.0.0-beta.1", "1"), Some(true));
        assert_eq!(satisfies("1.0.0", "*"), Some(true));
        assert_eq!(satisfies("1.0.0", "latest"), None);
    }

    #[test]
    fn current_deprecated_sunset_and_unsupported() {
        let s = evaluate(&matrix("current", ">=1.0.0, <2.0.0"), 1, "1.0.0");
        assert_eq!(s.status, "current");
        assert_eq!(
            s.message,
            "API v1 is current, and it works with CLI >=1.0.0, <2.0.0, which includes 1.0.0."
        );

        let s = evaluate(&matrix("deprecated", ">=1.0.0, <2.0.0"), 1, "1.0.0");
        assert_eq!(s.status, "deprecated");
        assert_eq!(
            s.message,
            "API v1 is deprecated since 2026-09-01; Extend retires it after 7 consecutive days with zero requests, \
             no earlier than 2026-10-04. Update with `honeycomb install 'extend'` before then."
        );

        let s = evaluate(&matrix("sunset", ">=1.0.0, <2.0.0"), 1, "1.0.0");
        assert_eq!(s.status, "sunset");
        assert!(
            s.message.starts_with("API v1 was retired on 2026-09-20"),
            "{}",
            s.message
        );

        let s = evaluate(&matrix("current", ">=1.1.0, <2.0.0"), 1, "1.0.0");
        assert_eq!(s.status, "unsupported");
        assert!(
            s.message
                .contains("This CLI (1.0.0) is outside the versions API v1 works with (>=1.1.0, <2.0.0)"),
            "{}",
            s.message
        );

        assert_eq!(evaluate(&matrix("current", ">=1.0.0"), 2, "1.0.0").status, "unknown");
        // The matrix as it was before it listed a lifecycle: still readable.
        let old =
            json!({"versions": [{"api_version": 1, "state": "current", "compatible": {"cli": ">=1.0.0, <2.0.0"}}]});
        assert_eq!(evaluate(&old, 1, "1.0.0").status, "current");
    }
}
