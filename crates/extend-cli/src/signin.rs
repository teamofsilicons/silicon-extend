//! The saved sign-in: `{state}/auth.json` (mode 0600 in a 0700 directory, written atomically),
//! holding Extend's Silicon Accounts tokens for one account, and the lock that makes refreshing it
//! single-flight.
//!
//! Refresh tokens rotate, and presenting a used one revokes the whole sign-in. So a refresh happens
//! only while holding `{state}/refresh.lock` (an OS file lock, released when the process exits,
//! even if it dies), after reading the file again: if another process refreshed while this one
//! waited, its tokens are used instead. The new pair is written before it is used.
//!
//! A file Extend 3 wrote (a Silicon IAM sign-in with Teams) or one that can't be read is never used
//! and never crashes a command: the CLI says to sign in again, and replaces it then.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use silicon_extend_client::auth::{Custodian, Tokens};

use crate::store;

/// The format of `auth.json` Extend 4 writes. Extend 3's had no `format`.
pub const FORMAT: u32 = 4;
/// Refresh the access token when less than this many seconds are left.
pub const REFRESH_WITHIN_S: i64 = 60;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub format: u32,
    /// The Silicon Accounts the tokens came from (and are refreshed and revoked at).
    pub accounts_url: String,
    /// The Extend service the tokens are sent to. A command for another service doesn't use them.
    pub api_url: String,
    pub app_id: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when the access token expires.
    pub expires_at: i64,
    /// Unix seconds when the sign-in itself ends, however often it is refreshed.
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
    /// The permanent Silicon Accounts uuid.
    pub uuid: String,
    /// The public id as last seen (`c:ada`, `si:scout`); it can change.
    pub id: String,
    /// `carbon` or `silicon`.
    pub kind: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub custodian: Option<Custodian>,
    /// How this sign-in started: `device` (a Carbon approved a code) or `slt` (a short-lived token).
    pub method: String,
    /// Unix seconds.
    pub signed_in_at: i64,
    #[serde(default)]
    pub scope: Option<String>,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("accounts_url", &self.accounts_url)
            .field("api_url", &self.api_url)
            .field("uuid", &self.uuid)
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Stored {
    pub fn is_silicon(&self) -> bool {
        self.kind == "silicon"
    }

    /// Seconds the access token has left (negative once it expired).
    pub fn seconds_left(&self, now: i64) -> i64 {
        self.expires_at - now
    }

    /// The sign-in a token response starts. Fails when Silicon Accounts named no account or no
    /// refresh token, which a sign-in always has.
    pub fn from_tokens(
        t: Tokens,
        accounts_url: &str,
        api_url: &str,
        method: &str,
        now: i64,
    ) -> Result<Self, &'static str> {
        let account = t.account.clone().ok_or("the answer named no account")?;
        let refresh = t.refresh_token.clone().ok_or("the answer had no refresh token")?;
        Ok(Self {
            format: FORMAT,
            accounts_url: accounts_url.to_owned(),
            api_url: api_url.to_owned(),
            app_id: extend_protocol::APP_ID.to_owned(),
            access_token: t.access_token.expose().to_owned(),
            refresh_token: refresh.into_inner(),
            expires_at: t.expires_at.unix_timestamp(),
            refresh_expires_at: t.refresh_expires_at.map(|e| e.unix_timestamp()),
            uuid: account.uuid,
            id: account.id,
            kind: match account.kind {
                extend_protocol::model::MemberKind::Silicon => "silicon".into(),
                extend_protocol::model::MemberKind::Carbon => "carbon".into(),
            },
            display_name: account.display_name,
            custodian: account.custodian,
            method: method.to_owned(),
            signed_in_at: now,
            scope: t.scope,
        })
    }

    /// The same sign-in with refreshed tokens. The account it names must be this one.
    pub fn refreshed(&self, t: Tokens) -> Result<Self, String> {
        if let Some(a) = &t.account
            && a.uuid != self.uuid
        {
            return Err(format!(
                "Silicon Accounts answered the refresh of {}'s sign-in with another account ({}), so it wasn't saved.",
                self.id, a.id
            ));
        }
        let refresh = t
            .refresh_token
            .clone()
            .ok_or_else(|| "Silicon Accounts answered the refresh without a new refresh token.".to_owned())?;
        let mut next = self.clone();
        next.access_token = t.access_token.expose().to_owned();
        next.refresh_token = refresh.into_inner();
        next.expires_at = t.expires_at.unix_timestamp();
        if let Some(e) = t.refresh_expires_at {
            next.refresh_expires_at = Some(e.unix_timestamp());
        }
        if let Some(a) = t.account {
            next.id = a.id;
            next.display_name = a.display_name.or(next.display_name);
            next.custodian = a.custodian.or(next.custodian);
        }
        if t.scope.is_some() {
            next.scope = t.scope;
        }
        Ok(next)
    }
}

/// What `auth.json` holds.
#[derive(Debug)]
pub enum Loaded {
    Missing,
    Current(Box<Stored>),
    /// Extend 3's Silicon IAM sign-in, with the id it named.
    Legacy {
        id: Option<String>,
    },
    /// Not readable as a sign-in, and why.
    Unreadable(String),
}

pub fn path() -> PathBuf {
    store::root().join("auth.json")
}

pub fn load() -> Loaded {
    let raw = match fs::read(path()) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Unreadable(format!("reading {}: {e}", path().display())),
    };
    parse(&raw)
}

pub fn parse(raw: &[u8]) -> Loaded {
    let v: serde_json::Value = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(e) => return Loaded::Unreadable(format!("it is not JSON ({e})")),
    };
    match v.get("format").and_then(serde_json::Value::as_u64) {
        Some(f) if f == u64::from(FORMAT) => match serde_json::from_value::<Stored>(v) {
            Ok(s) => Loaded::Current(Box::new(s)),
            Err(e) => Loaded::Unreadable(format!("a field is missing or wrong ({e})")),
        },
        Some(f) => Loaded::Unreadable(format!("it is format {f}, which this CLI doesn't know")),
        None if v.get("member_id").is_some() || v.get("teams").is_some() => Loaded::Legacy {
            id: v.get("member_id").and_then(|m| m.as_str()).map(str::to_owned),
        },
        None => Loaded::Unreadable("it has no format".into()),
    }
}

/// Saves the sign-in (0600, atomic).
pub fn save(s: &Stored) -> anyhow::Result<()> {
    store::write_private(&path(), &serde_json::to_vec_pretty(s)?)
}

/// Deletes the saved sign-in. True when there was one.
pub fn remove() -> bool {
    fs::remove_file(path()).is_ok()
}

/// Whether the state directory can hold a sign-in: checked before a short-lived token is spent or
/// a code is shown, so a sign-in never ends up approved but unsaved.
pub fn check_writable() -> Result<(), String> {
    let probe = store::root().join(format!(".write-check.tmp{}", std::process::id()));
    store::write_private(&probe, b"ok").map_err(|e| format!("{e:#}"))?;
    let _ = fs::remove_file(&probe);
    Ok(())
}

/// An exclusive lock on `{state}/refresh.lock`, held until dropped (or the process ends).
pub struct FileLock {
    _file: fs::File,
}

/// Takes the refresh lock, waiting up to `wait` for another process to finish. Errs with why.
pub fn lock(wait: Duration) -> Result<FileLock, String> {
    let root = store::root();
    fs::create_dir_all(&root).map_err(|e| format!("creating {}: {e}", root.display()))?;
    let p = root.join("refresh.lock");
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let file = opts.open(&p).map_err(|e| format!("opening {}: {e}", p.display()))?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "another extend process has held {} for {} seconds while refreshing the sign-in",
                    p.display(),
                    wait.as_secs()
                ));
            }
            Err(fs::TryLockError::Error(e)) => return Err(format!("locking {}: {e}", p.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> Stored {
        Stored {
            format: FORMAT,
            accounts_url: "http://localhost:9590".into(),
            api_url: "http://127.0.0.1:4221".into(),
            app_id: "extend".into(),
            access_token: "eyJ.secret.token".into(),
            refresh_token: "sar_secret".into(),
            expires_at: 1_000,
            refresh_expires_at: Some(9_000),
            uuid: "zQo".into(),
            id: "c:ada".into(),
            kind: "carbon".into(),
            display_name: Some("Ada".into()),
            custodian: None,
            method: "device".into(),
            signed_in_at: 900,
            scope: Some("profile".into()),
        }
    }

    #[test]
    fn the_file_round_trips_and_old_or_broken_files_are_recognised() {
        let s = stored();
        match parse(&serde_json::to_vec(&s).unwrap()) {
            Loaded::Current(back) => assert_eq!(*back, s),
            other => panic!("{other:?}"),
        }
        // Extend 3's IAM sign-in.
        let old =
            br#"{"api_url":"https://api.extend.teamofsilicons.com","access_token":"oat_x","refresh_token":"ort_y",
            "expires_at":1,"member_id":"si:chef","member_kind":"silicon","teams":["acme"],"team":"acme"}"#;
        assert!(matches!(parse(old), Loaded::Legacy { id: Some(ref i) } if i == "si:chef"));
        assert!(matches!(parse(b"not json"), Loaded::Unreadable(ref w) if w.contains("not JSON")));
        assert!(matches!(parse(br#"{"format":4,"uuid":"zQo"}"#), Loaded::Unreadable(ref w) if w.contains("missing")));
        assert!(matches!(parse(br#"{"format":9}"#), Loaded::Unreadable(ref w) if w.contains("format 9")));
        assert!(matches!(parse(br#"{}"#), Loaded::Unreadable(_)));
        // Debug never shows the tokens.
        let shown = format!("{s:?}");
        assert!(!shown.contains("secret") && shown.contains("c:ada"), "{shown}");
    }

    #[test]
    fn a_refresh_keeps_the_account_and_refuses_another() {
        let s = stored();
        let tokens = |uuid: &str, id: &str| Tokens {
            access_token: silicon_extend_client::auth::Secret::new("eyJ.new"),
            refresh_token: Some(silicon_extend_client::auth::Secret::new("sar_new")),
            expires_in: 1800,
            expires_at: time::OffsetDateTime::from_unix_timestamp(5_000).unwrap(),
            refresh_expires_at: None,
            scope: None,
            account: Some(silicon_extend_client::auth::SignedInAccount {
                uuid: uuid.into(),
                id: id.into(),
                kind: extend_protocol::model::MemberKind::Carbon,
                display_name: None,
                pfp_url: None,
                custodian: None,
            }),
        };
        let next = s.refreshed(tokens("zQo", "c:ada-renamed")).unwrap();
        assert_eq!(
            (next.refresh_token.as_str(), next.expires_at, next.id.as_str()),
            ("sar_new", 5_000, "c:ada-renamed")
        );
        assert_eq!(next.refresh_expires_at, Some(9_000), "kept when the answer has none");
        assert_eq!(next.display_name.as_deref(), Some("Ada"));
        let e = s.refreshed(tokens("other", "c:eve")).unwrap_err();
        assert!(e.contains("another account (c:eve)"), "{e}");
    }
}
