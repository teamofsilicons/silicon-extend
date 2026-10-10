//! Runs the real `extend` binary against a scriptable fake Extend service: the JSON convention,
//! paging, help while connected, grammar and settings, retired Extend 3 spellings, moving the state
//! directory, file downloads through Extend, the compatibility check, and the device, waking,
//! request, access, Ting and custodian commands on API v2. Signing in is in `cli_accounts.rs`.
//!
//! `EXTEND_TEST_BIN` runs the checks against another build of the CLI instead of this one.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{Value, json};

// ───────────────────────────── The JSON convention ─────────────────────────────

#[test]
fn json_prints_the_data_itself_and_errors_on_stderr() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/me") => Some(ok("me", me("si:chef"))),
        ("GET", "/api/v2/devices/7c1e09ab") => {
            Some(err(404, "device_not_found", "No device 7c1e09ab is visible to you."))
        }
        _ => None,
    });
    let cli = Cli::new("json", &fake.url);
    let o = cli.run(&["accounts", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(json_out(&o)["app_id"], "extend", "app_id at the top level");
    assert!(json_out(&o).get("ok").is_none() && json_out(&o).get("data").is_none());

    // Not signed in: --json exits 0, plain text exits 1 (like silicon-accounts login status).
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(json_out(&o), json!({"authenticated": false}));
    let o = cli.run(&["login", "status"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).starts_with("Not signed in"), "{}", stdout(&o));

    let cli = cli.signed_in("si:chef");
    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(json_out(&o)["authenticated"], true);
    assert_eq!(json_out(&o)["id"], "si:chef");
    assert_eq!(json_out(&o)["uuid"], "cHeF1");
    assert_eq!(json_out(&o)["custodian"]["id"], "c:alice");

    // A failure: nothing on stdout, one {"error": {...}} document on stderr.
    let o = cli.run(&["--json", "device", "show", "7c1e09ab"]);
    assert_eq!(o.status.code(), Some(5));
    assert!(o.stdout.is_empty(), "stdout: {}", stdout(&o));
    let e = json_err(&o);
    assert_eq!(e["error"]["code"], "device_not_found");
    assert_eq!(e["error"]["exit_code"], 5);
    assert_eq!(e["error"]["request_id"], "req-0192");
    assert_eq!(e["error"]["hint"], "fake hint for device_not_found");
    assert!(e["error"]["message"].as_str().unwrap().contains("No device 7c1e09ab"));
    // Usage errors too.
    let o = cli.run(&["--json", "device", "ls", "--onlinee"]);
    let e = json_err(&o);
    assert_eq!(
        (e["error"]["code"].as_str(), e["error"]["exit_code"].as_i64()),
        (Some("invalid_input"), Some(2))
    );
    // Every call went to API v2 with the access token, and no Team header.
    let shown = fake.requests("GET", "/api/v2/devices/7c1e09ab");
    assert_eq!(shown[0].headers["authorization"], "Bearer test-access");
    assert_eq!(shown[0].headers["silicon-extend-api-version"], "2");
    assert!(!shown[0].headers.contains_key("x-org-id"));
}

#[test]
fn device_ls_reads_every_page() {
    let fake = Fake::start(|r| {
        if r.path_only() != "/api/v2/devices" {
            return None;
        }
        let online_only = r.query("online").as_deref() == Some("true");
        let mut page = match r.query("cursor").as_deref() {
            None => {
                json!({"items": [device("00000001", "one", true), device("00000002", "two", false)], "next_cursor": "00000002"})
            }
            Some("00000002") => json!({"items": [device("00000003", "three", true)], "next_cursor": null}),
            _ => return None,
        };
        if online_only {
            // An older service filtered after paging, leaving offline devices out of a full page.
            page["items"] = json!(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|d| d["online"] == true)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        Some(ok("devices", page))
    });
    let cli = Cli::new("device-ls", &fake.url).signed_in("si:chef");
    let o = cli.run(&["device", "ls"]);
    assert!(o.status.success(), "{}", stderr(&o));
    for name in ["one", "two", "three"] {
        assert!(stdout(&o).contains(name), "{}", stdout(&o));
    }
    let o = cli.run(&["device", "ls", "--online", "--json"]);
    let names: Vec<String> = json_out(&o)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["one", "three"]);
    assert_eq!(json_out(&o)["next_cursor"], Value::Null);
    let asked = fake.requests("GET", "/api/v2/devices");
    assert!(asked.iter().all(|r| r.query("limit").as_deref() == Some("100")));
    assert!(asked.iter().any(|r| r.query("online").as_deref() == Some("true")));
    assert!(
        asked.iter().all(|r| r.query("scope").is_none()),
        "the service picks mine/accessible"
    );
}

#[test]
fn device_ls_says_when_it_stops_early() {
    let fake = Fake::start(|r| {
        (r.path_only() == "/api/v2/devices").then(|| {
            ok(
                "devices",
                json!({"items": [device("00000001", "loop", true)], "next_cursor": "00000001"}),
            )
        })
    });
    let cli = Cli::new("device-ls-loop", &fake.url).signed_in("si:chef");
    let o = cli.run(&["device", "ls"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("This list is incomplete"), "{}", stdout(&o));
    let o = cli.run(&["device", "ls", "--json"]);
    assert_eq!(json_out(&o)["next_cursor"], "00000001");
}

// ───────────────────────────── Help while connected ─────────────────────────────

#[test]
fn removed_model_commands_leave_ordinary_ref_and_android_commands_usable() {
    let commands = ["snapshot", "click", "fill", "press", "type", "adb"];
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/sessions/a3f") => Some(ok("session", session("a3f", "active", &commands))),
        ("POST", "/api/v2/sessions/a3f/commands") => {
            let mut result = command_result(json!([]), json!([]));
            result["command"] = r.body["data"]["command"].clone();
            result["output"] = json!({"nodes":[{"ref":"e2","role":"button","label":"Search"}]});
            Some(ok("command_result", result))
        }
        _ => None,
    });
    let cli = Cli::new("ordinary-commands", &fake.url)
        .signed_in("si:chef")
        .connected("a3f", &commands);
    for removed in ["act", "bench-ref"] {
        let out = cli.run(&[removed, "Click Search", "--json"]);
        assert!(!out.status.success());
        assert_eq!(json_err(&out)["error"]["code"], "unknown_command");
        assert!(fake.requests("POST", "/api/v2/sessions/a3f/commands").is_empty());
    }
    let help = stdout(&cli.run(&["--help"]));
    assert!(!help.contains("extend act") && !help.contains("bench-ref"), "{help}");

    let ordinary = [
        vec!["snapshot", "-i", "--force-full"],
        vec!["click", "@e2"],
        vec!["fill", "@e4", "NH1 Bowls"],
        vec!["press", "450", "615"],
        vec!["type", "NH1 Bowls"],
        vec!["adb", "shell", "input", "tap", "450", "615"],
    ];
    for argv in &ordinary {
        let out = cli.run(argv);
        assert!(out.status.success(), "{}: {}", argv.join(" "), stderr(&out));
    }
    let sent = fake.requests("POST", "/api/v2/sessions/a3f/commands");
    assert_eq!(sent.len(), ordinary.len());
    for (request, expected) in sent.iter().zip(ordinary) {
        assert_eq!(request.body["type"], "command");
        assert_eq!(request.body["data"]["command"], expected[0]);
        assert_eq!(request.body["data"]["args"], json!(&expected[1..]));
    }
}

#[test]
fn help_follows_the_connected_device() {
    let commands = Arc::new(Mutex::new(vec!["snapshot"]));
    let state = Arc::new(Mutex::new("active"));
    let (c2, s2) = (commands.clone(), state.clone());
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/sessions/a3f") => {
            Some(ok("session", session("a3f", *s2.lock().unwrap(), &c2.lock().unwrap())))
        }
        ("POST", "/api/v2/sessions/a3f/commands") => {
            let mut result = command_result(json!([]), json!([]));
            if *s2.lock().unwrap() == "ended" {
                result["ok"] = json!(false);
                result["error"] =
                    json!({"code": "session_ended", "message": "Session a3f ended: the device's Carbon stopped it."});
            }
            Some(ok("command_result", result))
        }
        _ => None,
    });
    let cli = Cli::new("help-connected", &fake.url).signed_in("si:chef");
    let o = cli.run(&["session", "connect", "a3f"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        cli.session_dir().join("current").exists(),
        "kept under the account's uuid"
    );
    let help = stdout(&cli.run(&["--help"]));
    assert!(help.contains("connected to session a3f on CLI box (linux)"), "{help}");
    assert!(!help.contains("tv-remote"), "{help}");
    let tv = stdout(&cli.run(&["tv-remote", "--help"]));
    assert!(
        tv.contains("Not available on CLI box (linux) in session a3f: it needs input.remote")
            && tv.contains("Only TVs have a remote."),
        "{tv}"
    );

    // `session status` refreshes the list.
    commands.lock().unwrap().push("terminal");
    assert!(cli.run(&["session", "status"]).status.success());
    assert!(stdout(&cli.run(&["--help"])).contains("  terminal "));
    // So does each device command.
    commands.lock().unwrap().push("clipboard");
    assert!(cli.run(&["snapshot"]).status.success());
    assert!(stdout(&cli.run(&["--help"])).contains("  clipboard "));

    // A result saying the session ended disconnects.
    *state.lock().unwrap() = "ended";
    let o = cli.run(&["snapshot"]);
    assert_eq!(o.status.code(), Some(1));
    let help = stdout(&cli.run(&["--help"]));
    assert!(!help.contains("connected to session"), "{help}");
    assert!(!cli.session_dir().join("current").exists());
}

#[test]
fn a_session_ended_error_disconnects() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/sessions/b4c/commands") => Some(err(
            409,
            "session_ended",
            "Session b4c ended while `snapshot` was running.",
        )),
        _ => None,
    });
    let cli = Cli::new("ended-error", &fake.url)
        .signed_in("si:chef")
        .connected("b4c", &["snapshot"]);
    let o = cli.run(&["snapshot"]);
    assert_eq!(o.status.code(), Some(6), "{}", stderr(&o));
    assert!(!stdout(&cli.run(&["--help"])).contains("connected to session"));
}

// ───────────────────────────── Grammar, settings, colour, -v ─────────────────────────────

#[test]
fn unknown_flags_and_settings_are_refused_with_the_choices() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/devices/7c1e09ab/requests") => Some(ok(
            "request",
            json!({"request_id": FILE_ID, "device_id": "7c1e09ab", "from": "si:sous", "to": "si:chef", "reason": r.body["data"]["reason"],
                   "created_at": "2026-09-27T00:00:00Z", "delivery": "delivered"}),
        )),
        _ => None,
    });
    let cli = Cli::new("grammar", &fake.url).signed_in("si:sous");
    let o = cli.run(&["device", "ls", "--onlinee"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("`extend device ls` has no flag --onlinee"),
        "{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains("Did you mean --online?"), "{}", stderr(&o));
    let o = cli.run(&["logout", "--bogus"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("takes no flags of its own"), "{}", stderr(&o));
    let o = cli.run(&["--bogus", "device", "ls"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("--bogus is not a global flag"), "{}", stderr(&o));

    // A flag's value is the value, even when it looks like a global flag.
    let o = cli.run(&["request", "send", "7c1e09ab", "--reason", "-h"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        fake.requests("POST", "/api/v2/devices/7c1e09ab/requests")[0].body["data"]["reason"],
        "-h"
    );

    for (args, says) in [
        (
            &["config", "set", "color", "purple"][..],
            "color can't be \"purple\": it is one of auto, always, never",
        ),
        (
            &["config", "set", "screenshot_scale", "7"],
            "it is a number from 0.01 to 1",
        ),
        (&["config", "set", "api_url", "ftp://x"], "uses the ftp scheme"),
        (
            &["config", "set", "accounts_url", "http://accounts.example.com"],
            "uses plain http for a host that is not this machine",
        ),
        (
            &["config", "set", "self_destruct", "45d"],
            "outside 1 minute to 30 days",
        ),
        (&["config", "get", "bogus"], "unknown setting \"bogus\""),
        (&["config", "unset", "bogus"], "unknown setting \"bogus\""),
        (
            &["config", "set", "team", "acme"],
            "the team setting was removed in Extend 4",
        ),
    ] {
        let o = cli.run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(stderr(&o).contains(says), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("  hint: "),
            "{args:?}: every usage error says what to do"
        );
    }
    let o = cli.run(&["config", "get", "telemetry"]);
    assert_eq!(stdout(&o).trim(), "on (default)");
    let o = cli.run(&["config", "set", "accounts_url", "http://localhost:9590/"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        stdout(&cli.run(&["config", "get", "accounts_url"])).trim(),
        "http://localhost:9590"
    );
}

#[test]
fn extend_3_spellings_say_what_replaced_them() {
    let fake = Fake::start(|_| None);
    let cli = Cli::new("retired", &fake.url).signed_in("c:alice");
    for (args, says, hint) in [
        (
            &["--team", "acme", "device", "ls"][..],
            "--team was removed in Extend 4",
            "Drop --team",
        ),
        (
            &["device", "ls", "--team=acme"],
            "--team was removed in Extend 4",
            "Drop --team",
        ),
        (
            &["--test", "9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c", "device", "ls"],
            "--test was removed in Extend 4",
            "extend>dev",
        ),
        (
            &["team", "silicons"],
            "`extend team` was removed in Extend 4",
            "extend silicon ls",
        ),
        (
            &["permission", "ls"],
            "`extend permission` was removed in Extend 4",
            "file ls",
        ),
        (
            &["login", "contexts"],
            "`extend login contexts` was removed in Extend 4",
            "SILICON_HOME",
        ),
        (
            &["login", "use", "c:alice", "acme"],
            "`extend login use` was removed",
            "SILICON_HOME",
        ),
        (
            &["config", "test", "ls"],
            "`extend config test` was removed",
            "extend>dev",
        ),
        (&["env", "show"], "`extend env` was removed", "extend>dev"),
        (
            &["device", "import", "7c1e09ab"],
            "`extend device import` was removed",
            "extend device ls",
        ),
        (
            &["device", "visibility", "7c1e09ab", "team"],
            "`extend device visibility` was removed",
            "device access grant",
        ),
        (
            &["device", "ls", "--team-visible"],
            "no longer takes --team-visible",
            "Drop --team-visible",
        ),
        (
            &["device", "pair", "4f9c2a", "--name", "x", "--visibility", "team"],
            "no longer takes --visibility",
            "--access",
        ),
        (
            &["ting", "status", "--all-teams"],
            "no longer takes --all-teams",
            "Drop --all-teams",
        ),
        (
            &[
                "device",
                "wake-requests",
                "mute",
                "0d44e1f2",
                "--silicon",
                "si:chef",
                "--only-team",
                "acme",
            ],
            "no longer takes --only-team",
            "--silicon",
        ),
    ] {
        let o = cli.run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(stderr(&o).contains(says), "{args:?}: {}", stderr(&o));
        assert!(stderr(&o).contains(hint), "{args:?}: {}", stderr(&o));
    }
    // Nothing reached the service.
    assert!(fake.seen.lock().unwrap().is_empty(), "{:?}", fake.seen.lock().unwrap());
    // A script still written for a test environment never reaches the real service.
    let cli = cli.env("EXTEND_TEST_SECRET", "ask_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG");
    let o = cli.run(&["device", "ls"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("Extend 4 has no test environments"),
        "{}",
        stderr(&o)
    );
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[test]
fn color_follows_the_setting_and_no_color() {
    let cli = Cli::new("color", "http://127.0.0.1:9");
    let o = cli.run(&["frobnicate"]);
    assert!(!stderr(&o).contains('\x1b'), "not a terminal: no colour by default");
    assert!(cli.run(&["config", "set", "color", "always"]).status.success());
    let o = cli.run(&["frobnicate"]);
    assert!(stderr(&o).contains("\x1b[1;31merror:\x1b[0m"), "{:?}", stderr(&o));
    // `always` is the user's own choice, so it wins over NO_COLOR.
    let cli = cli.env("NO_COLOR", "1");
    assert!(stderr(&cli.run(&["frobnicate"])).contains('\x1b'));
    assert!(cli.run(&["config", "set", "color", "never"]).status.success());
    assert!(!stderr(&cli.run(&["frobnicate"])).contains('\x1b'));
    // --json is never coloured.
    assert!(cli.run(&["config", "set", "color", "always"]).status.success());
    assert!(!stderr(&cli.run(&["--json", "frobnicate"])).contains('\x1b'));
}

#[test]
fn verbose_shows_each_call_its_time_and_request_id() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/devices/7c1e09ab") => Some(ok("device", device("7c1e09ab", "box", true))),
        ("GET", "/api/v2/devices/0000dead") => Some(err(404, "device_not_found", "No device 0000dead.")),
        _ => None,
    });
    let cli = Cli::new("verbose", &fake.url).signed_in("c:alice");
    let o = cli.run(&["-v", "device", "show", "7c1e09ab"]);
    let err = stderr(&o);
    assert!(
        err.contains("[extend] GET ") && err.contains("/api/version: API v2 in "),
        "{err}"
    );
    assert!(err.contains("[extend] GET /api/v2/devices/7c1e09ab: ok in "), "{err}");
    assert!(err.contains("[extend] finished in "), "{err}");
    let o = cli.run(&["--verbose", "device", "show", "0000dead"]);
    assert!(
        stderr(&o).contains("[extend] GET /api/v2/devices/0000dead: 404 device_not_found in ")
            && stderr(&o).contains(", request req-0192"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&cli.run(&["device", "show", "7c1e09ab"])).contains("[extend]"));
}

/// `extend -v … 2>&1 | grep -q …` and `extend … | head -1`: once the reader has what it wanted it
/// goes away, and the CLI's later writes hit a broken pipe. That must not turn into a panic.
#[test]
fn a_reader_that_goes_away_does_not_crash_the_command() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/devices/7c1e09ab") => Some(ok("device", device("7c1e09ab", "Lab box", true))),
        _ => None,
    });
    let cli = Cli::new("reader-gone", &fake.url).signed_in("c:alice");
    let (status, out) = cli.run_with_reader_gone(&["-v", "device", "show", "7c1e09ab"], false);
    assert_eq!(status.code(), Some(0), "stderr gone; stdout: {out}");
    assert!(out.contains("Lab box"), "{out}");
    let (status, err) = cli.run_with_reader_gone(&["-v", "device", "show", "7c1e09ab"], true);
    assert_eq!(status.code(), Some(0), "stdout gone; stderr: {err}");
    assert!(!err.contains("panicked"), "{err}");
    assert!(
        err.contains("[extend] finished in ") && err.contains("with exit code 0"),
        "{err}"
    );
    let (status, err) = cli.run_with_reader_gone(&["--help"], true);
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

// ───────────────────────────── config home ─────────────────────────────

#[test]
fn config_home_moves_the_sign_in_and_settings() {
    let fake = Fake::start(|r| (r.path_only() == "/api/v2/me").then(|| ok("me", me("si:chef"))));
    let cli = Cli::new("home-move", &fake.url).signed_in("si:chef");
    assert!(cli.run(&["config", "set", "telemetry", "off"]).status.success());
    let target = cli.home.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    let o = cli.run(&["config", "home", target.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("Moved the sign-in of si:chef, 1 setting(s)"),
        "{}",
        stdout(&o)
    );

    let o = cli.run(&["login", "status", "--json"]);
    assert_eq!(
        json_out(&o)["authenticated"],
        true,
        "the sign-in came along: {}",
        stderr(&o)
    );
    assert_eq!(stdout(&cli.run(&["config", "get", "telemetry"])).trim(), "off");
    let new_root = target.canonicalize().unwrap().join(".extend");
    assert!(new_root.join("auth.json").exists() && new_root.join("config.toml").exists());
    assert!(!cli.state().join("auth.json").exists(), "the old copy is gone");
    assert!(stdout(&cli.run(&["docs"])).contains(&new_root.display().to_string()));

    let o = cli.run(&["config", "home", "/definitely/not/here"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("not a directory: /definitely/not/here") && stderr(&o).contains("hint: "));

    // Back home: refused while the default still holds state, unless asked to switch.
    std::fs::write(cli.state().join("config.toml"), "output = \"json\"\n").unwrap();
    let o = cli.run(&["config", "home", cli.home.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("already holds Extend state (config.toml)") && stderr(&o).contains("--use-existing"),
        "{}",
        stderr(&o)
    );
    let o = cli.run(&["config", "home", cli.home.to_str().unwrap(), "--use-existing"]);
    assert!(o.status.success(), "{}", stderr(&o));
    // Now the default's settings apply, and the moved sign-in stays where it went.
    let o = cli.run(&["login", "status"]);
    assert_eq!(
        json_out(&o),
        json!({"authenticated": false}),
        "output=json from the default state"
    );
}

// ───────────────────────────── Files ─────────────────────────────

fn file_fake(content: Resp) -> Fake {
    let content = Arc::new(Mutex::new(Some(content)));
    Fake::start(move |r| {
        let url = "https://briefcase.example/f/shot";
        match (r.method.as_str(), r.path_only()) {
            ("GET", p) if p == format!("/api/v2/files/{FILE_ID}") => Some(ok("file", file_info(url))),
            ("GET", p) if p == format!("/api/v2/files/{FILE_ID}/content") => {
                let c = content.lock().unwrap();
                let c = c.as_ref().unwrap();
                Some(Resp {
                    status: c.status,
                    headers: c.headers.clone(),
                    body: c.body.clone(),
                })
            }
            ("GET", "/api/v2/files") => Some(ok("files", json!({"items": [file_info(url)], "next_cursor": null}))),
            ("POST", "/api/v2/sessions/a3f/commands") => Some(ok(
                "command_result",
                command_result(
                    json!([file_info(url)]),
                    json!([
                        "shot.png is stored, but not shared with c:alice, so they can't open it in Briefcase yet: fake."
                    ]),
                ),
            )),
            _ => None,
        }
    })
}

fn png() -> Resp {
    Resp {
        status: 200,
        headers: vec![
            ("content-type".into(), "image/png".into()),
            ("content-disposition".into(), "attachment; filename=\"shot.png\"".into()),
        ],
        body: b"PNG-BYTES!!".to_vec(),
    }
}

#[test]
fn files_download_through_extend_after_their_links() {
    let fake = file_fake(png());
    let cli = Cli::new("files", &fake.url).signed_in("si:chef");
    let o = cli.run(&["file", "get", FILE_ID]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    let link = out
        .find("https://briefcase.example/f/shot")
        .expect("the link is printed");
    assert!(link < out.find("Saved to").unwrap(), "{out}");
    assert_eq!(std::fs::read(cli.home.join("shot.png")).unwrap(), b"PNG-BYTES!!");
    let asked = fake.requests("GET", &format!("/api/v2/files/{FILE_ID}/content"));
    assert_eq!(
        asked[0].headers["authorization"], "Bearer test-access",
        "Extend's token goes to Extend"
    );

    let o = cli.run(&["file", "get", FILE_ID, "--out", "copy.png", "--json"]);
    assert_eq!(json_out(&o)["bytes"], 11);
    assert_eq!(std::fs::read(cli.home.join("copy.png")).unwrap(), b"PNG-BYTES!!");

    // A custodian narrows the list to one Silicon it looks after.
    let o = cli.run(&["file", "ls", "--silicon", "si:chef"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("BY") && stdout(&o).contains("si:chef"),
        "{}",
        stdout(&o)
    );
    assert_eq!(
        fake.requests("GET", "/api/v2/files")[0].query("silicon").as_deref(),
        Some("si:chef")
    );

    // A device command: links and warnings first, then the download.
    let cli = cli.connected("a3f", &["screenshot"]);
    let o = cli.run(&["screenshot", "--out", "shots/"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("took a screenshot\nscreenshot shot.png"),
        "{}",
        stdout(&o)
    );
    assert!(
        stdout(&o).contains("Saved shot.png to shots/shot.png"),
        "{}",
        stdout(&o)
    );
    assert!(
        stderr(&o).contains("warning: shot.png is stored, but not shared with c:alice"),
        "{}",
        stderr(&o)
    );
    assert_eq!(std::fs::read(cli.home.join("shots/shot.png")).unwrap(), b"PNG-BYTES!!");
    let o = cli.run(&["screenshot", "--json", "--out", "j.png"]);
    let doc = json_out(&o);
    assert_eq!(
        doc["warnings"][0].as_str().unwrap().split(',').next(),
        Some("shot.png is stored")
    );
    assert_eq!(doc["saved_to"][0], "j.png");
    assert_eq!(doc["command"], "screenshot");
}

#[test]
fn a_failed_download_still_leaves_the_link() {
    let fake = file_fake(err(403, "no_access", "Briefcase refused to read shot.png for si:chef."));
    let cli = Cli::new("files-refused", &fake.url)
        .signed_in("si:chef")
        .connected("a3f", &["screenshot"]);
    let o = cli.run(&["screenshot", "--out", "x.png"]);
    assert_eq!(o.status.code(), Some(4), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("https://briefcase.example/f/shot"),
        "the link was printed first: {}",
        stdout(&o)
    );
    assert!(
        stderr(&o).contains(&format!("Could not download shot.png ({FILE_ID}): Briefcase refused")),
        "{}",
        stderr(&o)
    );
    assert!(
        stderr(&o).contains("The file is still in Briefcase: https://briefcase.example/f/shot"),
        "{}",
        stderr(&o)
    );
    assert!(!cli.home.join("x.png").exists());

    let o = cli.run(&["--json", "screenshot", "--out", "x.png"]);
    assert_eq!(
        json_err(&o)["error"]["details"]["result"]["files"][0]["url"],
        "https://briefcase.example/f/shot"
    );
}

// ───────────────────────────── extend version ─────────────────────────────

fn matrix(state: &str) -> Value {
    json!({"service_version": "4.0.0", "current": 2, "supported": [1, 2], "sunset_rule": "Sunset after 7 consecutive days with zero requests",
           "versions": [
             {"api_version": 1, "state": "current", "deprecated_at": null, "sunset_at": null, "sunset_earliest_at": null,
              "sunset_rule": "Sunset after 7 consecutive days with zero requests",
              "compatible": {"client_crate": ">=1.0.0, <4.0.0", "cli": ">=1.0.0, <4.0.0", "device_app_min": "1.0.0"}},
             {"api_version": 2, "state": state, "deprecated_at": if state == "current" { Value::Null } else { json!("2026-09-01T00:00:00Z") },
              "sunset_at": null, "sunset_earliest_at": if state == "deprecated" { json!("2026-10-04T00:00:00Z") } else { Value::Null },
              "sunset_rule": "Sunset after 7 consecutive days with zero requests",
              "compatible": {"client_crate": ">=4.0.0, <5.0.0", "cli": ">=4.0.0, <5.0.0", "device_app_min": "1.0.0"}}]})
}

#[test]
fn version_reads_the_compatibility_matrix() {
    for (state, says) in [
        (
            "current",
            "Status: current. API v2 is current, and it works with CLI >=4.0.0, <5.0.0",
        ),
        (
            "deprecated",
            "Status: deprecated. API v2 is deprecated since 2026-09-01; Extend retires it after 7 consecutive days with zero requests, no earlier than 2026-10-04. Update with `silicon-apps update extend` before then.",
        ),
    ] {
        let fake = Fake::start(move |r| (r.path_only() == "/api/v2/contracts").then(|| ok("contracts", matrix(state))));
        let cli = Cli::new(&format!("version-{state}"), &fake.url);
        let o = cli.run(&["version"]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert!(stdout(&o).contains(says), "{}", stdout(&o));
        assert!(
            stdout(&o).contains("API v2 at") && stdout(&o).contains("(service 4.0.0)"),
            "{}",
            stdout(&o)
        );
        let o = cli.run(&["-V", "--json"]);
        assert_eq!(json_out(&o)["status"], state);
        assert_eq!(json_out(&o)["cli_range"], ">=4.0.0, <5.0.0");
    }

    // An Extend 3 service: no API version in common.
    let fake = Fake::start_with_version(
        json!({"code": "api_version_unsupported", "message": "No API version in common: the client supports [2], Extend supports [1].", "hint": "Update the CLI."}),
        |_| None,
    );
    let o = Cli::new("version-old-service", &fake.url).run(&["version", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(json_out(&o)["status"], "unsupported");
    let asked = fake.requests("GET", "/api/version");
    assert_eq!(asked[0].headers["silicon-extend-supported-api-versions"], "2");

    let o = Cli::new("version-down", "http://127.0.0.1:9").run(&["version"]);
    assert!(o.status.success());
    assert!(
        stdout(&o).contains("API unreachable") && stdout(&o).contains("Status: unknown."),
        "{}",
        stdout(&o)
    );
}

// ───────────────────────────── Devices, waking ─────────────────────────────

#[test]
fn device_ls_shows_awake_and_in_use_as_each_viewer_may_see_them() {
    let fake = Fake::start(|r| {
        (r.path_only() == "/api/v2/devices").then(|| {
            let items = if r.headers.get("authorization").is_some_and(|a| a.contains("silicon")) {
                json!([
                    device_with("0d44e1f2", "Family TV", json!({"os": "android_tv", "kind": "tv", "awake": false, "sleep_state": "standby",
                        "open_wake_requests": 1, "same_device": ["3a2b0c1d"], "in_use_by_other": true})),
                    device_with("7c1e09ab", "CLI box", json!({"awake": true, "in_use": {"silicon_id": "si:sous", "session_id": "a3f", "since": "2026-09-27T10:00:00Z"}})),
                ])
            } else {
                json!([
                    device_with("0d44e1f2", "Family TV", json!({"os": "android_tv", "kind": "tv", "awake": false, "sleep_state": "standby",
                        "paired_by_others": true, "in_use_by_other": true, "access_count": 2, "days_left": 13})),
                    device_with("2e7f00d1", "Studio Mac", json!({"os": "macos", "in_use_by_other": true, "in_use_by_other_carried": true})),
                    device_with("7c1e09ab", "CLI box", json!({"online": false, "last_sleep_state": "asleep",
                        "in_use": {"silicon_id": "si:chef", "silicon_uuid": "cHeF1", "session_id": "b4c", "since": "2026-09-27T10:00:00Z"}})),
                ])
            };
            ok("devices", json!({"items": items, "next_cursor": null}))
        })
    });
    let carbon = Cli::new("ls-carbon", &fake.url).signed_in("c:alice");
    let o = carbon.run(&["device", "ls"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    for want in [
        "ID        NAME",
        "AWAKE",
        "LAST USED",
        "Family TV (shared)",
        "no (standby)",
        "yes (another Carbon's Silicon)",
        "a carried device (stop it at the computer)",
        "si:chef (",
        "— (offline; last seen asleep)",
    ] {
        assert!(out.contains(want), "no {want:?} in:\n{out}");
    }
    assert!(!out.contains("labs") && !out.contains("acme"), "no Teams: {out}");

    let silicon = Cli::new("ls-silicon", &fake.url).signed_in_with("si:chef", "silicon-access", "sar_x", 4_102_444_800);
    let out = stdout(&silicon.run(&["device", "ls"]));
    for want in ["no (standby)  in use, asked (same device as 3a2b0c1d)", "si:sous"] {
        assert!(out.contains(want), "no {want:?} in:\n{out}");
    }
    assert!(!out.contains("LAST USED") && !out.contains("another Carbon"), "{out}");
}

#[test]
fn pairing_keeps_the_device_private_and_grants_by_id() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/pairings") => Some(answer(
            201,
            "device",
            device_with(
                "2e7f00d1",
                "Family Mac",
                json!({"os": "macos", "paired_by_others": true}),
            ),
        )),
        _ => None,
    });
    let cli = Cli::new("pair", &fake.url).signed_in("c:alice");
    let o = cli.run(&[
        "device",
        "pair",
        "4f9c2a",
        "--name",
        "Family Mac",
        "--access",
        "si:chef",
        "--access",
        "sCoUt",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let sent = &fake.requests("POST", "/api/v2/pairings")[0];
    assert_eq!(
        sent.body,
        json!({"type": "pairing", "data": {"pairing_code": "4f9c2a", "name": "Family Mac", "silicon_ids": ["si:chef", "sCoUt"]}}),
        "no visibility: every device is private"
    );
    assert!(sent.headers.contains_key("idempotency-key"));
    let out = stdout(&o);
    assert!(
        out.contains(
            "This device is also paired by another Carbon; your pair is separate (own name, access and lifetime)."
        ) && out.contains(
            "Only Silicons given access by the Carbon who installed Silicon Extend on it can use its terminal"
        ),
        "{out}"
    );
}

#[test]
fn wake_asks_refreshes_and_cancels() {
    let asks = Arc::new(Mutex::new(0));
    let a2 = asks.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/devices/0d44e1f2/wake-requests") => {
            let mut n = a2.lock().unwrap();
            *n += 1;
            Some(match *n {
                1 => answer(
                    201,
                    "wake_request",
                    wake(
                        1,
                        json!({"ting": "deferred", "wake_detectable": false, "device_notice": "unsupported",
                               "host": {"device_id": "2e7f00d1", "name": "Studio Mac", "online": true}}),
                    ),
                ),
                2 => err(
                    429,
                    "rate_limited",
                    "You asked to wake 0d44e1f2 less than 5 minutes ago.",
                ),
                3 => answer(200, "wake_request", wake(2, json!({}))),
                _ => err(409, "conflict", "Family TV is already awake."),
            })
        }
        ("GET", "/api/v2/devices/0d44e1f2") => Some(ok(
            "device",
            device_with("0d44e1f2", "Family TV", json!({"os": "tvos"})),
        )),
        ("GET", "/api/v2/devices/0d44e1f2/wake-requests") => Some(ok(
            "wake_requests",
            json!({"items": [wake(1, json!({"from": "si:chef"}))], "next_cursor": null}),
        )),
        ("DELETE", "/api/v2/devices/0d44e1f2/wake-requests/0192f3a4-0000-7000-8000-000000000001") => Some(no_content()),
        _ => None,
    });
    let cli = Cli::new("wake", &fake.url).signed_in("si:chef");
    let o = cli.run(&["device", "wake", "0d44e1f2", "--reason", "Need the TV on"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    for want in [
        "Asked c:alice to wake Family TV (0d44e1f2); the request expires at",
        "Family TV can't show wake requests itself; its Carbon gets it through Ting.",
        "Studio Mac (2e7f00d1), which it pairs through, must be awake too.",
        "Extend can't tell when Family TV wakes; its Carbon will say so.",
        "Its Carbon will be told through Ting shortly.",
        "then run: extend session new 0d44e1f2",
    ] {
        assert!(out.contains(want), "no {want:?} in:\n{out}");
    }
    let sent = &fake.requests("POST", "/api/v2/devices/0d44e1f2/wake-requests")[0];
    assert_eq!(
        sent.body,
        json!({"type": "wake_request", "data": {"reason": "Need the TV on"}})
    );
    assert!(sent.headers.contains_key("idempotency-key"));
    assert!(
        fake.requests("POST", "/v1/oauth/token").is_empty(),
        "a working sign-in isn't refreshed just to ask"
    );

    let o = cli.run(&["device", "wake", "0d44e1f2", "--reason", "Need the TV on"]);
    assert_eq!(o.status.code(), Some(12), "{}", stderr(&o));
    let o = cli.run(&["--json", "device", "wake", "0d44e1f2", "--reason", "Need the TV on"]);
    assert_eq!(json_out(&o)["asks"], 2);
    let o = cli.run(&["device", "wake", "0d44e1f2", "--reason", "Need the TV on"]);
    assert_eq!(o.status.code(), Some(6));

    *asks.lock().unwrap() = 2;
    let o = cli.run(&["device", "wake", "0d44e1f2", "--reason", "Still need it"]);
    assert!(
        stdout(&o).contains("Asked again (ask 2); the request now expires at"),
        "{}",
        stdout(&o)
    );

    let o = cli.run(&["device", "wake", "0d44e1f2", "--cancel"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("Withdrew your request to wake 0d44e1f2."));
    assert_eq!(
        fake.requests("GET", "/api/v2/devices/0d44e1f2/wake-requests")[0]
            .query("state")
            .as_deref(),
        Some("open")
    );

    let o = cli.run(&["device", "wake", "0d44e1f2"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("extend device wake 0d44e1f2 --reason"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn wake_requests_are_listed_answered_and_muted() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/devices/0d44e1f2/wake-requests") => Some(ok(
            "wake_requests",
            json!({"items": [wake(1, json!({"ting_last_error": "Ting doesn't know extend.device.wake_requested."}))], "next_cursor": null}),
        )),
        ("POST", "/api/v2/devices/0d44e1f2/wake-requests/answer") => Some(ok(
            "wake_answer",
            json!({"answer": r.body["data"]["answer"], "ended": [wake(1, json!({"state": "declined"}))]}),
        )),
        ("PUT", "/api/v2/devices/0d44e1f2/wake-settings") => Some(ok(
            "wake_settings",
            json!({"device_id": "0d44e1f2", "muted": false, "silicons_muted": [{"silicon_id": "si:chef"}]}),
        )),
        _ => None,
    });
    let cli = Cli::new("wake-requests", &fake.url).signed_in("c:alice");
    let out = stdout(&cli.run(&["device", "wake-requests", "ls", "0d44e1f2", "--open"]));
    assert!(
        out.contains("si:chef") && out.contains("Need the TV on") && out.contains("Ting doesn't know"),
        "{out}"
    );
    assert!(!out.contains("TEAM"), "{out}");
    let o = cli.run(&["device", "wake-requests", "answer", "0d44e1f2", "woken"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("0d44e1f2 is awake: every open request to wake it has ended"),
        "{}",
        stdout(&o)
    );
    let o = cli.run(&[
        "device",
        "wake-requests",
        "answer",
        "0d44e1f2",
        "declined",
        "--wake-id",
        "0192f3a4-0000-7000-8000-000000000001",
    ]);
    assert!(
        stdout(&o).contains("Declined 1 request(s) to wake 0d44e1f2: si:chef."),
        "{}",
        stdout(&o)
    );
    let answers = fake.requests("POST", "/api/v2/devices/0d44e1f2/wake-requests/answer");
    assert_eq!(answers[0].body["data"], json!({"answer": "woken"}));
    assert_eq!(
        answers[1].body["data"],
        json!({"answer": "declined", "wake_ids": ["0192f3a4-0000-7000-8000-000000000001"]})
    );
    let o = cli.run(&[
        "device",
        "wake-requests",
        "answer",
        "0d44e1f2",
        "woken",
        "--wake-id",
        "x",
    ]);
    assert_eq!(o.status.code(), Some(2));

    let o = cli.run(&["device", "wake-requests", "mute", "0d44e1f2", "--silicon", "si:chef"]);
    assert!(
        stdout(&o).contains("Wake requests from si:chef for 0d44e1f2 are off"),
        "{}",
        stdout(&o)
    );
    assert_eq!(
        fake.requests("PUT", "/api/v2/devices/0d44e1f2/wake-settings")[0].body["data"],
        json!({"muted": true, "silicon_id": "si:chef"})
    );
    let o = cli.run(&["device", "wake-requests", "ls", "0d44e1f2", "--silicon", "si:chef"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn requests_say_where_they_went() {
    let routed = Arc::new(Mutex::new(true));
    let r2 = routed.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/devices/7c1e09ab/requests") => Some(answer(
            201,
            "request",
            if *r2.lock().unwrap() {
                json!({"request_id": "0192f3a4-0000-7000-8000-000000000002", "device_id": "7c1e09ab", "from": "si:sous",
                       "to": "the Carbon who gave access to the Silicon using it", "reason": "OTP", "created_at": "2026-09-27T10:00:00Z",
                       "delivery": "pending", "routed_to": "carbon", "to_hidden": true})
            } else {
                json!({"request_id": "0192f3a4-0000-7000-8000-000000000002", "device_id": "7c1e09ab", "from": "si:sous",
                       "to": "si:chef", "session_id": "a3f", "reason": "OTP", "created_at": "2026-09-27T10:00:00Z",
                       "delivery": "delivered", "routed_to": "holder"})
            },
        )),
        ("GET", "/api/v2/devices/7c1e09ab/requests") | ("GET", "/api/v2/requests") => Some(ok(
            "requests",
            json!({"items": [{"request_id": "0192f3a4-0000-7000-8000-000000000002", "device_id": "7c1e09ab", "from": "si:scout",
                              "to": "c:alice", "session_id": "a3f", "reason": "OTP", "created_at": "2026-09-27T10:00:00Z",
                              "delivery": "delivered", "routed_to": "carbon"}], "next_cursor": null}),
        )),
        _ => None,
    });
    let sous = Cli::new("request-routed", &fake.url).signed_in("si:sous");
    let out = stdout(&sous.run(&["request", "send", "7c1e09ab", "--reason", "OTP"]));
    assert_eq!(
        out.trim(),
        "Sent to the Carbon who gave access to the Silicon using it; it's in use by a Silicon you can't see. Delivery: pending."
    );
    *routed.lock().unwrap() = false;
    let out = stdout(&sous.run(&["request", "send", "7c1e09ab", "--reason", "OTP"]));
    assert_eq!(
        out.trim(),
        "Sent to si:chef (using 7c1e09ab in session a3f). Delivery: delivered."
    );
    let alice = Cli::new("request-received", &fake.url).signed_in("c:alice");
    let out = stdout(&alice.run(&["device", "requests", "7c1e09ab"]));
    let row = out.lines().find(|l| l.contains("si:scout")).unwrap_or_default();
    assert!(
        row.contains(" you "),
        "the routed request names the asking Silicon and says it came to you:\n{out}"
    );
    // A custodian reads its Silicon's requests.
    assert!(alice.run(&["request", "ls", "--silicon", "si:chef"]).status.success());
    assert_eq!(
        fake.requests("GET", "/api/v2/requests")[0].query("silicon").as_deref(),
        Some("si:chef")
    );
}

#[test]
fn device_stop_reads_both_answers() {
    let other = Arc::new(Mutex::new(false));
    let o2 = other.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/devices/0d44e1f2/stop") => Some(if *o2.lock().unwrap() {
            ok(
                "device_stopped",
                json!({"device_id": "0d44e1f2", "stopped_at": "2026-09-27T10:00:00Z", "in_use_by_other": true}),
            )
        } else {
            ok("session", session("a3f", "ended", &[]))
        }),
        ("GET", "/api/v2/devices/0d44e1f2") => Some(ok("device", device_with("0d44e1f2", "Living room TV", json!({})))),
        ("POST", "/api/v2/devices/2e7f00d1/stop") => Some(err(
            409,
            "conflict",
            "A device carried by Studio Mac is in use. It can be stopped by the Carbon who paired it, or from Studio Mac's Extend app.",
        )),
        _ => None,
    });
    let cli = Cli::new("stop", &fake.url).signed_in("c:alice");
    assert_eq!(
        stdout(&cli.run(&["device", "stop", "0d44e1f2"])).trim(),
        "Stopped si:chef (session a3f) on CLI box."
    );
    *other.lock().unwrap() = true;
    assert_eq!(
        stdout(&cli.run(&["device", "stop", "0d44e1f2"])).trim(),
        "Stopped the Silicon using Living room TV (another Carbon gave it access)."
    );
    assert_eq!(
        json_out(&cli.run(&["device", "stop", "0d44e1f2", "--json"]))["in_use_by_other"],
        true
    );
    let o = cli.run(&["device", "stop", "2e7f00d1"]);
    assert_eq!(o.status.code(), Some(6));
    assert!(stderr(&o).contains("A device carried by Studio Mac is in use."));
}

#[test]
fn access_is_given_to_any_silicon_by_id() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("PUT", "/api/v2/devices/7c1e09ab/access/si:nobody") => Some(err(
            422,
            "invalid_input",
            "No account si:nobody exists in Silicon Accounts.",
        )),
        ("PUT", p) if p.starts_with("/api/v2/devices/7c1e09ab/access/") => Some(ok(
            "access_grant",
            json!({"device_id": "7c1e09ab", "silicon_id": "si:chef", "silicon_uuid": "cHeF1", "granted_by": "c:alice",
                   "granted_at": "2026-09-27T10:00:00Z", "last_used_at": null}),
        )),
        ("GET", "/api/v2/ting-registration") => Some(ok(
            "ting_registration",
            json!({"member": "c:alice", "status": "on", "missing_types": ["extend.device.wake_requested"], "delivery_enabled": true}),
        )),
        ("GET", "/api/v2/devices/7c1e09ab/access") => Some(ok(
            "access",
            json!({"items": [
                {"device_id": "7c1e09ab", "silicon_id": "si:chef", "silicon_uuid": "cHeF1", "granted_by": "c:alice", "granted_at": "2026-09-27T10:00:00Z", "last_used_at": null},
                {"device_id": "7c1e09ab", "silicon_id": "si:scout", "silicon_uuid": "sCoUt", "granted_by": "c:alice", "granted_at": "2026-09-27T10:00:00Z", "last_used_at": null, "wake_muted": true}
            ]}),
        )),
        ("DELETE", _) => Some(no_content()),
        _ => None,
    });
    let cli = Cli::new("access", &fake.url).signed_in("c:alice");
    let o = cli.run(&["device", "access", "grant", "7c1e09ab", "si:chef"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        stdout(&o).trim(),
        "Granted si:chef access to 7c1e09ab. It can use it now (one Silicon at a time) and its custodian can see it."
    );
    assert!(
        stderr(&o).contains("Ting doesn't know these Extend notification types yet")
            && stderr(&o).contains("extend.device.wake_requested: A Silicon asks its Carbon to wake a device"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&o).contains("--org"), "{}", stderr(&o));
    let o = cli.run(&["device", "access", "grant", "7c1e09ab", "si:nobody"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("No account si:nobody exists"), "{}", stderr(&o));

    let out = stdout(&cli.run(&["device", "access", "ls", "7c1e09ab"]));
    assert!(
        out.contains("SILICON")
            && out.contains("UUID")
            && out.contains("sCoUt")
            && out.contains("off")
            && !out.contains("TEAM"),
        "{out}"
    );

    let o = cli.run(&["device", "access", "revoke", "7c1e09ab", "si:chef", "si:scout"]);
    assert_eq!(
        stdout(&o).trim(),
        "Revoked access for si:chef and si:scout on 7c1e09ab; any running session of theirs there has ended."
    );
    let deletes = fake.requests("DELETE", "/api/v2/devices/7c1e09ab/access/si:chef");
    assert_eq!(deletes.len(), 1);
    assert!(deletes[0].query("team").is_none());
}

#[test]
fn ting_status_and_on() {
    let delivery = Arc::new(Mutex::new(true));
    let d2 = delivery.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/ting-registration") => Some(ok(
            "ting_registration",
            json!({"member": "c:alice", "member_uuid": "aLiCe", "status": "off", "missing_types": ["extend.device.woken"],
                   "delivery_enabled": *d2.lock().unwrap()}),
        )),
        ("PUT", "/api/v2/ting-registration") => Some(ok(
            "ting_registration",
            json!({"member": "c:alice", "status": "on", "missing_types": [], "delivery_enabled": true}),
        )),
        _ => None,
    });
    let cli = Cli::new("ting", &fake.url).signed_in("c:alice");
    let out = stdout(&cli.run(&["ting", "status"]));
    assert!(
        out.contains("You turned Extend's notifications off in Ting. Turn them on: extend ting on")
            && out.contains("extend.device.woken: A device a Silicon asked to wake is awake"),
        "{out}"
    );
    assert_eq!(json_out(&cli.run(&["ting", "status", "--json"]))["status"], "off");
    let o = cli.run(&["ting", "on"]);
    assert!(stdout(&o).contains("reach you through Ting again"), "{}", stdout(&o));
    assert!(
        fake.requests("PUT", "/api/v2/ting-registration")[0]
            .query("team")
            .is_none()
    );
    *delivery.lock().unwrap() = false;
    let out = stdout(&cli.run(&["ting", "status"]));
    assert!(
        out.contains("Notifications through Ting are off on this Extend server"),
        "{out}"
    );
}

#[test]
fn a_custodian_sees_and_stops_what_its_silicons_do() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/silicons") => Some(ok(
            "silicons",
            json!({"items": [
                {"uuid": "cHeF1", "id": "si:chef", "display_name": "Chef", "looked_after": true, "granted_by_you": 1, "grants": 2, "running_sessions": 1},
                {"uuid": "sCoUt", "id": "si:scout", "looked_after": false, "granted_by_you": 1}
            ]}),
        )),
        ("GET", "/api/v2/silicons/si:chef") => Some(ok(
            "silicon",
            json!({"uuid": "cHeF1", "id": "si:chef", "display_name": "Chef", "looked_after": true, "granted_by_you": 1, "grants": 2, "running_sessions": 1}),
        )),
        ("GET", "/api/v2/silicons/si:chef/grants") => Some(ok(
            "access",
            json!({"items": [{"device_id": "0d44e1f2", "silicon_id": "si:chef", "granted_by": "c:bob", "granted_at": "2026-09-27T10:00:00Z",
                              "last_used_at": null, "device_name": "Bob's TV", "device_os": "android_tv", "owner": {"type": "carbon", "id": "c:bob"}}]}),
        )),
        ("DELETE", "/api/v2/silicons/si:chef/grants/0d44e1f2") => Some(no_content()),
        ("GET", "/api/v2/sessions") => Some(ok(
            "sessions",
            json!({"items": [session("a3f", "active", &[])], "next_cursor": null}),
        )),
        ("POST", "/api/v2/sessions/a3f/end") => Some(ok("session", session("a3f", "ended", &[]))),
        _ => None,
    });
    let cli = Cli::new("custodian", &fake.url).signed_in("c:alice");
    let o = cli.run(&["silicon", "ls"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(
        out.contains("YOU LOOK AFTER IT") && out.contains("si:chef") && out.contains("cHeF1"),
        "{out}"
    );
    let out = stdout(&cli.run(&["silicon", "show", "si:chef"]));
    assert!(
        out.contains("You look after it: yes")
            && out.contains("Bob's TV")
            && out.contains("c:bob")
            && out.contains("extend silicon renounce si:chef <device_id>"),
        "{out}"
    );
    let o = cli.run(&["silicon", "renounce", "si:chef", "0d44e1f2"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        fake.requests("DELETE", "/api/v2/silicons/si:chef/grants/0d44e1f2")
            .len(),
        1
    );
    let o = cli.run(&["session", "ls", "--silicon", "si:chef", "--state", "active"]);
    assert!(
        stdout(&o).contains("Stop one: extend session end <session_id>"),
        "{}",
        stdout(&o)
    );
    let asked = &fake.requests("GET", "/api/v2/sessions")[0];
    assert_eq!(
        (asked.query("silicon").as_deref(), asked.query("state").as_deref()),
        (Some("si:chef"), Some("active"))
    );
    assert!(cli.run(&["session", "end", "a3f"]).status.success());
    let o = cli.run(&["silicon", "renounce", "si:chef", "bad id"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn setup_retry_follows_the_step() {
    let reads = Arc::new(Mutex::new(0));
    let r2 = reads.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/devices/7c1e09ab/setup") => {
            let mut n = r2.lock().unwrap();
            *n += 1;
            let status = match *n {
                1 | 2 => "failed",
                3 => "in_progress",
                _ => "done",
            };
            Some(ok(
                "setup",
                json!({"state": if status == "done" { "complete" } else { "needs_carbon" }, "steps": [
                    {"key": "wireless_debugging", "title": "Turn on wireless debugging", "status": status,
                     "error": "Wireless debugging is off. Turn it on in Developer options."}]}),
            ))
        }
        ("POST", "/api/v2/devices/7c1e09ab/setup/retry") => {
            Some(answer(202, "setup_retry", json!({"retrying": ["wireless_debugging"]})))
        }
        ("POST", "/api/v2/devices/2e7f00d1/setup/retry") => Some(err(
            426,
            "upgrade_required",
            "Studio Mac runs Silicon Extend 1.0.0, which can't retry from here. Update it to 1.1, or tap Retry on the device.",
        )),
        ("GET", "/api/v2/devices/2e7f00d1/setup") => Some(ok("setup", json!({"state": "needs_carbon", "steps": []}))),
        ("GET", "/api/v2/devices/7c1e09ab") => Some(ok("device", device("7c1e09ab", "Pixel", true))),
        _ => None,
    });
    let cli = Cli::new("setup-retry", &fake.url).signed_in("c:alice");
    let o = cli.run(&["device", "setup", "7c1e09ab"]);
    assert!(
        stdout(&o).contains("Retry: extend device setup 7c1e09ab --retry --step wireless_debugging"),
        "{}",
        stdout(&o)
    );
    let o = cli.run(&["device", "setup", "7c1e09ab", "--retry", "--step", "wireless_debugging"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(
        out.starts_with("Retrying Turn on wireless debugging on Pixel.")
            && out.contains("Done: Turn on wireless debugging."),
        "{out}"
    );
    assert_eq!(
        fake.requests("POST", "/api/v2/devices/7c1e09ab/setup/retry")[0].body,
        json!({"type": "setup_retry", "data": {"step": "wireless_debugging"}})
    );
    let o = cli.run(&["device", "setup", "2e7f00d1", "--retry"]);
    assert_eq!(o.status.code(), Some(13));
    assert!(stderr(&o).contains("Update it to 1.1"), "{}", stderr(&o));
    let o = cli.run(&["device", "setup", "7c1e09ab", "--step", "x"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn a_session_on_a_device_that_is_not_awake_starts_with_a_note() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("POST", "/api/v2/sessions") => {
            let mut s = session("a3f", "active", &["terminal"]);
            s["device"] = device_with(
                "0d44e1f2",
                "Living room TV",
                json!({"os": "android_tv", "kind": "tv", "awake": false, "sleep_state": "standby"}),
            );
            Some(answer(201, "session", s))
        }
        _ => None,
    });
    let cli = Cli::new("session-asleep", &fake.url).signed_in("si:chef");
    let o = cli.run(&["session", "new", "0d44e1f2"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "a3f");
    let err = stderr(&o);
    assert!(
        err.contains("Living room TV isn't awake (standby); commands that need its screen will fail until its Carbon turns it on. Ask: extend device wake 0d44e1f2 --reason \"...\""),
        "{err}"
    );
    assert!(
        err.contains("extend session connect a3f") && err.contains("extend session end a3f"),
        "{err}"
    );
    assert!(!err.contains("--team"), "{err}");
}

#[test]
fn device_show_explains_waking_sharing_and_access() {
    let fake = Fake::start(|r| match (r.method.as_str(), r.path_only()) {
        ("GET", "/api/v2/devices/2e7f00d1") => Some(ok(
            "device",
            device_with(
                "2e7f00d1",
                "Family Mac",
                json!({"os": "macos", "awake": false, "sleep_state": "locked", "awake_changed_at": "2026-09-27T10:02:00Z",
                       "paired_by_others": true, "wake_muted": false, "wake_detectable": false,
                       "wake_requests": [wake(1, json!({"device_id": "2e7f00d1"}))],
                       "missing": [{"capability": "terminal", "reason": "Several Carbons paired this computer. Only Silicons given access by the Carbon who installed Silicon Extend on it can use its terminal. The screen, keyboard and apps work as usual."}]}),
            ),
        )),
        ("GET", "/api/v2/devices/2e7f00d1/access") => Some(ok(
            "access",
            json!({"items": [
                {"device_id": "2e7f00d1", "silicon_id": "si:chef", "granted_by": "c:alice", "granted_at": "2026-09-27T10:00:00Z", "last_used_at": null},
                {"device_id": "2e7f00d1", "silicon_id": "si:scout", "granted_by": "c:alice", "granted_at": "2026-09-27T10:00:00Z", "last_used_at": null, "wake_muted": true}
            ]}),
        )),
        _ => None,
    });
    let o = Cli::new("show-owner", &fake.url)
        .signed_in("c:alice")
        .run(&["device", "show", "2e7f00d1"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    for want in [
        "Awake:     no (locked since",
        "Waking:    Extend can't tell when this Mac wakes",
        "Also paired by another Carbon",
        "Wake requests: on",
        "Access: si:chef, si:scout (wake requests off)",
        "Open wake requests:\n  si:chef, asked",
        "extend device wake-requests answer 2e7f00d1 woken",
        "Shared computer: Other Carbons paired this computer too.",
        "terminal       Several Carbons paired this computer.",
    ] {
        assert!(out.contains(want), "no {want:?} in:\n{out}");
    }
}

#[test]
fn banner_setting_uses_only_the_new_field_and_checks_arguments() {
    let f = Fake::start(|r| {
        if r.method == "PATCH" && r.path_only() == "/api/v2/devices/7c1e09ab" {
            let mut d = device("7c1e09ab", "Living room TV", true);
            d["in_use_indicator"] = r.body["data"]["in_use_indicator"].clone();
            Some(ok("device", d))
        } else {
            None
        }
    });
    let c = Cli::new("banner", &f.url).signed_in("c:alice");
    for (arg, value) in [("off", "hidden"), ("on", "shown")] {
        let o = c.run(&["--json", "device", "banner", "7c1e09ab", arg]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert_eq!(json_out(&o)["in_use_indicator"], value);
    }
    let requests = f.requests("PATCH", "/api/v2/devices/7c1e09ab");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].body,
        json!({"type":"device", "data":{"in_use_indicator":"hidden"}})
    );
    for args in [
        vec!["device", "banner", "7c1e09ab", "maybe"],
        vec!["device", "banner", "7c1e09ab"],
        vec!["device", "banner", "bad/id", "off"],
    ] {
        assert_eq!(c.run(&args).status.code(), Some(2));
    }
    assert_eq!(f.requests("PATCH", "/api/v2/devices/7c1e09ab").len(), 2);
}
