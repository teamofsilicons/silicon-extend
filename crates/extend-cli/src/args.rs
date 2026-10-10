//! Reading the command line: the global flags, and each command's own flags and arguments.
//!
//! Every Extend command (not the device commands, whose arguments are the device's) says which
//! flags it takes in [`SPECS`]; anything else is refused with the valid choices. `help.rs` must
//! document every flag listed here (a test checks it).

use extend_protocol::{COMMAND_TIMEOUT_MAX_MS, COMMAND_TIMEOUT_MIN_MS};

use crate::error::{CliError, R};

#[derive(Debug, Default, Clone)]
pub struct Globals {
    pub json: bool,
    pub session: Option<String>,
    pub timeout: Option<u64>,
    pub help: bool,
    pub version: bool,
    pub verbose: bool,
    /// A global flag Extend 4 removed (`--team`, `--test`), refused with what to do instead.
    pub retired: Option<&'static str>,
}

/// The global flags, for messages.
pub const GLOBAL_FLAG_NAMES: &[&str] = &[
    "--json",
    "--session <session_id>",
    "--timeout <ms>",
    "-h/--help",
    "-V/--version",
    "-v/--verbose",
];

/// Global flags Extend 3 had, each taking a value: read (with the value) so the command is refused
/// with what to do instead, not mistaken for another argument.
pub const RETIRED_GLOBAL_FLAGS: &[&str] = &["--team", "--test"];

/// Device commands whose arguments are a command line for the device itself (`adb shell df -h`).
/// Extend reads its own flags only between the command name and the device's first argument; from
/// that argument on, every token is sent exactly as typed, so `-h`, `-v`, `--json` and `--out`
/// reach the device. A `--` directly after the command name ends Extend's flags and is not sent.
pub const VERBATIM_COMMANDS: &[&str] = &["adb"];

/// Extend's own flags for device commands that make files, and whether each takes a value.
const FILE_FLAGS: &[(&str, bool)] = &[("--ttl", true), ("--keep", false), ("--out", true)];

fn file_flag_takes_value(token: &str) -> Option<bool> {
    FILE_FLAGS.iter().find(|(f, _)| *f == token).map(|(_, v)| *v)
}

pub fn missing_value(name: &str) -> CliError {
    match name {
        "--ttl" => CliError::usage(
            "--ttl needs a duration, like 7d",
            "Give it a duration from 1m to 30d: --ttl 90m, --ttl 12h, --ttl 7d.",
        ),
        "--out" => CliError::usage(
            "--out needs a local path",
            "Give the file or directory to save to: --out ./shot.png, or --out ./downloads/.",
        ),
        "--session" => CliError::usage(
            "--session needs a session id",
            "Give the session id `extend session new` printed: --session a3f.",
        ),
        "--timeout" => CliError::usage(
            "--timeout needs a number of milliseconds",
            "Give 1000–300000: --timeout 60000.",
        ),
        _ => CliError::usage(
            format!("{name} needs a value"),
            format!("Put the value right after it: {name} <value>. `extend <command> --help` shows what it takes."),
        ),
    }
}

/// One Extend command's own flags: the name, and whether it takes a value.
pub struct Spec {
    pub path: &'static str,
    pub flags: &'static [(&'static str, bool)],
}

/// Every Extend command and its flags, besides the global flags.
pub const SPECS: &[Spec] = &[
    Spec {
        path: "login",
        flags: &[
            ("--slt", true),
            ("--slt-stdin", false),
            ("--open", false),
            ("--label", true),
        ],
    },
    Spec {
        path: "login status",
        flags: &[("--offline", false)],
    },
    Spec {
        path: "logout",
        flags: &[],
    },
    Spec {
        path: "accounts",
        flags: &[],
    },
    Spec {
        path: "iam",
        flags: &[],
    },
    Spec {
        path: "silicon ls",
        flags: &[],
    },
    Spec {
        path: "silicon show",
        flags: &[],
    },
    Spec {
        path: "silicon renounce",
        flags: &[],
    },
    Spec {
        path: "config ls",
        flags: &[],
    },
    Spec {
        path: "config get",
        flags: &[],
    },
    Spec {
        path: "config set",
        flags: &[],
    },
    Spec {
        path: "config unset",
        flags: &[],
    },
    Spec {
        path: "config home",
        flags: &[("--use-existing", false)],
    },
    Spec {
        path: "device ls",
        flags: &[("--online", false), ("--os", true), ("--removed", false)],
    },
    Spec {
        path: "device show",
        flags: &[],
    },
    Spec {
        path: "device pair",
        flags: &[("--name", true), ("--ttl-days", true), ("--access", true)],
    },
    Spec {
        path: "device attach",
        flags: &[("--os", true), ("--name", true), ("--address", true)],
    },
    Spec {
        path: "device setup",
        flags: &[("--watch", false), ("--retry", false), ("--step", true)],
    },
    Spec {
        path: "device setup-code",
        flags: &[],
    },
    Spec {
        path: "device rename",
        flags: &[],
    },
    Spec {
        path: "device banner",
        flags: &[],
    },
    Spec {
        path: "device ttl",
        flags: &[],
    },
    Spec {
        path: "device stop",
        flags: &[],
    },
    Spec {
        path: "device rm",
        flags: &[("--yes", false)],
    },
    Spec {
        path: "device access",
        flags: &[],
    },
    Spec {
        path: "device activity",
        flags: &[
            ("--silicon", true),
            ("--since", true),
            ("--until", true),
            ("--limit", true),
        ],
    },
    Spec {
        path: "device requests",
        flags: &[],
    },
    Spec {
        path: "device wake",
        flags: &[("--reason", true), ("--cancel", false)],
    },
    Spec {
        path: "device wake-requests",
        flags: &[("--open", false), ("--wake-id", true), ("--silicon", true)],
    },
    Spec {
        path: "session new",
        flags: &[("--connect", false)],
    },
    Spec {
        path: "session connect",
        flags: &[],
    },
    Spec {
        path: "session disconnect",
        flags: &[],
    },
    Spec {
        path: "session status",
        flags: &[],
    },
    Spec {
        path: "session ls",
        flags: &[("--device", true), ("--state", true), ("--silicon", true)],
    },
    Spec {
        path: "session end",
        flags: &[],
    },
    Spec {
        path: "takeover",
        flags: &[("--reason", true)],
    },
    Spec {
        path: "request send",
        flags: &[("--reason", true)],
    },
    Spec {
        path: "request ls",
        flags: &[
            ("--sent", false),
            ("--received", false),
            ("--device", true),
            ("--silicon", true),
        ],
    },
    Spec {
        path: "ting status",
        flags: &[],
    },
    Spec {
        path: "ting on",
        flags: &[],
    },
    Spec {
        path: "file ls",
        flags: &[("--device", true), ("--kind", true), ("--silicon", true)],
    },
    Spec {
        path: "file show",
        flags: &[],
    },
    Spec {
        path: "file get",
        flags: &[("--out", true)],
    },
    Spec {
        path: "file keep",
        flags: &[],
    },
    Spec {
        path: "report",
        flags: &[("--pr", true)],
    },
    Spec {
        path: "version",
        flags: &[],
    },
    Spec {
        path: "docs",
        flags: &[],
    },
    Spec {
        path: "help",
        flags: &[],
    },
];

pub fn spec(path: &str) -> &'static Spec {
    SPECS
        .iter()
        .find(|s| s.path == path)
        .unwrap_or_else(|| panic!("no flag spec for `extend {path}`"))
}

/// Whether `token` (or its `--flag=` part) takes a value in any Extend command.
fn extend_value_flag(token: &str) -> bool {
    SPECS
        .iter()
        .flat_map(|s| s.flags.iter())
        .any(|(f, takes)| *takes && *f == token)
}

fn is_extend_command(word: &str) -> bool {
    SPECS
        .iter()
        .any(|s| s.path == word || s.path.split(' ').next() == Some(word))
}

fn parse_timeout(v: &str) -> R<u64> {
    let ms: u64 = v.parse().map_err(|_| {
        CliError::usage(
            format!("--timeout takes milliseconds, got {v:?}"),
            "Give a whole number of milliseconds, 1000–300000: --timeout 60000.",
        )
    })?;
    if !(COMMAND_TIMEOUT_MIN_MS..=COMMAND_TIMEOUT_MAX_MS).contains(&ms) {
        return Err(CliError::usage(
            format!("--timeout {ms} is outside 1000–300000 milliseconds"),
            "Give 1000 (1 second) to 300000 (5 minutes); the default is 30000.",
        ));
    }
    Ok(ms)
}

/// A refused command line, with the globals read before the problem.
pub type GlobalsError = Box<(Globals, CliError)>;

/// Takes the global flags out of argv. They may come anywhere before a `--`, except inside a
/// verbatim command's own arguments (see [`VERBATIM_COMMANDS`]) and in the value of an Extend
/// command's flag (`extend request send 7c1e09ab --reason -h` sends the reason `-h`).
///
/// On an error, the globals read so far come back too, so `--json` still shapes the error.
pub fn parse_globals(argv: Vec<String>) -> Result<(Globals, Vec<String>), GlobalsError> {
    let mut g = Globals::default();
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.into_iter();
    let mut passthrough = false;
    // Between a verbatim command's name and its first argument, where only Extend's flags go.
    let mut leading = false;
    // The first word is an Extend command (not a device command), whose flag values are taken
    // as they are.
    let mut extend_command = false;
    macro_rules! value {
        ($name:expr) => {
            match it.next() {
                Some(v) => v,
                None => return Err(Box::new((g, missing_value($name)))),
            }
        };
    }
    while let Some(a) = it.next() {
        if passthrough {
            rest.push(a);
            continue;
        }
        let flag_name = a.split_once('=').map_or(a.as_str(), |(k, _)| k);
        match a.as_str() {
            "--" => {
                passthrough = true;
                rest.push(a);
            }
            _ if extend_command && a.starts_with("--") && !a.contains('=') && extend_value_flag(flag_name) => {
                let v = it.next();
                rest.push(a);
                rest.extend(v);
            }
            "--json" => g.json = true,
            "-h" | "--help" => g.help = true,
            "-V" | "--version" => g.version = true,
            "-v" | "--verbose" => g.verbose = true,
            "--session" => g.session = Some(value!("--session")),
            _ if RETIRED_GLOBAL_FLAGS.contains(&flag_name) => {
                if !a.contains('=') {
                    let _ = it.next();
                }
                g.retired = RETIRED_GLOBAL_FLAGS.iter().copied().find(|f| *f == flag_name);
            }
            "--timeout" => {
                let v = value!("--timeout");
                match parse_timeout(&v) {
                    Ok(ms) => g.timeout = Some(ms),
                    Err(e) => return Err(Box::new((g, e))),
                }
            }
            _ if a.starts_with("--session=") => g.session = Some(a["--session=".len()..].to_owned()),
            _ if a.starts_with("--timeout=") => match parse_timeout(&a["--timeout=".len()..]) {
                Ok(ms) => g.timeout = Some(ms),
                Err(e) => return Err(Box::new((g, e))),
            },
            // `--ttl`, `--keep` and `--out` stay in place for the device command, with their
            // values, so a value is never mistaken for the device's first argument.
            _ if leading && file_flag_takes_value(&a).is_some() => {
                let v = if file_flag_takes_value(&a) == Some(true) {
                    Some(value!(&a))
                } else {
                    None
                };
                rest.push(a);
                rest.extend(v);
            }
            _ => {
                if leading {
                    // The device's command line starts here; nothing after this is Extend's.
                    passthrough = true;
                } else if rest.is_empty() && VERBATIM_COMMANDS.contains(&a.as_str()) {
                    leading = true;
                } else if rest.is_empty() && is_extend_command(&a) {
                    extend_command = true;
                }
                rest.push(a);
            }
        }
    }
    Ok((g, rest))
}

/// An Extend command's arguments: positionals, and the flags its [`Spec`] allows.
#[derive(Debug)]
pub struct Args {
    pub pos: Vec<String>,
    pub flags: Vec<(String, Option<String>)>,
    path: &'static str,
}

/// Plain `x`, `-`, and negative numbers are positional; `-x` and `--x` are flags.
fn looks_like_flag(a: &str) -> bool {
    a.len() > 1 && a.starts_with('-') && !a[1..].starts_with(|c: char| c.is_ascii_digit() || c == '.')
}

impl Args {
    /// Reads `argv` for `extend <path>`, refusing flags the command doesn't take.
    pub fn parse(argv: &[String], path: &str) -> R<Self> {
        let spec = spec(path);
        let mut pos = Vec::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if a == "--" {
                // Everything after `--` is positional, even if it looks like a flag.
                pos.extend(argv[i + 1..].iter().cloned());
                break;
            }
            if !looks_like_flag(a) {
                pos.push(a.clone());
                i += 1;
                continue;
            }
            let (name, inline) = match a.split_once('=').filter(|_| a.starts_with("--")) {
                Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                None => (a.clone(), None),
            };
            let Some((_, takes)) = spec.flags.iter().find(|(f, _)| *f == name) else {
                if let Some(e) = crate::retired::flag(spec.path, &name) {
                    return Err(e);
                }
                return Err(unknown_flag(spec, &name));
            };
            match (takes, inline) {
                (true, Some(v)) => flags.push((name, Some(v))),
                (true, None) => {
                    let Some(v) = argv.get(i + 1) else {
                        return Err(missing_value(&name).hint(format!(
                            "Put the value right after it. Usage: {}",
                            crate::help::usage_of(spec.path)
                        )));
                    };
                    flags.push((name, Some(v.clone())));
                    i += 1;
                }
                (false, Some(v)) => {
                    return Err(CliError::usage(
                        format!("{name} takes no value, but got {name}={v}"),
                        format!("Write {name} on its own. Usage: {}", crate::help::usage_of(spec.path)),
                    ));
                }
                (false, None) => flags.push((name, None)),
            }
            i += 1;
        }
        Ok(Self {
            pos,
            flags,
            path: spec.path,
        })
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }
    pub fn value(&self, name: &str) -> Option<String> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.clone())
    }
    pub fn values(&self, name: &str) -> Vec<String> {
        self.flags
            .iter()
            .filter(|(k, _)| k == name)
            .filter_map(|(_, v)| v.clone())
            .collect()
    }
    /// The `i`th positional, or a usage error naming it.
    pub fn req(&self, i: usize, what: &str) -> R<String> {
        self.pos.get(i).cloned().ok_or_else(|| {
            CliError::usage(
                format!("`extend {}` is missing the {what}", self.path),
                format!("Usage: {}", crate::help::usage_of(self.path)),
            )
        })
    }
    /// Refuses positionals past the first `n`.
    pub fn at_most(&self, n: usize) -> R<()> {
        if self.pos.len() <= n {
            return Ok(());
        }
        let extra = &self.pos[n..];
        Err(CliError::usage(
            format!(
                "`extend {}` takes {} argument{}, and got {} more: {}",
                self.path,
                if n == 0 { "no".to_owned() } else { n.to_string() },
                if n == 1 { "" } else { "s" },
                extra.len(),
                extra.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>().join(" ")
            ),
            format!(
                "Usage: {}. A value with spaces goes in quotes: \"like this\".",
                crate::help::usage_of(self.path)
            ),
        ))
    }
}

fn unknown_flag(spec: &Spec, name: &str) -> CliError {
    let known: Vec<String> = spec
        .flags
        .iter()
        .map(|(f, v)| if *v { format!("{f} <value>") } else { (*f).to_owned() })
        .collect();
    let near = spec
        .flags
        .iter()
        .map(|(f, _)| *f)
        .find(|f| similar(f, name))
        .map(|f| format!("Did you mean {f}? "))
        .unwrap_or_default();
    let choices = if known.is_empty() {
        format!("`extend {}` takes no flags of its own", spec.path)
    } else {
        format!("`extend {}` takes {}", spec.path, known.join(", "))
    };
    let dash = if name.starts_with("--") {
        String::new()
    } else {
        format!(" If {name} is a value, not a flag, put it after `--`.")
    };
    CliError::usage(
        format!("`extend {}` has no flag {name}", spec.path),
        format!(
            "{near}{choices}, plus the global flags ({}).{dash}",
            GLOBAL_FLAG_NAMES.join(", ")
        ),
    )
}

/// Close enough to be a typo: one edit apart, or the same after the first four letters.
fn similar(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 2 {
        return false;
    }
    // Levenshtein distance, small inputs.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()] <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unknown_flags_are_refused_with_the_choices() {
        let e = Args::parse(&strings(&["--onlinee"]), "device ls").unwrap_err();
        assert_eq!(e.message, "`extend device ls` has no flag --onlinee");
        let hint = e.hint.unwrap();
        assert!(hint.starts_with("Did you mean --online? "), "{hint}");
        assert!(hint.contains("--os <value>") && hint.contains("--removed"), "{hint}");
        let e = Args::parse(&strings(&["--bogus"]), "logout").unwrap_err();
        assert!(
            e.hint.unwrap().contains("`extend logout` takes no flags of its own"),
            "{}",
            e.message
        );
        let e = Args::parse(&strings(&["7c1e09ab", "-x"]), "device show").unwrap_err();
        assert!(e.hint.unwrap().contains("put it after `--`"));
        // Values of flags, `--` and negative numbers are not flags.
        let a = Args::parse(&strings(&["7c1e09ab", "--", "-x"]), "device rename").unwrap();
        assert_eq!(a.pos, strings(&["7c1e09ab", "-x"]));
        let a = Args::parse(&strings(&["7c1e09ab", "-5"]), "device ttl").unwrap();
        assert_eq!(a.pos, strings(&["7c1e09ab", "-5"]));
        let e = Args::parse(&strings(&["--online=yes"]), "device ls").unwrap_err();
        assert!(e.message.contains("--online takes no value"), "{}", e.message);
        let e = Args::parse(&strings(&["x", "--name"]), "device pair").unwrap_err();
        assert!(e.message.contains("--name needs a value"), "{}", e.message);
    }

    #[test]
    fn login_takes_a_token_three_ways_and_the_device_flow_flags() {
        let a = Args::parse(&strings(&["slt_abc"]), "login").unwrap();
        assert_eq!(a.pos, strings(&["slt_abc"]));
        let a = Args::parse(&strings(&["--slt", "slt_abc"]), "login").unwrap();
        assert_eq!(a.value("--slt").as_deref(), Some("slt_abc"));
        let a = Args::parse(&strings(&["--slt=slt_abc"]), "login").unwrap();
        assert_eq!(a.value("--slt").as_deref(), Some("slt_abc"));
        assert!(
            Args::parse(&strings(&["--slt-stdin"]), "login")
                .unwrap()
                .flag("--slt-stdin")
        );
        let a = Args::parse(&strings(&["--open", "--label", "build box"]), "login").unwrap();
        assert!(a.flag("--open"));
        assert_eq!(a.value("--label").as_deref(), Some("build box"));
        assert!(
            Args::parse(&strings(&["--offline"]), "login status")
                .unwrap()
                .flag("--offline")
        );
        // A token that looks like a global flag is still the token.
        let (g, rest) = parse_globals(strings(&["login", "--slt", "--json"])).unwrap();
        assert!(!g.json);
        assert_eq!(rest, strings(&["login", "--slt", "--json"]));
    }

    #[test]
    fn retired_global_flags_are_read_with_their_value() {
        let (g, rest) = parse_globals(strings(&["--team", "acme", "device", "ls"])).unwrap();
        assert_eq!(g.retired, Some("--team"));
        assert_eq!(rest, strings(&["device", "ls"]));
        let (g, rest) = parse_globals(strings(&["device", "ls", "--test=9b3e"])).unwrap();
        assert_eq!(g.retired, Some("--test"));
        assert_eq!(rest, strings(&["device", "ls"]));
        // Inside adb's own arguments they are the device's.
        let (g, rest) = parse_globals(strings(&["adb", "shell", "tool", "--team", "x"])).unwrap();
        assert_eq!(g.retired, None);
        assert_eq!(rest, strings(&["adb", "shell", "tool", "--team", "x"]));
    }

    #[test]
    fn extra_arguments_are_refused() {
        let a = Args::parse(&strings(&["a", "b"]), "device show").unwrap();
        let e = a.at_most(1).unwrap_err();
        assert!(
            e.message
                .contains("`extend device show` takes 1 argument, and got 1 more: \"b\""),
            "{}",
            e.message
        );
        assert!(e.hint.unwrap().starts_with("Usage: extend device show <device_id>"));
    }

    #[test]
    fn an_extend_flag_value_is_never_a_global_flag() {
        let (g, rest) = parse_globals(strings(&["request", "send", "7c1e09ab", "--reason", "-h"])).unwrap();
        assert!(!g.help);
        assert_eq!(rest, strings(&["request", "send", "7c1e09ab", "--reason", "-h"]));
        let (g, rest) = parse_globals(strings(&["device", "pair", "4f9c2a", "--name", "--json"])).unwrap();
        assert!(!g.json);
        assert_eq!(rest[3..], strings(&["--name", "--json"]));
        // A global flag elsewhere still counts.
        let (g, _) = parse_globals(strings(&["request", "send", "x", "--reason", "hi", "--json"])).unwrap();
        assert!(g.json);
    }

    #[test]
    fn a_parse_error_keeps_the_globals_read_so_far() {
        let (g, e) = *parse_globals(strings(&["--json", "device", "ls", "--timeout", "5"])).unwrap_err();
        assert!(g.json);
        assert!(e.message.contains("outside 1000–300000"), "{}", e.message);
    }

    #[test]
    fn every_spec_names_a_documented_command() {
        // `iam` is the hidden alias of `accounts`, kept for the Silicon runtime for one release.
        for s in SPECS.iter().filter(|s| s.path != "iam") {
            assert!(
                crate::help::find(s.path).is_some() || crate::help::find(s.path.split(' ').next().unwrap()).is_some(),
                "`extend {}` has no help node",
                s.path
            );
        }
    }
}
