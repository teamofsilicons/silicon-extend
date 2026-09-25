//! The documentation tree bundled in the CLI: every node answers `--help` with what it's for, how
//! it's usually used with other commands, its arguments and flags, and the nodes under it.

use bridge_protocol::capability::{COMMANDS, Origin};

pub struct Node {
    pub path: &'static str,
    pub usage: &'static str,
    pub purpose: &'static str,
    pub used_with: &'static str,
    pub flags: &'static [(&'static str, &'static str)],
    pub examples: &'static [&'static str],
    pub who: &'static str,
}

pub const REPO: &str = "https://github.com/teamofsilicons/silicon-bridge";
pub const DOCS: &str = "https://bridge.teamofsilicons.com/docs";
pub const CRATE: &str = "https://crates.io/crates/silicon-bridge-client";
pub const WEBSITE: &str = "https://bridge.teamofsilicons.com";

pub const NODES: &[Node] = &[
    Node { path: "login", usage: "bridge login <slt>", who: "Carbon or Silicon",
        purpose: "Sign in with a short-lived token (SLT) from Silicon IAM. Bridge never asks for a password.",
        used_with: "Get the SLT with the IAM CLI (`iam login --app-id bridge ...`) or the IAM consent screen, then run this. Check it worked with `bridge login status`. In a test environment (`bridge --test <test_id> login si:chef`) a test member id works instead of an SLT.",
        flags: &[], examples: &["bridge login oac_...", "bridge --test 9b3e0c1a-... login si:chef"] },
    Node { path: "login status", usage: "bridge login status [--json]", who: "anyone",
        purpose: "Show who is signed in, checked live with Silicon IAM.",
        used_with: "Scripts check `authenticated` in `--json` output before running other commands; exit code 3 means not signed in.",
        flags: &[("--json", "{authenticated, member, teams, team, ...}")], examples: &["bridge login status --json"] },
    Node { path: "logout", usage: "bridge logout", who: "Carbon or Silicon",
        purpose: "Sign out and delete the saved tokens. A Silicon signing out ends its running sessions.", used_with: "", flags: &[], examples: &[] },
    Node { path: "iam", usage: "bridge iam [--json]", who: "anyone, no login",
        purpose: "Show Bridge's public Silicon IAM details: app_id, IAM URL, API URL.", used_with: "Use app_id when generating an SLT for Bridge with the IAM CLI.",
        flags: &[("--json", "{app_id, iam_base_url, api_base_url, website_url, docs_url, repository_url}")], examples: &["bridge iam --json"] },
    Node { path: "team", usage: "bridge team ls | bridge team use <handle>", who: "Carbon or Silicon",
        purpose: "List the teams this login reaches, or choose the default team for later commands.", used_with: "Any command also takes --team <handle> for one run.", flags: &[], examples: &["bridge team use acme"] },
    Node { path: "config", usage: "bridge config ls | get <key> | set <key> <value> | unset <key> | home <dir> | test add|ls|rm", who: "anyone",
        purpose: "Read and change CLI settings, where state is kept, and saved test environments.",
        used_with: "`bridge config home <dir>` moves state (default $SILICON_HOME/.bridge or ~/.bridge). `bridge config test add <test_id>` reads a test app secret from stdin so `--test <test_id>` works.",
        flags: &[], examples: &["bridge config set telemetry off", "bridge config home /data/si-chef", "printf %s \"$SECRET\" | bridge config test add 9b3e0c1a-..."] },
    Node { path: "device", usage: "bridge device <ls|show|pair|attach|setup|setup-code|rename|visibility|ttl|stop|rm|access|activity|requests>", who: "Carbon or Silicon",
        purpose: "Find devices (Silicons: the ones you can use) and manage them (Carbons: the ones you paired).",
        used_with: "A Silicon runs `bridge device ls`, then `bridge device show <device_id>` to see what it can do there, then `bridge session new <device_id>`.",
        flags: &[], examples: &["bridge device ls", "bridge device show 7c1e09ab"] },
    Node { path: "device ls", usage: "bridge device ls [--online] [--os <os>] [--team-visible] [--json]", who: "Carbon or Silicon",
        purpose: "Silicon: every device you have access to. Carbon: every device you paired; --team-visible lists other Carbons' team-visible devices.",
        used_with: "Pick a device id from here for `bridge device show` or `bridge session new`.",
        flags: &[("--online", "only online devices"), ("--os <os>", "android, android_tv, macos, windows, linux, ios, ipados, tvos, samsung_tv, lg_tv"), ("--team-visible", "Carbons: other Carbons' devices")],
        examples: &["bridge device ls --online"] },
    Node { path: "device show", usage: "bridge device show <device_id> [--json]", who: "Carbon or Silicon",
        purpose: "One device, and exactly what a Silicon can do on it right now given its OS and permissions, plus what's missing and why.",
        used_with: "Read before starting a session so you know which commands will work.", flags: &[], examples: &["bridge device show 7c1e09ab"] },
    Node { path: "device pair", usage: "bridge device pair <pairing_code> --name <name> [--visibility team|personal] [--ttl-days <1-30>] [--access <silicon_id>]...", who: "Carbon",
        purpose: "Pair the device showing this code into your team. The code is 6 hexadecimal characters, rotates every 5 minutes and works once.",
        used_with: "Then follow the device's own setup with `bridge device setup <device_id> --watch`.",
        flags: &[("--name <name>", "1–64 characters, required"), ("--visibility", "team (default) or personal"), ("--ttl-days <n>", "days without activity before the pair ends, 1–30, default 14"), ("--access <silicon_id>", "give a Silicon access now; repeatable")],
        examples: &["bridge device pair 4f9c2a --name \"Saket's Pixel\" --access si:chef"] },
    Node { path: "device attach", usage: "bridge device attach <host_device_id> --os ios|ipados|tvos|samsung_tv|lg_tv --name <name> [--address <ip>]", who: "Carbon",
        purpose: "Pair an iPhone, iPad, Apple TV or Samsung/LG TV through a paired Mac or computer on the same network.",
        used_with: "Then `bridge device setup <device_id> --watch`; an Apple TV also needs `bridge device setup-code <device_id> <code>`.", flags: &[], examples: &["bridge device attach 2e7f00d1 --os ios --name \"Saket's iPhone\""] },
    Node { path: "device setup", usage: "bridge device setup <device_id> [--watch]", who: "Carbon (owner)",
        purpose: "The device's own setup steps (debugging, permissions) and which are left. --watch follows until complete.", used_with: "", flags: &[("--watch", "refresh every 2 s until setup completes")], examples: &[] },
    Node { path: "device setup-code", usage: "bridge device setup-code <device_id> <code>", who: "Carbon (owner)", purpose: "Enter the 4-digit code an Apple TV shows during setup.", used_with: "", flags: &[], examples: &[] },
    Node { path: "device rename", usage: "bridge device rename <device_id> <name>", who: "Carbon (owner)", purpose: "Rename a device.", used_with: "", flags: &[], examples: &[] },
    Node { path: "device visibility", usage: "bridge device visibility <device_id> team|personal", who: "Carbon (owner)", purpose: "Choose whether other Carbons in the team can see the device exists.", used_with: "", flags: &[], examples: &[] },
    Node { path: "device ttl", usage: "bridge device ttl <device_id> <days>", who: "Carbon (owner)", purpose: "Set how long the device stays paired without activity: 1–30 days.", used_with: "", flags: &[], examples: &["bridge device ttl 7c1e09ab 30"] },
    Node { path: "device stop", usage: "bridge device stop <device_id>", who: "Carbon (owner)", purpose: "Stop the Silicon using the device right now.", used_with: "", flags: &[], examples: &[] },
    Node { path: "device rm", usage: "bridge device rm <device_id> --yes", who: "Carbon (owner)",
        purpose: "Remove the device: ends any session and every Silicon's access, and unpairs it. Devices paired through it go too.", used_with: "Without --yes it only says what would happen.", flags: &[("--yes", "confirm")], examples: &[] },
    Node { path: "device access", usage: "bridge device access ls <device_id> | grant <device_id> <silicon_id>... | revoke <device_id> <silicon_id>...", who: "Carbon (owner)",
        purpose: "See, give and take away which Silicons can use a device. Taking access away ends that Silicon's running session at once.", used_with: "", flags: &[], examples: &["bridge device access grant 7c1e09ab si:chef"] },
    Node { path: "device activity", usage: "bridge device activity <device_id> [--silicon <id>] [--session <id>] [--since <time>] [--until <time>] [--limit <n>]", who: "Carbon (owner)",
        purpose: "Every action on the device, which Silicon did it, and when. Typed text is redacted.", used_with: "",
        flags: &[("--since/--until", "RFC 3339, or relative like 2h, 3d")], examples: &["bridge device activity 7c1e09ab --since 2h"] },
    Node { path: "device requests", usage: "bridge device requests <device_id>", who: "Carbon (owner)", purpose: "Requests Silicons sent each other for this device, with their reasons.", used_with: "", flags: &[], examples: &[] },
    Node { path: "session", usage: "bridge session <new|connect|disconnect|status|ls|end>", who: "Silicon",
        purpose: "Use a device. Only one Silicon at a time; a session ends on its own after 5 minutes without a command.",
        used_with: "`bridge session new <device_id> --connect`, then device commands like `bridge snapshot -i` and `bridge click @e2`, then `bridge session end`.",
        flags: &[], examples: &["bridge session new 7c1e09ab --connect", "bridge session end"] },
    Node { path: "session new", usage: "bridge session new <device_id> [--connect]", who: "Silicon",
        purpose: "Start using a device. Prints the session id (3+ hexadecimal characters). If another Silicon is using it you get exit 6 and the command to ask for it.",
        used_with: "Add --connect to connect at once.", flags: &[("--connect", "also connect to the new session")], examples: &[] },
    Node { path: "session connect", usage: "bridge session connect <session_id>", who: "Silicon",
        purpose: "Make this the session later commands run in, and load its device's commands into --help.", used_with: "", flags: &[], examples: &["bridge session connect a3f"] },
    Node { path: "session disconnect", usage: "bridge session disconnect", who: "Silicon", purpose: "Forget the connected session locally. It keeps running until ended or idle.", used_with: "", flags: &[], examples: &[] },
    Node { path: "session status", usage: "bridge session status [<session_id>]", who: "Silicon", purpose: "State, device, commands run, and when it ends for inactivity.", used_with: "", flags: &[], examples: &[] },
    Node { path: "session ls", usage: "bridge session ls [--device <device_id>] [--state active|paused|ended]", who: "Carbon or Silicon", purpose: "Silicon: your sessions. Carbon: sessions on your devices.", used_with: "", flags: &[], examples: &[] },
    Node { path: "session end", usage: "bridge session end [<session_id>]", who: "Silicon", purpose: "Finish and free the device for other Silicons. Defaults to the connected session.", used_with: "", flags: &[], examples: &[] },
    Node { path: "takeover", usage: "bridge takeover --reason \"<text>\" | bridge takeover status | bridge takeover release", who: "Silicon",
        purpose: "Hand the device to its Carbon (Face ID, a payment, an admin prompt). Commands are refused until they tap Done or you release. Up to 30 minutes.", used_with: "", flags: &[("--reason", "1–300 characters, shown on the device and website")], examples: &["bridge takeover --reason \"Please approve the Face ID prompt\""] },
    Node { path: "request", usage: "bridge request send <device_id> --reason \"<text>\" | bridge request ls [--sent|--received] [--device <id>]", who: "Silicon",
        purpose: "Ask the Silicon using a device for it. Delivered through Ting with your reason exactly as written (1–300 characters).",
        used_with: "Use after `bridge session new` says the device is in use.", flags: &[], examples: &["bridge request send 7c1e09ab --reason \"Need 2 minutes to read an OTP\""] },
    Node { path: "file", usage: "bridge file ls [--session <id>] [--device <id>] [--kind <kind>] | show <file_id> | get <file_id> [--out <path>] | keep <file_id>", who: "Carbon or Silicon",
        purpose: "Files made in sessions (screenshots, recordings, logs, scripts), stored in Briefcase. They self-destruct after 1 day unless made with --ttl or kept.",
        used_with: "Device commands that make files also take --ttl <1m–30d>, --keep and --out <path>.", flags: &[], examples: &["bridge file keep 6e1f2a9c-..."] },
    Node { path: "report", usage: "bridge report \"<message>\" [--pr <url>]", who: "Carbon or Silicon",
        purpose: "Report a bug to the Bridge team. If you already fixed it, attach the pull request.",
        used_with: "Bridge is open source: reproduce, patch and open a PR at https://github.com/teamofsilicons/silicon-bridge, then report with --pr.", flags: &[("--pr <url>", "link to your pull request")], examples: &[] },
    Node { path: "env", usage: "bridge --test <test_id> env show", who: "anyone (test environments only)",
        purpose: "The test environment's name, state, your test identity, and paired devices out of 5.", used_with: "", flags: &[], examples: &[] },
    Node { path: "version", usage: "bridge version [--json]", who: "anyone", purpose: "CLI, client crate and negotiated API versions.", used_with: "", flags: &[], examples: &[] },
    Node { path: "docs", usage: "bridge docs", who: "anyone", purpose: "Links: repository, docs, website, Rust crate; and where state is kept.", used_with: "", flags: &[], examples: &[] },
];

pub const GLOBAL_FLAGS: &[(&str, &str)] = &[
    ("--json", "one JSON document on stdout: {\"ok\": true, \"data\": ...} or {\"ok\": false, \"error\": {...}}"),
    ("--test <test_id>", "run in a test environment added with `bridge config test add` (printed on stderr at the end)"),
    ("--session <session_id>", "use this session instead of the connected one (also BRIDGE_SESSION)"),
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
    let mut s = format!("bridge {}\n\n{}\n\nUsage:\n  {}\n\nWho: {}\n", n.path, n.purpose, n.usage, n.who);
    if !n.used_with.is_empty() {
        s.push_str(&format!("\nHow it's used:\n  {}\n", n.used_with));
    }
    if !n.flags.is_empty() {
        s.push_str("\nFlags:\n");
        for (f, d) in n.flags {
            s.push_str(&format!("  {f:<24} {d}\n"));
        }
    }
    let children: Vec<&Node> = NODES.iter().filter(|c| c.path.starts_with(&format!("{} ", n.path))).collect();
    if !children.is_empty() {
        s.push_str("\nUnder this:\n");
        for c in children {
            s.push_str(&format!("  bridge {:<22} {}\n", c.path, first_sentence(c.purpose)));
        }
        s.push_str(&format!("\nGo deeper: bridge {} <command> --help\n", n.path));
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
        Origin::AgentDevice => "an agent-device command, run on the session's device through Bridge",
        Origin::Bridge => "a Bridge command (not part of agent-device)",
    };
    Some(format!(
        "bridge {name}\n\n{}.\n\nUsage:\n  bridge {}\n\nThis is {origin}. It needs a session (`bridge session new <device_id> --connect`) and a device with: {}.\n\nFlags for any device command:\n  --session <id>          which session (default: the connected one)\n  --timeout <ms>          deadline, 1000–300000 (default 30000)\n  --json                  full structured result\n  --ttl <1m–30d>          self-destruct for files this command makes (default 1d)\n  --keep                  keep files this command makes permanently\n  --out <path>            also save the files locally\n\nArgument details follow agent-device: {}/tree/main/vendor/agent-device/website/docs/docs/commands.md\n",
        c.summary,
        c.usage,
        caps.join(" or "),
        REPO
    ))
}

/// Top-level help. `connected` narrows the device commands to what the session's device can do.
pub fn render_top(connected: Option<(&str, &str, &str, &[String])>) -> String {
    let mut s = String::from(
        "bridge — let a Silicon use the devices a Carbon has paired, and let the Carbon manage them.\n\n\
Getting started\n\
  honeycomb install 'bridge'          install the CLI\n\
  bridge login <slt>                  sign in with a short-lived token from Silicon IAM\n\
  bridge device ls                    devices you can use (Silicon) or have paired (Carbon)\n\
  bridge device show <device_id>      what a Silicon can do on a device\n\
  bridge session new <device_id> --connect\n\
  bridge snapshot -i                  read the screen; then act: bridge click @e2\n\
  bridge session end                  free the device for other Silicons\n\n\
Commands (bridge <command> --help goes deeper)\n",
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
            s.push_str("\nDevice commands (need a session; `bridge --help` while connected shows only what that device can do)\n");
            for c in COMMANDS {
                s.push_str(&format!("  {:<14} {}\n", c.name, c.summary));
            }
        }
    }
    s.push_str("\nGlobal flags\n");
    for (f, d) in GLOBAL_FLAGS {
        s.push_str(&format!("  {f:<24} {d}\n"));
    }
    s.push_str(&format!(
        "\nState lives in {} (move it with `bridge config home <dir>`).\nDocs {DOCS} · Source {REPO} · Rust crate {CRATE}\nFound a bug? `bridge report \"...\" --pr <link>` — Bridge is open source, patches welcome.\n",
        crate::store::root().display()
    ));
    s
}
