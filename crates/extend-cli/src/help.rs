//! The documentation tree bundled in the CLI: every node answers `--help` with what it's for, how
//! it's usually used with other commands, its arguments and flags, and the nodes under it.

use extend_protocol::capability::{COMMANDS, CommandSpec, Origin};

pub struct Node {
    pub path: &'static str,
    pub usage: &'static str,
    pub purpose: &'static str,
    pub used_with: &'static str,
    pub flags: &'static [(&'static str, &'static str)],
    pub examples: &'static [&'static str],
    pub who: &'static str,
}

pub const REPO: &str = "https://github.com/teamofsilicons/silicon-extend";
pub const DOCS: &str = "https://extend.teamofsilicons.com/docs";
pub const CRATE: &str = "https://crates.io/crates/silicon-extend-client";
pub const WEBSITE: &str = "https://extend.teamofsilicons.com";

pub const NODES: &[Node] = &[
    Node {
        path: "login",
        usage: "extend login <slt>",
        who: "Carbon or Silicon",
        purpose: "Sign in with a short-lived token (SLT) from Silicon IAM. Extend never asks for a password.",
        used_with: "Get the SLT with the IAM CLI (`iam login --app-id extend ...`) or the IAM consent screen, then run this. Check it worked with `extend login status`. In a test environment (`extend --test <test_id> login si:chef`) a test member id works instead of an SLT.",
        flags: &[],
        examples: &["extend login oac_...", "extend --test 9b3e0c1a-... login si:chef"],
    },
    Node {
        path: "login status",
        usage: "extend login status [--json]",
        who: "anyone",
        purpose: "Show who is signed in, checked live with Silicon IAM.",
        used_with: "Scripts check `authenticated` in `--json` output before running other commands; exit code 3 means not signed in.",
        flags: &[("--json", "{authenticated, member, teams, team, ...}")],
        examples: &["extend login status --json"],
    },
    Node {
        path: "logout",
        usage: "extend logout",
        who: "Carbon or Silicon",
        purpose: "Sign out and delete the saved tokens. A Silicon signing out ends its running sessions.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "iam",
        usage: "extend iam [--json]",
        who: "anyone, no login",
        purpose: "Show Extend's public Silicon IAM details: app_id, IAM URL, API URL.",
        used_with: "Use app_id when generating an SLT for Extend with the IAM CLI.",
        flags: &[(
            "--json",
            "{app_id, iam_base_url, api_base_url, website_url, docs_url, repository_url}",
        )],
        examples: &["extend iam --json"],
    },
    Node {
        path: "team",
        usage: "extend team ls | extend team silicons | extend team use <handle>",
        who: "Carbon or Silicon",
        purpose: "List the teams this login reaches, the Silicons in the team (to grant access), or choose the default team.",
        used_with: "Any command also takes --team <handle> for one run.",
        flags: &[],
        examples: &["extend team use acme"],
    },
    Node {
        path: "config",
        usage: "extend config ls | get <key> | set <key> <value> | unset <key> | home <dir> | test add|ls|rm",
        who: "anyone",
        purpose: "Read and change CLI settings, where state is kept, and saved test environments.",
        used_with: "`extend config home <dir>` moves state (default $SILICON_HOME/.extend or ~/.extend). `extend config test add <test_id>` reads a test app secret from stdin so `--test <test_id>` works.",
        flags: &[],
        examples: &[
            "extend config set telemetry off",
            "extend config home /data/si-chef",
            "printf %s \"$SECRET\" | extend config test add 9b3e0c1a-...",
        ],
    },
    Node {
        path: "device",
        usage: "extend device <ls|show|pair|attach|setup|setup-code|rename|visibility|ttl|stop|rm|access|activity|requests>",
        who: "Carbon or Silicon",
        purpose: "Find devices (Silicons: the ones you can use) and manage them (Carbons: the ones you paired).",
        used_with: "A Silicon runs `extend device ls`, then `extend device show <device_id>` to see what it can do there, then `extend session new <device_id>`.",
        flags: &[],
        examples: &["extend device ls", "extend device show 7c1e09ab"],
    },
    Node {
        path: "device ls",
        usage: "extend device ls [--online] [--os <os>] [--team-visible] [--json]",
        who: "Carbon or Silicon",
        purpose: "Silicon: every device you have access to. Carbon: every device you paired; --team-visible lists other Carbons' team-visible devices.",
        used_with: "Pick a device id from here for `extend device show` or `extend session new`.",
        flags: &[
            ("--online", "only online devices"),
            (
                "--os <os>",
                "android, android_tv, macos, windows, linux, ios, ipados, tvos, samsung_tv, lg_tv",
            ),
            ("--team-visible", "Carbons: other Carbons' devices"),
        ],
        examples: &["extend device ls --online"],
    },
    Node {
        path: "device show",
        usage: "extend device show <device_id> [--json]",
        who: "Carbon or Silicon",
        purpose: "One device, and exactly what a Silicon can do on it right now given its OS and permissions, plus what's missing and why.",
        used_with: "Read before starting a session so you know which commands will work.",
        flags: &[],
        examples: &["extend device show 7c1e09ab"],
    },
    Node {
        path: "device pair",
        usage: "extend device pair <pairing_code> --name <name> [--visibility team|personal] [--ttl-days <1-30>] [--access <silicon_id>]...",
        who: "Carbon",
        purpose: "Pair the device showing this code into your team. The code is 6 hexadecimal characters, rotates every 5 minutes and works once.",
        used_with: "Then follow the device's own setup with `extend device setup <device_id> --watch`.",
        flags: &[
            ("--name <name>", "1–64 characters, required"),
            ("--visibility", "team (default) or personal"),
            (
                "--ttl-days <n>",
                "days without activity before the pair ends, 1–30, default 14",
            ),
            ("--access <silicon_id>", "give a Silicon access now; repeatable"),
        ],
        examples: &["extend device pair 4f9c2a --name \"Saket's Pixel\" --access si:chef"],
    },
    Node {
        path: "device attach",
        usage: "extend device attach <host_device_id> --os ios|ipados|tvos|samsung_tv|lg_tv --name <name> [--address <ip>]",
        who: "Carbon",
        purpose: "Pair an iPhone, iPad, Apple TV or Samsung/LG TV through a paired Mac or computer on the same network.",
        used_with: "Then `extend device setup <device_id> --watch`; an Apple TV also needs `extend device setup-code <device_id> <code>`.",
        flags: &[],
        examples: &["extend device attach 2e7f00d1 --os ios --name \"Saket's iPhone\""],
    },
    Node {
        path: "device setup",
        usage: "extend device setup <device_id> [--watch]",
        who: "Carbon (owner)",
        purpose: "The device's own setup steps (debugging, permissions) and which are left. --watch follows until complete.",
        used_with: "",
        flags: &[("--watch", "refresh every 2 s until setup completes")],
        examples: &[],
    },
    Node {
        path: "device setup-code",
        usage: "extend device setup-code <device_id> <code>",
        who: "Carbon (owner)",
        purpose: "Enter the 4-digit code an Apple TV shows during setup.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "device rename",
        usage: "extend device rename <device_id> <name>",
        who: "Carbon (owner)",
        purpose: "Rename a device.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "device visibility",
        usage: "extend device visibility <device_id> team|personal",
        who: "Carbon (owner)",
        purpose: "Choose whether other Carbons in the team can see the device exists.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "device ttl",
        usage: "extend device ttl <device_id> <days>",
        who: "Carbon (owner)",
        purpose: "Set how long the device stays paired without activity: 1–30 days.",
        used_with: "",
        flags: &[],
        examples: &["extend device ttl 7c1e09ab 30"],
    },
    Node {
        path: "device stop",
        usage: "extend device stop <device_id>",
        who: "Carbon (owner)",
        purpose: "Stop the Silicon using the device right now.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "device rm",
        usage: "extend device rm <device_id> --yes",
        who: "Carbon (owner)",
        purpose: "Remove the device: ends any session and every Silicon's access, and unpairs it. Devices paired through it go too.",
        used_with: "Without --yes it only says what would happen.",
        flags: &[("--yes", "confirm")],
        examples: &[],
    },
    Node {
        path: "device access",
        usage: "extend device access ls <device_id> | grant <device_id> <silicon_id>... | revoke <device_id> <silicon_id>...",
        who: "Carbon (owner)",
        purpose: "See, give and take away which Silicons can use a device. Taking access away ends that Silicon's running session at once.",
        used_with: "",
        flags: &[],
        examples: &["extend device access grant 7c1e09ab si:chef"],
    },
    Node {
        path: "device activity",
        usage: "extend device activity <device_id> [--silicon <id>] [--session <id>] [--since <time>] [--until <time>] [--limit <n>]",
        who: "Carbon (owner)",
        purpose: "Every action on the device, which Silicon did it, and when. Typed text is redacted.",
        used_with: "",
        flags: &[("--since/--until", "RFC 3339, or relative like 2h, 3d")],
        examples: &["extend device activity 7c1e09ab --since 2h"],
    },
    Node {
        path: "device requests",
        usage: "extend device requests <device_id>",
        who: "Carbon (owner)",
        purpose: "Requests Silicons sent each other for this device, with their reasons.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "session",
        usage: "extend session <new|connect|disconnect|status|ls|end>",
        who: "Silicon",
        purpose: "Use a device. Only one Silicon at a time; a session ends on its own after 5 minutes without a command.",
        used_with: "`extend session new <device_id> --connect`, then device commands like `extend snapshot -i` and `extend click @e2`, then `extend session end`.",
        flags: &[],
        examples: &["extend session new 7c1e09ab --connect", "extend session end"],
    },
    Node {
        path: "session new",
        usage: "extend session new <device_id> [--connect]",
        who: "Silicon",
        purpose: "Start using a device. Prints the session id (3+ hexadecimal characters). If another Silicon is using it you get exit 6 and the command to ask for it.",
        used_with: "Add --connect to connect at once.",
        flags: &[("--connect", "also connect to the new session")],
        examples: &[],
    },
    Node {
        path: "session connect",
        usage: "extend session connect <session_id>",
        who: "Silicon",
        purpose: "Make this the session later commands run in, and load its device's commands into --help.",
        used_with: "",
        flags: &[],
        examples: &["extend session connect a3f"],
    },
    Node {
        path: "session disconnect",
        usage: "extend session disconnect",
        who: "Silicon",
        purpose: "Forget the connected session locally. It keeps running until ended or idle.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "session status",
        usage: "extend session status [<session_id>]",
        who: "Silicon",
        purpose: "State, device, commands run, and when it ends for inactivity.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "session ls",
        usage: "extend session ls [--device <device_id>] [--state active|paused|ended]",
        who: "Carbon or Silicon",
        purpose: "Silicon: your sessions. Carbon: sessions on your devices.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "session end",
        usage: "extend session end [<session_id>]",
        who: "Silicon",
        purpose: "Finish and free the device for other Silicons. Defaults to the connected session.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "takeover",
        usage: "extend takeover --reason \"<text>\" | extend takeover status | extend takeover release",
        who: "Silicon",
        purpose: "Hand the device to its Carbon (Face ID, a payment, an admin prompt). Commands are refused until they tap Done or you release. Up to 30 minutes.",
        used_with: "",
        flags: &[("--reason", "1–300 characters, shown on the device and website")],
        examples: &["extend takeover --reason \"Please approve the Face ID prompt\""],
    },
    Node {
        path: "request",
        usage: "extend request send <device_id> --reason \"<text>\" | extend request ls [--sent|--received] [--device <id>]",
        who: "Silicon",
        purpose: "Ask the Silicon using a device for it. Delivered through Ting with your reason exactly as written (1–300 characters).",
        used_with: "Use after `extend session new` says the device is in use.",
        flags: &[],
        examples: &["extend request send 7c1e09ab --reason \"Need 2 minutes to read an OTP\""],
    },
    Node {
        path: "file",
        usage: "extend file ls [--session <id>] [--device <id>] [--kind <kind>] | show <file_id> | get <file_id> [--out <path>] | keep <file_id>",
        who: "Carbon or Silicon",
        purpose: "Files made in sessions (screenshots, recordings, logs, scripts), stored in Briefcase. They self-destruct after 1 day unless made with --ttl or kept.",
        used_with: "Device commands that make files also take --ttl <1m–30d>, --keep and --out <path>.",
        flags: &[],
        examples: &["extend file keep 6e1f2a9c-..."],
    },
    Node {
        path: "report",
        usage: "extend report \"<message>\" [--pr <url>]",
        who: "Carbon or Silicon",
        purpose: "Report a bug to the Extend team. If you already fixed it, attach the pull request.",
        used_with: "Extend is open source: reproduce, patch and open a PR at https://github.com/teamofsilicons/silicon-extend, then report with --pr.",
        flags: &[("--pr <url>", "link to your pull request")],
        examples: &[],
    },
    Node {
        path: "env",
        usage: "extend --test <test_id> env show",
        who: "anyone (test environments only)",
        purpose: "The test environment's name, state, your test identity, and paired devices out of 5.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "version",
        usage: "extend version [--json]",
        who: "anyone",
        purpose: "CLI, client crate and negotiated API versions.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "docs",
        usage: "extend docs",
        who: "anyone",
        purpose: "Links: repository, docs, website, Rust crate; and where state is kept.",
        used_with: "",
        flags: &[],
        examples: &[],
    },
];

pub const GLOBAL_FLAGS: &[(&str, &str)] = &[
    (
        "--json",
        "one JSON document on stdout: {\"ok\": true, \"data\": ...} or {\"ok\": false, \"error\": {...}}",
    ),
    (
        "--test <test_id>",
        "run in a test environment added with `extend config test add` (printed on stderr at the end)",
    ),
    (
        "--session <session_id>",
        "use this session instead of the connected one (also EXTEND_SESSION)",
    ),
    ("--team <handle>", "use this team for one command"),
    ("--timeout <ms>", "device command deadline, 1000–300000 (default 30000)"),
    ("-h, --help", "help for this part of the tree"),
    ("-V, --version", "versions"),
    ("-v, --verbose", "request ids and timings on stderr"),
];

pub fn find(path: &str) -> Option<&'static Node> {
    NODES.iter().find(|n| n.path == path)
}

pub fn render_node(n: &Node) -> String {
    let mut s = format!(
        "extend {}\n\n{}\n\nUsage:\n  {}\n\nWho: {}\n",
        n.path, n.purpose, n.usage, n.who
    );
    if !n.used_with.is_empty() {
        s.push_str(&format!("\nHow it's used:\n  {}\n", n.used_with));
    }
    if !n.flags.is_empty() {
        s.push_str("\nFlags:\n");
        for (f, d) in n.flags {
            s.push_str(&format!("  {f:<24} {d}\n"));
        }
    }
    let children: Vec<&Node> = NODES
        .iter()
        .filter(|c| c.path.starts_with(&format!("{} ", n.path)))
        .collect();
    if !children.is_empty() {
        s.push_str("\nUnder this:\n");
        for c in children {
            s.push_str(&format!("  extend {:<22} {}\n", c.path, first_sentence(c.purpose)));
        }
        s.push_str(&format!("\nGo deeper: extend {} <command> --help\n", n.path));
    }
    if !n.examples.is_empty() {
        s.push_str("\nExamples:\n");
        for e in n.examples {
            s.push_str(&format!("  {e}\n"));
        }
    }
    s
}

fn first_sentence(s: &str) -> &str {
    s.split(". ").next().unwrap_or(s).trim_end_matches('.')
}

pub fn render_device_command(name: &str) -> Option<String> {
    let c = COMMANDS.iter().find(|c| c.name == name)?;
    let caps: Vec<&str> = c.any_of.iter().map(|x| x.as_str()).collect();
    let origin = match c.origin {
        Origin::AgentDevice => "an agent-device command, run on the session's device through Extend",
        Origin::Extend => "an Extend command (not part of agent-device)",
    };
    let placement = match name {
        "adb" => ADB_ARGUMENTS,
        "install" | "reinstall" => INSTALL_ARGUMENTS,
        _ => {
            "They may come before or after the command's arguments. `--` ends Extend's flags: everything after it goes to the device as written, even `-h` or `--json`.\n"
        }
    };
    let more = match c.origin {
        Origin::AgentDevice => format!(
            "\nArgument details follow agent-device: {REPO}/tree/main/vendor/agent-device/website/docs/docs/commands.md\n"
        ),
        Origin::Extend => String::new(),
    };
    Some(format!(
        "extend {name}\n\n{}.\n\nUsage:\n  extend {}\n\nThis is {origin}. It needs a session (`extend session new <device_id> --connect`) and a device with: {}.\n\nFlags for any device command:\n  --session <id>          which session (default: the connected one)\n  --timeout <ms>          deadline, 1000–300000 (default 30000)\n  --json                  full structured result\n  --ttl <1m–30d>          self-destruct for files this command makes (default 1d)\n  --keep                  keep files this command makes permanently\n  --out <path>            also save the files locally\n\n{placement}{more}",
        c.summary,
        usage(c),
        caps.join(" or "),
    ))
}

/// Usage lines the CLI prints instead of the shared command table's (`COMMANDS` in
/// extend-protocol), where that table is behind understanding/cli.yaml: it says
/// `install <app> <file_id|path>`, but only a local .apk works, and it doesn't show where
/// Extend's flags go for `adb`.
const USAGE_OVERRIDES: &[(&str, &str)] = &[
    ("install", "install <package> <path.apk>"),
    ("reinstall", "reinstall <package> <path.apk>"),
    ("adb", "adb [<extend flags>] [--] <args>..."),
];

fn usage(c: &CommandSpec) -> &'static str {
    USAGE_OVERRIDES
        .iter()
        .find(|(n, _)| *n == c.name)
        .map_or(c.usage, |(_, u)| u)
}

/// What `extend install` / `extend reinstall` take.
const INSTALL_ARGUMENTS: &str = "\
Extend's flags may come before or after the command's arguments.

<path.apk> is an .apk file on this computer; it is sent with the command, so it can be at most
8 MiB (8388608 bytes). A missing path, a directory, a link or a Briefcase file id is refused before
anything is sent: download a Briefcase file first with extend file get <file_id> --out ./app.apk.
An .aab bundle is refused too: build a universal APK from it with bundletool first.
For a larger APK, the error explains how to push it in parts and install it with
extend adb shell pm install -r /data/local/tmp/<file>.
";

/// How `extend adb` reads its arguments (see `VERBATIM_COMMANDS` in main.rs).
const ADB_ARGUMENTS: &str = "\
Everything after the first adb argument goes to the device exactly as typed, including -h, -v,
--json and --out (only pull's local path is kept here; see below). Put Extend's flags before it:
  extend --json adb shell df -h
  extend adb --timeout 60000 shell sleep 40
A `--` right after `adb` ends Extend's flags and is not sent: extend adb -- shell grep -v error /sdcard/app.log

Local files stay on this computer's side:
  push <local file> <device path>     sends the local file with the command
  install [-r] <local .apk>           sends the local APK with the command
  pull <device path> [<local path>]   saves the pulled file locally; --out <local path> before or
                                      after the device path does the same
push and install take a file on this computer only: a missing path, a directory, a link or a
Briefcase file id is refused before anything is sent (download a Briefcase file first with
extend file get <file_id> --out <local path>). A command carries at most 8 MiB of local files. For a
larger file, split it into parts of 8 MiB or less (split -b 8m), push each part to its own path under
/data/local/tmp, and join them with extend adb shell 'cat <parts> > <file>'.
";

/// Top-level help. `connected` narrows the device commands to what the session's device can do.
pub fn render_top(connected: Option<(&str, &str, &str, &[String])>) -> String {
    let mut s = String::from(
        "extend — let a Silicon use the devices a Carbon has paired, and let the Carbon manage them.\n\n\
Getting started\n\
  honeycomb install 'extend'          install the CLI\n\
  extend login <slt>                  sign in with a short-lived token from Silicon IAM\n\
  extend device ls                    devices you can use (Silicon) or have paired (Carbon)\n\
  extend device show <device_id>      what a Silicon can do on a device\n\
  extend session new <device_id> --connect\n\
  extend snapshot -i                  read the screen; then act: extend click @e2\n\
  extend session end                  free the device for other Silicons\n\n\
Commands (extend <command> --help goes deeper)\n",
    );
    for n in NODES.iter().filter(|n| !n.path.contains(' ')) {
        s.push_str(&format!("  {:<12} {}\n", n.path, first_sentence(n.purpose)));
    }
    match connected {
        Some((sid, device, os, cmds)) => {
            s.push_str(&format!("\nDevice commands — connected to session {sid} on {device} ({os}); only commands that work there are shown\n"));
            for c in COMMANDS.iter().filter(|c| cmds.iter().any(|x| x == c.name)) {
                s.push_str(&format!("  {:<14} {}\n", c.name, c.summary));
            }
        }
        None => {
            s.push_str("\nDevice commands (need a session; `extend --help` while connected shows only what that device can do)\n");
            for c in COMMANDS {
                s.push_str(&format!("  {:<14} {}\n", c.name, c.summary));
            }
        }
    }
    s.push_str("\nGlobal flags (anywhere before a `--`; for `extend adb`, only before its first argument)\n");
    for (f, d) in GLOBAL_FLAGS {
        s.push_str(&format!("  {f:<24} {d}\n"));
    }
    s.push_str(&format!(
        "\nState lives in {} (move it with `extend config home <dir>`).\nDocs {DOCS} · Source {REPO} · Rust crate {CRATE}\nFound a bug? `extend report \"...\" --pr <link>` — Extend is open source, patches welcome.\n",
        crate::store::root().display()
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_help_matches_the_contract() {
        let contract = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../understanding/cli.yaml"),
        )
        .unwrap();
        for name in ["install", "reinstall"] {
            let help = render_device_command(name).unwrap();
            let usage_line = help.lines().skip_while(|l| *l != "Usage:").nth(1).unwrap();
            assert!(
                !usage_line.contains("file_id"),
                "`extend {name} --help` offers a file id: {usage_line}"
            );
            let line = format!("extend {}", usage(COMMANDS.iter().find(|c| c.name == name).unwrap()));
            assert!(help.contains(&format!("Usage:\n  {line}\n")), "{help}");
            assert!(contract.contains(&line), "understanding/cli.yaml has no `{line}`");
            assert!(
                help.contains("at most\n8 MiB") && help.contains("extend file get <file_id> --out ./app.apk"),
                "{help}"
            );
        }
    }
}
