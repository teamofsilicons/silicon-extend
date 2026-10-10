//! Signing in with Silicon Accounts, run through the real `extend` binary against a fake that is
//! both the Extend service and Silicon Accounts: discovery in an empty home (the Silicon Apps
//! contract), the device flow, short-lived tokens, the saved sign-in (mode, refresh single-flight,
//! a refused refresh), signing out, and the files Extend 3 left behind.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Every file under `dir`.
fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_in(&p));
            } else {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

// ───────────────────────────── Discovery (Silicon Apps' three checks) ─────────────────────────────

#[test]
fn discovery_answers_in_an_empty_home_with_nothing_written() {
    let empty = std::env::temp_dir().join(format!("extend-empty-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(bin())
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &empty)
            .env("SILICON_HOME", &empty)
            .current_dir(&empty)
            .output()
            .unwrap()
    };
    let o = run(&["--help"]);
    assert_eq!(o.status.code(), Some(0));
    let help = stdout(&o);
    assert!(
        help.contains("silicon-apps install extend") && help.contains("extend login --slt-stdin"),
        "{help}"
    );
    for banned in [
        "IAM",
        "Honeycomb",
        "honeycomb",
        "--team",
        "--test",
        "organization",
        "Team ",
    ] {
        assert!(!help.contains(banned), "--help mentions {banned:?}:\n{help}");
    }

    let o = run(&["accounts", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let v = json_out(&o);
    let golden = json!({
        "app_id": "extend",
        "accounts_url": "https://accounts.teamofsilicons.com",
        "api_url": "https://backend.extend.teamofsilicons.com",
        "version": env!("CARGO_PKG_VERSION"),
        "client_id": "extend",
        "device_flow": true,
        "public_client": true,
        "sign_in": {"carbon": "extend login", "silicon": "silicon-accounts login --app extend -q | extend login --slt-stdin"},
        "status": "extend login status --json",
        "website_url": "https://extend.teamofsilicons.com",
        "docs_url": "https://extend.teamofsilicons.com/docs",
        "repository_url": "https://github.com/teamofsilicons/silicon-extend",
        "package_url": "https://crates.io/crates/silicon-extend-client",
        "install": "silicon-apps install extend",
        "update": "silicon-apps update extend",
    });
    assert_eq!(v, golden);
    // The hidden alias the Silicon runtime still runs answers the same, and isn't in the help.
    let o = run(&["iam", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(json_out(&o), golden);
    assert!(!help.contains("  iam "), "{help}");

    let o = run(&["login", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(json_out(&o), json!({"authenticated": false}));
    let o = run(&["login", "status"]);
    assert_eq!(o.status.code(), Some(1));

    assert!(files_in(&empty).is_empty(), "discovery wrote {:?}", files_in(&empty));
    let _ = std::fs::remove_dir_all(&empty);
}

// ───────────────────────────── The device flow (Carbons) ─────────────────────────────

/// A fake whose device-code polls answer `polls` in turn, then the Carbon's tokens.
fn device_fake(polls: Vec<&'static str>, interval: u64) -> (Fake, Arc<Mutex<Vec<&'static str>>>) {
    let polls = Arc::new(Mutex::new(polls));
    let p2 = polls.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/v1/device/authorize") => Some(raw_json(
            200,
            json!({"device_code": "sad_fake_device_code", "user_code": "MVHB-KQAW", "verification_uri": "http://accounts.test/device",
                   "verification_uri_complete": "http://accounts.test/device?code=MVHB-KQAW", "expires_in": 600, "interval": interval,
                   "expires_at": "2026-10-10T10:10:00Z"}),
        )),
        ("POST", "/v1/oauth/token")
            if r.form("grant_type").as_deref() == Some("urn:ietf:params:oauth:grant-type:device_code") =>
        {
            let mut p = p2.lock().unwrap();
            let next = if p.is_empty() { "tokens" } else { p.remove(0) };
            Some(match next {
                "pending" => oauth_error(
                    "authorization_pending",
                    "The Carbon hasn't approved this device code yet.",
                ),
                "slow_down" => oauth_error("slow_down", "Polling too fast."),
                "denied" => oauth_error("access_denied", "The Carbon denied it."),
                "expired" => oauth_error("expired_token", "The device code expired."),
                _ => raw_json(200, tokens("c:alice", "device-access", "sar_device")),
            })
        }
        ("GET", "/api/v2/me") => Some(ok(
            "me",
            json!({"uuid": "aLiCe", "id": "c:alice", "type": "carbon", "display_name": "Alice"}),
        )),
        _ => None,
    });
    (fake, polls)
}

#[test]
fn a_carbon_signs_in_by_approving_a_code() {
    let (fake, _) = device_fake(vec!["pending", "slow_down"], 1);
    let cli = Cli::new("device-flow", &fake.url);
    let started = Instant::now();
    let o = cli.run(&["login", "--json", "--label", "test box"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        started.elapsed() >= Duration::from_secs(7),
        "interval 1, then 6 after slow_down: {:?}",
        started.elapsed()
    );
    let lines: Vec<Value> = stdout(&o).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["event"], "device_code");
    assert_eq!(lines[0]["user_code"], "MVHB-KQAW");
    assert_eq!(lines[0]["verification_uri"], "http://accounts.test/device");
    assert_eq!(lines[1], json!({"event": "slow_down", "interval": 6}));
    let last = lines.last().unwrap();
    assert_eq!(last["event"], "signed_in");
    assert_eq!(last["authenticated"], true);
    assert_eq!(
        (last["uuid"].as_str(), last["id"].as_str(), last["kind"].as_str()),
        (Some("aLiCe"), Some("c:alice"), Some("carbon"))
    );
    assert_eq!(last["method"], "device");
    assert_eq!(last["verified"], true);
    assert_eq!(last["display_name"], "Alice", "picked up from Extend's GET /api/v2/me");

    // What was sent: client_id alone, the label, and nothing secret.
    let authorize = &fake.requests("POST", "/v1/device/authorize")[0];
    assert_eq!(authorize.body["client_id"], "extend");
    assert_eq!(authorize.body["client_label"], "test box");
    for poll in fake.requests("POST", "/v1/oauth/token") {
        assert_eq!(poll.form("client_id").as_deref(), Some("extend"));
        assert!(poll.form("client_secret").is_none() && !poll.headers.contains_key("authorization"));
    }
    // The saved sign-in: 0600 in a 0700 directory, Extend's tokens, the account.
    let auth = cli.auth();
    assert_eq!(
        (
            auth["format"].as_i64(),
            auth["uuid"].as_str(),
            auth["refresh_token"].as_str()
        ),
        (Some(4), Some("aLiCe"), Some("sar_device"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&cli.state().join("auth.json")), 0o600);
        assert_eq!(mode(&cli.state()), 0o700);
    }
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(json_out(&o)["id"], "c:alice");
    assert_eq!(json_out(&o)["verified"], true);
}

#[test]
fn a_denied_or_expired_code_is_not_a_sign_in() {
    for (poll, reason, says) in [
        ("denied", "device_denied", "was denied"),
        ("expired", "device_expired", "expired before anyone approved it"),
    ] {
        let (fake, polls) = device_fake(vec![poll], 1);
        let cli = Cli::new(&format!("device-{poll}"), &fake.url);
        let o = cli.run(&["login"]);
        assert_eq!(o.status.code(), Some(3), "{poll}: {}", stderr(&o));
        let err = stderr(&o);
        assert!(
            err.contains("To sign in to Extend, open http://accounts.test/device") && err.contains("MVHB-KQAW"),
            "{err}"
        );
        assert!(err.contains(says) && err.contains("extend login"), "{poll}: {err}");
        assert!(!cli.state().join("auth.json").exists());
        polls.lock().unwrap().push(poll);
        let o = cli.run(&["login", "--json"]);
        assert_eq!(json_err(&o)["error"]["details"]["reason"], reason);
        assert_eq!(json_err(&o)["error"]["code"], "not_signed_in");
    }
}

// ───────────────────────────── Short-lived tokens (Silicons) ─────────────────────────────

/// A fake Silicon Accounts that knows a few short-lived tokens.
fn slt_fake() -> Fake {
    Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/v1/oauth/token")
            if r.form("grant_type").as_deref() == Some("urn:silicon:params:oauth:grant-type:slt") =>
        {
            Some(match r.form("slt").as_deref() {
                Some("slt_good_one") | Some("slt_good_two") | Some("slt_good_three") => {
                    raw_json(200, tokens("si:chef", "slt-access", "sar_from_slt"))
                }
                Some("slt_used_already") => oauth_error(
                    "invalid_grant",
                    "The short-lived token was already used at 2026-10-10T10:00:00Z.",
                ),
                Some("slt_expired_tok") => oauth_error(
                    "invalid_grant",
                    "The short-lived token expired at 2026-10-10T10:00:00Z.",
                ),
                Some("slt_for_remind") => oauth_error(
                    "invalid_grant",
                    "The short-lived token was issued for the app 'remind', not for 'extend'.",
                ),
                _ => oauth_error("invalid_grant", "The short-lived token is not known."),
            })
        }
        ("GET", "/api/v2/me") => Some(ok("me", me("si:chef"))),
        _ => None,
    })
}

#[test]
fn a_silicon_signs_in_with_a_short_lived_token_three_ways() {
    let fake = slt_fake();
    let cli = Cli::new("slt-ways", &fake.url);
    let o = cli.run_stdin(&["login", "--slt-stdin"], "slt_good_one\n");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o).lines().next().unwrap(),
        "Signed in to Extend as si:chef, a Silicon looked after by c:alice."
    );
    let o = cli.run(&["login", "--slt", "slt_good_two", "--json"]);
    assert_eq!(json_out(&o)["method"], "slt");
    assert_eq!(json_out(&o)["custodian"]["id"], "c:alice");
    // The positional form the Silicon runtime uses.
    let o = cli.run(&["login", "slt_good_three"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));

    let exchanges = fake.requests("POST", "/v1/oauth/token");
    assert_eq!(exchanges.len(), 3);
    for x in &exchanges {
        assert_eq!(x.form("client_id").as_deref(), Some("extend"));
        assert!(
            x.form("client_secret").is_none() && !x.headers.contains_key("authorization"),
            "no secret, ever"
        );
    }
    // Signing in again as the same account replaces the sign-in without ending it (that would end
    // its running sessions); nothing was revoked.
    assert!(fake.requests("POST", "/v1/oauth/revoke").is_empty());
    // The token never lands in a file.
    for f in files_in(&cli.home) {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        assert!(!text.contains("slt_good"), "{f} holds the short-lived token");
    }
}

#[test]
fn every_refused_token_says_why_and_how_to_get_a_fresh_one() {
    let fake = slt_fake();
    let cli = Cli::new("slt-refused", &fake.url);
    for (slt, reason, says) in [
        ("slt_used_already", "slt_already_used", "was already used"),
        ("slt_expired_tok", "slt_expired", "expired at"),
        ("slt_for_remind", "slt_wrong_app", "issued for the app 'remind'"),
        ("slt_nobody_knows", "slt_unknown", "is not known"),
    ] {
        let o = cli.run_stdin(&["login", "--slt-stdin", "--json"], slt);
        assert_eq!(o.status.code(), Some(3), "{slt}: {}", stderr(&o));
        let e = json_err(&o);
        assert_eq!(e["error"]["code"], "slt_invalid", "{slt}");
        assert_eq!(e["error"]["details"]["reason"], reason, "{slt}");
        assert!(e["error"]["message"].as_str().unwrap().contains(says), "{slt}: {e}");
        assert!(
            e["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("silicon-accounts login --app extend -q"),
            "{slt}: {e}"
        );
        assert!(
            !stderr(&o).contains(slt) && !stdout(&o).contains(slt),
            "{slt} was echoed"
        );
    }
    // Not a short-lived token at all: refused here, nothing sent.
    let sent = fake.requests("POST", "/v1/oauth/token").len();
    let o = cli.run(&["login", "--slt", "sar_a_refresh_token"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        stderr(&o).contains("it is a refresh token") && stderr(&o).contains("Nothing was sent"),
        "{}",
        stderr(&o)
    );
    assert_eq!(fake.requests("POST", "/v1/oauth/token").len(), sent);
    // One way at a time, and the device-flow flags don't go with a token.
    assert_eq!(cli.run(&["login", "slt_a", "--slt", "slt_b"]).status.code(), Some(2));
    assert_eq!(cli.run(&["login", "--slt", "slt_a", "--open"]).status.code(), Some(2));
    assert_eq!(cli.run_stdin(&["login", "--slt-stdin"], "").status.code(), Some(2));
    assert!(!cli.state().join("auth.json").exists());
}

#[test]
fn a_token_is_never_spent_where_the_sign_in_could_not_work_or_be_saved() {
    // The Extend service trusts another Silicon Accounts.
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/accounts") => Some(ok(
            "accounts",
            json!({"app_id": "extend", "accounts_url": "https://accounts.teamofsilicons.com", "api_base_url": "x",
                   "website_url": "w", "docs_url": "d", "repository_url": "r", "ting_enabled": false}),
        )),
        _ => None,
    });
    let cli = Cli::new("mismatch", &fake.url);
    let o = cli.run(&["login", "--slt", "slt_good_one", "--json"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    let e = json_err(&o);
    assert_eq!(e["error"]["details"]["reason"], "accounts_mismatch");
    assert!(
        e["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("ACCOUNTS_URL=https://accounts.teamofsilicons.com"),
        "{e}"
    );
    assert!(
        fake.requests("POST", "/v1/oauth/token").is_empty(),
        "the token wasn't spent"
    );

    // A state directory this user can't write.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let fake = slt_fake();
        let cli = Cli::new("unwritable", &fake.url);
        // A home this user can't create .extend in.
        std::fs::remove_dir_all(cli.state()).unwrap();
        std::fs::set_permissions(&cli.home, std::fs::Permissions::from_mode(0o500)).unwrap();
        let o = cli.run(&["login", "--slt", "slt_good_one"]);
        std::fs::set_permissions(&cli.home, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
        assert!(
            stderr(&o).contains("can't save a sign-in") && stderr(&o).contains("Nothing was sent"),
            "{}",
            stderr(&o)
        );
        assert!(fake.requests("POST", "/v1/oauth/token").is_empty());
    }

    // Plain http to another machine.
    let cli = Cli::new("insecure", "http://127.0.0.1:9").env("ACCOUNTS_URL", "http://accounts.example.com");
    let o = cli.run(&["login", "--slt", "slt_good_one"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("unencrypted"), "{}", stderr(&o));
}

// ───────────────────────────── The saved sign-in: refresh ─────────────────────────────

/// A fake that rotates refresh tokens like Silicon Accounts: each works once, and presenting a used
/// one is refused (and would revoke the sign-in). It counts the refreshes.
fn rotating_fake(refreshes: Arc<AtomicUsize>, refuse_device_list_once: bool) -> Fake {
    let valid: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::from([(
        "sar_old".to_owned(),
        "sar_new".to_owned(),
    )])));
    let refused = Arc::new(Mutex::new(refuse_device_list_once));
    Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/v1/oauth/token") if r.form("grant_type").as_deref() == Some("refresh_token") => {
            std::thread::sleep(Duration::from_millis(300));
            refreshes.fetch_add(1, Ordering::SeqCst);
            let rt = r.form("refresh_token").unwrap_or_default();
            Some(match valid.lock().unwrap().remove(&rt) {
                Some(next) => raw_json(200, tokens("c:alice", "fresh-access", &next)),
                None => oauth_error(
                    "invalid_grant",
                    "This refresh token was already used once. Presenting a used refresh token revokes the whole sign-in to protect the account, so this sign-in is now revoked; sign in again.",
                ),
            })
        }
        ("GET", "/api/v2/devices") => {
            let mut once = refused.lock().unwrap();
            if *once && r.headers.get("authorization").map(String::as_str) == Some("Bearer still-valid-access") {
                *once = false;
                return Some(err(401, "token_expired", "The access token expired."));
            }
            Some(ok(
                "devices",
                json!({"items": [device("7c1e09ab", "box", true)], "next_cursor": null}),
            ))
        }
        _ => None,
    })
}

#[test]
fn a_stale_token_is_refreshed_once_even_by_concurrent_commands() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let fake = rotating_fake(refreshes.clone(), false);
    let cli = Cli::new("refresh-concurrent", &fake.url).signed_in_with("c:alice", "old-access", "sar_old", now() + 20);
    // Four commands at once, each finding less than a minute left on the access token.
    let children: Vec<_> = (0..4)
        .map(|_| {
            cli.command(&["device", "ls", "--json"], &cli.home)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    }
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "one refresh for everyone; a second would revoke the sign-in"
    );
    let auth = cli.auth();
    assert_eq!(
        (auth["refresh_token"].as_str(), auth["access_token"].as_str()),
        (Some("sar_new"), Some("fresh-access"))
    );
    for r in fake.requests("GET", "/api/v2/devices") {
        assert_eq!(r.headers["authorization"], "Bearer fresh-access");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(cli.state().join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn a_refused_token_is_refreshed_and_the_call_repeated() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let fake = rotating_fake(refreshes.clone(), true);
    let cli =
        Cli::new("refresh-on-401", &fake.url).signed_in_with("c:alice", "still-valid-access", "sar_old", now() + 900);
    let o = cli.run(&["device", "ls", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    let calls = fake.requests("GET", "/api/v2/devices");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].headers["authorization"], "Bearer fresh-access");
}

#[test]
fn a_refused_refresh_ends_the_sign_in_and_says_so() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let fake = rotating_fake(refreshes, false);
    let cli = Cli::new("refresh-refused", &fake.url).signed_in_with("si:chef", "old-access", "sar_spent", now() - 10);
    let o = cli.run(&["device", "ls", "--json"]);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    let e = json_err(&o);
    assert_eq!(e["error"]["code"], "token_expired");
    assert_eq!(e["error"]["details"]["reason"], "sign_in_ended");
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Your sign-in to Extend has ended: This refresh token was already used once"),
        "{e}"
    );
    assert!(e["error"]["hint"].as_str().unwrap().contains("--slt-stdin"), "{e}");
    assert!(!cli.state().join("auth.json").exists(), "an ended sign-in is deleted");
    assert!(
        fake.requests("GET", "/api/v2/devices").is_empty(),
        "an expired token was never sent"
    );
    assert_eq!(
        json_out(&cli.run(&["login", "status", "--json"])),
        json!({"authenticated": false})
    );
}

#[test]
fn login_status_checks_with_extend_unless_offline() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/me") => Some(err(503, "service_unavailable", "Extend is restarting.")),
        _ => None,
    });
    let cli = Cli::new("status-offline", &fake.url).signed_in("si:chef");
    let o = cli.run(&["login", "status", "--offline", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json_out(&o);
    assert_eq!(
        (v["authenticated"].as_bool(), v["verified"].as_bool()),
        (Some(true), Some(false))
    );
    assert!(fake.requests("GET", "/api/v2/me").is_empty(), "--offline sends nothing");
    for key in [
        "uuid",
        "id",
        "kind",
        "expires_at",
        "refresh_expires_at",
        "custodian",
        "method",
    ] {
        assert!(v.get(key).is_some(), "no {key} in {v}");
    }
    // Extend can't be asked: still signed in (from the file), not verified, and why.
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json_out(&o);
    assert_eq!(
        (v["authenticated"].as_bool(), v["verified"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(v["verify_error"]["code"], "service_unavailable");
}

// ───────────────────────────── Signing out ─────────────────────────────

#[test]
fn logout_signs_out_at_extend_or_else_at_silicon_accounts() {
    let fail_extend = Arc::new(Mutex::new(false));
    let f2 = fail_extend.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/sessions") => Some(ok(
            "sessions",
            json!({"items": [session("a3f", "active", &[])], "next_cursor": null}),
        )),
        ("POST", "/api/v2/auth/logout") if *f2.lock().unwrap() => Some(err(
            503,
            "service_unavailable",
            "Extend couldn't reach Silicon Accounts.",
        )),
        ("POST", "/api/v2/auth/logout") => Some(no_content()),
        ("POST", "/v1/oauth/revoke") => Some(raw_json(200, json!({"revoked": true}))),
        _ => None,
    });
    let cli = Cli::new("logout", &fake.url)
        .signed_in("si:chef")
        .connected("a3f", &["snapshot"]);
    let sessions = cli.session_dir();
    assert!(sessions.join("current").exists());
    let o = cli.run(&["logout"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "Signed out si:chef. Ended session a3f.");
    let sent = &fake.requests("POST", "/api/v2/auth/logout")[0];
    assert_eq!(
        sent.body,
        json!({"type": "logout", "data": {"refresh_token": "sar_test-refresh"}})
    );
    assert_eq!(sent.headers["authorization"], "Bearer test-access");
    assert!(!cli.state().join("auth.json").exists());
    assert!(!sessions.exists(), "its cached sessions are forgotten");
    // Signed out already: nothing to do, still exit 0.
    let o = cli.run(&["logout", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(json_out(&o)["signed_out"], false);

    // Extend can't do it: the CLI revokes at Silicon Accounts itself, as Extend's public client.
    *fail_extend.lock().unwrap() = true;
    let cli = Cli::new("logout-fallback", &fake.url).signed_in("c:alice");
    let o = cli.run(&["logout", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let v = json_out(&o);
    assert_eq!(
        (v["signed_out"].as_bool(), v["revoked"].as_bool(), v["via"].as_str()),
        (Some(true), Some(true), Some("silicon-accounts"))
    );
    let revoke = &fake.requests("POST", "/v1/oauth/revoke")[0];
    assert_eq!(
        (
            revoke.form("token").as_deref(),
            revoke.form("token_type_hint").as_deref(),
            revoke.form("client_id").as_deref()
        ),
        (Some("sar_test-refresh"), Some("refresh_token"), Some("extend"))
    );
    assert!(!revoke.headers.contains_key("authorization"));
    assert!(!cli.state().join("auth.json").exists());
}

// ───────────────────────────── Files Extend 3 left, and other origins ─────────────────────────────

#[test]
fn an_extend_3_sign_in_is_never_used_and_is_replaced_at_the_next_sign_in() {
    let fake = slt_fake();
    let cli = Cli::new("legacy", &fake.url);
    std::fs::write(
        cli.state().join("auth.json"),
        json!({"api_url": fake.url, "access_token": "oat_old", "refresh_token": "ort_old", "expires_at": 4_102_444_800i64,
               "member_id": "si:chef", "member_kind": "silicon", "teams": ["acme"], "team": "acme"}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(cli.state().join("contexts/production")).unwrap();
    std::fs::write(cli.state().join("contexts/production/abc.json"), "{}").unwrap();
    std::fs::create_dir_all(cli.state().join("test")).unwrap();

    let o = cli.run(&["device", "ls"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        stderr(&o).contains("is from Extend 3, which Extend 4 no longer accepts") && stderr(&o).contains("--slt-stdin"),
        "{}",
        stderr(&o)
    );
    assert!(
        fake.seen
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.headers.contains_key("authorization")),
        "the old token was never sent"
    );
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(json_out(&o)["authenticated"], false);
    assert!(json_out(&o)["reason"].as_str().unwrap().contains("Extend 3"));

    let o = cli.run_stdin(&["login", "--slt-stdin"], "slt_good_one");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("Removed the sign-ins Extend 3 saved here"),
        "{}",
        stdout(&o)
    );
    assert_eq!(cli.auth()["format"], 4);
    assert!(!cli.state().join("contexts").exists() && !cli.state().join("test").exists());

    // An unreadable file doesn't crash anything either.
    std::fs::write(cli.state().join("auth.json"), "{not json").unwrap();
    let o = cli.run(&["device", "ls"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("can't be read"), "{}", stderr(&o));
    let o = cli.run(&["logout"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(!cli.state().join("auth.json").exists());
}

#[test]
fn a_sign_in_is_only_sent_to_the_extend_it_was_made_for() {
    let fake = Fake::start(|r| {
        (r.path_only() == "/api/v2/devices").then(|| ok("devices", json!({"items": [], "next_cursor": null})))
    });
    let mut cli = Cli::new("origin-isolation", "https://old.example").signed_in("c:alice");
    cli.url = fake.url.clone();
    cli.accounts = fake.url.clone();
    let o = cli.run(&["device", "ls"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        stderr(&o).contains("for the Extend at https://old.example"),
        "{}",
        stderr(&o)
    );
    assert!(
        fake.seen
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.headers.contains_key("authorization"))
    );
}

#[test]
fn signing_in_as_another_account_signs_the_previous_one_out() {
    let fake = slt_fake();
    let fake_url = fake.url.clone();
    let cli = Cli::new("replace", &fake_url).signed_in_with("c:alice", "alice-access", "sar_alice", 4_102_444_800);
    let o = cli.run(&["login", "--slt", "slt_good_one", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let v = json_out(&o);
    assert_eq!(v["id"], "si:chef");
    assert_eq!(v["replaced"]["id"], "c:alice");
    let revoke = fake.requests("POST", "/v1/oauth/revoke");
    assert_eq!(revoke.len(), 1);
    assert_eq!(revoke[0].form("token").as_deref(), Some("sar_alice"));
}

#[test]
fn a_refresh_never_overwrites_a_newer_sign_in() {
    let home = Arc::new(Mutex::new(std::path::PathBuf::new()));
    let h2 = home.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/v1/oauth/token") if r.form("grant_type").as_deref() == Some("refresh_token") => {
            // While the refresh is out, someone writes a newer sign-in without waiting for the lock.
            let file = h2.lock().unwrap().join("auth.json");
            let mut newer: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            newer["access_token"] = json!("newer-access");
            newer["refresh_token"] = json!("sar_newer");
            std::fs::write(file, newer.to_string()).unwrap();
            Some(raw_json(200, tokens("c:alice", "old-family-access", "sar_old_family")))
        }
        ("POST", "/v1/oauth/revoke") => Some(raw_json(200, json!({"revoked": true}))),
        ("GET", "/api/v2/devices") => Some(ok("devices", json!({"items": [], "next_cursor": null}))),
        _ => None,
    });
    let cli = Cli::new("refresh-newer", &fake.url).signed_in_with("c:alice", "old-access", "sar_old", now() - 5);
    *home.lock().unwrap() = cli.state();
    let o = cli.run(&["device", "ls"]);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("changed while it was being refreshed"),
        "{}",
        stderr(&o)
    );
    assert_eq!(cli.auth()["refresh_token"], "sar_newer", "the newer sign-in is kept");
    assert_eq!(
        fake.requests("POST", "/v1/oauth/revoke")[0].form("token").as_deref(),
        Some("sar_old_family")
    );
    assert!(fake.requests("GET", "/api/v2/devices").is_empty());
}
