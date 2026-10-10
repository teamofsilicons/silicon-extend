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
/// How to install the CLI. Silicon Apps keeps it up to date from then on.
pub const INSTALL: &str = "silicon-apps install extend";

pub const NODES: &[Node] = &[
    Node {
        path: "login",
        usage: "extend login [--open] [--label <text>] | extend login --slt-stdin | extend login --slt <slt> | extend login <slt>",
        who: "Carbon (a code) or Silicon (a short-lived token)",
        purpose: "Sign in to Extend with Silicon Accounts. Carbons approve a code; Silicons hand over a short-lived token. Extend never asks for a password.",
        used_with: "Carbons: `extend login` prints a link and a code (like MVHB-KQAW); open the link on any device, check it is Extend asking, and approve. The CLI waits (up to 10 minutes) and signs you in. Add --open to open the link here. \
Silicons: `silicon-accounts login --app extend -q | extend login --slt-stdin`. The token works once, for 2 minutes, for Extend only; a used, expired or other-app token is refused with the exact reason (exit 3), so mint a fresh one. `--slt <slt>` and `extend login <slt>` work too, but put the token in the process list. \
The tokens are kept in the state directory (auth.json, readable only by you) and refreshed on their own. A state directory holds one sign-in: signing in as another account signs the previous one out. Check with `extend login status`.",
        flags: &[
            ("--slt-stdin", "Silicons: read the short-lived token from stdin"),
            ("--slt <slt>", "Silicons: the short-lived token (slt_…)"),
            ("--open", "Carbons: open the approval page in the browser"),
            (
                "--label <text>",
                "Carbons: names this sign-in on the approval page (default: extend CLI on <host>)",
            ),
            (
                "--json",
                "one JSON line per event on stdout: device_code (with user_code and verification_uri), slow_down, retrying, then signed_in",
            ),
        ],
        examples: &[
            "extend login",
            "silicon-accounts login --app extend -q | extend login --slt-stdin",
            "extend login --json",
        ],
    },
    Node {
        path: "login status",
        usage: "extend login status [--offline] [--json]",
        who: "anyone",
        purpose: "Show who is signed in, checked with Extend now (refreshing the sign-in if needed).",
        used_with: "Scripts read `authenticated` in the --json output, which always exits 0: {\"authenticated\": false} when signed out. Without --json it exits 1 when signed out. `verified` says whether Extend confirmed the sign-in just now; --offline reads only the saved sign-in, with no network.",
        flags: &[
            ("--offline", "read only the saved sign-in; nothing is sent"),
            (
                "--json",
                "{\"authenticated\": true, \"uuid\", \"id\", \"kind\", \"display_name\", \"custodian\", \"expires_at\", \"refresh_expires_at\", \"verified\", \"method\", ...} or {\"authenticated\": false}",
            ),
        ],
        examples: &["extend login status --json"],
    },
    Node {
        path: "logout",
        usage: "extend logout",
        who: "Carbon or Silicon",
        purpose: "Sign out of Extend and delete the saved sign-in. A Silicon signing out ends its running sessions. A Carbon signing out ends the running sessions of the Silicons they gave access to (on their own pairs only, never another Carbon's).",
        used_with: "Extend revokes the sign-in at Silicon Accounts; if Extend can't be reached, the CLI revokes it there itself. Signed out already, it says so and exits 0. Sign in again with `extend login`.",
        flags: &[],
        examples: &["extend logout"],
    },
    Node {
        path: "accounts",
        usage: "extend accounts [--json]",
        who: "anyone, no sign-in",
        purpose: "Where Extend's accounts come from and how to sign in: Extend's app id in Silicon Accounts, the Silicon Accounts and Extend URLs this CLI uses, and its version.",
        used_with: "Answers with no network and no sign-in, and always exits 0. A Silicon uses app_id to mint a short-lived token: `silicon-accounts login --app extend -q`.",
        flags: &[(
            "--json",
            "{\"app_id\": \"extend\", \"accounts_url\", \"api_url\", \"version\", \"client_id\", \"device_flow\", \"public_client\", \"sign_in\": {\"carbon\", \"silicon\"}, ...}",
        )],
        examples: &["extend accounts --json"],
    },
    Node {
        path: "silicon",
        usage: "extend silicon ls | show <si:id> | renounce <si:id> <device_id>",
        who: "Carbon (custodian)",
        purpose: "The Silicons you look after (you are their custodian in Silicon Accounts) and the ones you gave access to. You see what the Silicons you look after do in Extend and can stop it; you never act as them.",
        used_with: "For a Silicon you look after: `extend silicon show <si:id>` lists every device it can use (whoever gave the access), `extend session ls --silicon <si:id>` its sessions (end one with `extend session end <session_id>`), `extend file ls --silicon <si:id>` its files, `extend request ls --silicon <si:id>` its requests. Give any Silicon access to your device with `extend device access grant <device_id> <si:id>`.",
        flags: &[],
        examples: &["extend silicon ls", "extend silicon show si:chef"],
    },
    Node {
        path: "silicon ls",
        usage: "extend silicon ls [--json]",
        who: "Carbon",
        purpose: "The Silicons you look after, then the ones you gave access to, with how many of your devices each can use.",
        used_with: "Nobody else's Silicons are listed: there is no directory to browse. Name any Silicon by its si: id to give it access.",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "silicon show",
        usage: "extend silicon show <si:id|uuid> [--json]",
        who: "Carbon",
        purpose: "One Silicon you look after or gave access to; for one you look after, every device it can use, whoever gave the access.",
        used_with: "",
        flags: &[],
        examples: &["extend silicon show si:chef"],
    },
    Node {
        path: "silicon renounce",
        usage: "extend silicon renounce <si:id|uuid> <device_id>",
        who: "Carbon (its custodian) or the Silicon itself",
        purpose: "Give up a Silicon's access to a device. Its running session there ends and its open wake requests there are withdrawn; the device's Carbon sees it in the device's activity.",
        used_with: "The Carbon who paired the device can take access away with `extend device access revoke` instead.",
        flags: &[],
        examples: &["extend silicon renounce si:chef 7c1e09ab"],
    },
    Node {
        path: "config",
        usage: "extend config ls | get <key> | set <key> <value> | unset <key> | home <dir> [--use-existing]",
        who: "anyone",
        purpose: "Read and change CLI settings, and where state is kept.",
        used_with: "Settings (each value is checked): api_url and accounts_url (https, or http for localhost, 127.0.0.1 or [::1]; EXTEND_API_URL and ACCOUNTS_URL override them), telemetry on|off, output text|json, screenshot_scale 0.01–1, self_destruct 1m–30d, download_dir <existing directory>, color auto|always|never. \
`extend config home <dir>` moves the saved sign-in, settings and sessions to <dir>/.extend (default $SILICON_HOME/.extend or ~/.extend); if <dir> already holds Extend state it refuses, unless --use-existing switches to that state and leaves this one where it is.",
        flags: &[(
            "--use-existing",
            "config home: switch to the state already in <dir> instead of moving this one there",
        )],
        examples: &["extend config set telemetry off", "extend config home /data/si-chef"],
    },
    Node {
        path: "device",
        usage: "extend device <ls|show|pair|attach|setup|setup-code|rename|banner|ttl|stop|rm|access|activity|requests|wake|wake-requests>",
        who: "Carbon or Silicon",
        purpose: "Find devices (Silicons: the ones you can use) and manage them (Carbons: the ones you paired).",
        used_with: "A Silicon runs `extend device ls`, then `extend device show <device_id>` to see what it can do there, then `extend session new <device_id>`. A device that isn't awake still works for the terminal and Android debugging; for its screen, ask its Carbon with `extend device wake <device_id> --reason \"...\"`. \
A device is private to the Carbon who paired it and the Silicons they give access to (`extend device access grant <device_id> <si:id>`). Several Carbons can pair one device (\"Pair with another Carbon\" in its Extend app); each pair is separate.",
        flags: &[],
        examples: &["extend device ls", "extend device show 7c1e09ab"],
    },
    Node {
        path: "device ls",
        usage: "extend device ls [--online] [--os <os>] [--removed] [--json]",
        who: "Carbon or Silicon",
        purpose: "Silicon: every device you have access to. Carbon: every device you paired.",
        used_with: "Pick a device id from here for `extend device show` or `extend session new`. Every page is read, so the list is complete (it says so if it had to stop early). \
Columns for a Carbon: ID NAME OS ONLINE AWAKE IN USE ACCESS LAST USED DAYS LEFT; NAME says (shared) when another Carbon paired the device too, and IN USE names your Silicon, says \"yes (another Carbon's Silicon)\", or \"a carried device (stop it at the computer)\". \
Columns for a Silicon: ID NAME OS ONLINE AWAKE IN USE; IN USE names the Silicon when it uses the device through the same Carbon's pair and has your custodian, else says \"in use\", then \"asked\" while your wake request is open, and (same device as <id>) when another Carbon's pair of the same device is yours to use too. \
AWAKE is yes, no (screen off, locked, asleep, standby, another account), or — when Extend can't tell (offline, an app older than 1.1, an iPhone or iPad).",
        flags: &[
            ("--online", "only online devices"),
            (
                "--os <os>",
                "android, android_tv, macos, windows, linux, ios, ipados, tvos, samsung_tv, lg_tv",
            ),
            (
                "--removed",
                "Carbons: also your removed devices, whose activity stays readable",
            ),
        ],
        examples: &["extend device ls --online"],
    },
    Node {
        path: "device show",
        usage: "extend device show <device_id> [--json]",
        who: "Carbon or Silicon",
        purpose: "One device, and exactly what a Silicon can do on it right now given its OS and permissions, plus what's missing and why.",
        used_with: "Read before starting a session so you know which commands will work. It says whether the device is awake, and your open wake request. A Carbon also sees which Silicons have access, whether another Carbon paired it too, whether wake requests are on, and every open wake request. On a computer several Carbons paired, only Silicons given access by the Carbon who installed Silicon Extend on it get the terminal; the others see it under Missing, with why.",
        flags: &[],
        examples: &["extend device show 7c1e09ab"],
    },
    Node {
        path: "device pair",
        usage: "extend device pair <pairing_code> --name <name> [--ttl-days <1-30>] [--access <si:id>]...",
        who: "Carbon",
        purpose: "Pair the device showing this code to your account. The code is 6 hexadecimal characters, rotates every 5 minutes and works once.",
        used_with: "The device is private to you and the Silicons you give access to (--access now, or `extend device access grant` later). Then follow the device's own setup with `extend device setup <device_id> --watch`. A device another Carbon already paired shows a code under \"Pair with another Carbon\" in its Extend app (1.1 or later); your pair is separate, with its own name, access and lifetime.",
        flags: &[
            ("--name <name>", "1–64 characters, required"),
            (
                "--ttl-days <n>",
                "days without activity before the pair ends, 1–30, default 14",
            ),
            (
                "--access <si:id>",
                "give a Silicon access now (its si: id or uuid); repeatable",
            ),
        ],
        examples: &["extend device pair 4f9c2a --name \"Saket's Pixel\" --access si:chef"],
    },
    Node {
        path: "device attach",
        usage: "extend device attach <host_device_id> --os ios|ipados|tvos|samsung_tv|lg_tv --name <name> [--address <ip>]",
        who: "Carbon",
        purpose: "Pair an iPhone, iPad, Apple TV or Samsung/LG TV through a paired Mac or computer on the same network.",
        used_with: "Then `extend device setup <device_id> --watch`; an Apple TV also needs `extend device setup-code <device_id> <code>`. Give Silicons access with `extend device access grant`.",
        flags: &[
            ("--os <os>", "ios, ipados, tvos, samsung_tv or lg_tv; required"),
            ("--name <name>", "1–64 characters, required"),
            (
                "--address <ip>",
                "a TV's address on the network, when it isn't found on its own",
            ),
        ],
        examples: &["extend device attach 2e7f00d1 --os ios --name \"Saket's iPhone\""],
    },
    Node {
        path: "device setup",
        usage: "extend device setup <device_id> [--watch] [--retry [--step <key>]]",
        who: "Carbon (owner)",
        purpose: "The device's own setup steps (debugging, permissions) and which are left. --watch follows until complete; --retry runs failed steps again.",
        used_with: "Run after `extend device pair` or `extend device attach`. A failed step says what is wrong and what to do; once that is done, `--retry` runs it again at once and follows it until it finishes or fails. The device (or the computer it pairs through) must be online and run Silicon Extend 1.1 or later; an older app retries from its own screen.",
        flags: &[
            ("--watch", "refresh every 2 s until setup completes"),
            (
                "--retry",
                "run every failed step again now, then follow them (at most once every 5 s)",
            ),
            (
                "--step <key>",
                "with --retry: only this step (its key is in --json, like wireless_debugging)",
            ),
        ],
        examples: &[
            "extend device setup 7c1e09ab --retry",
            "extend device setup 7c1e09ab --retry --step wireless_debugging",
        ],
    },
    Node {
        path: "device setup-code",
        usage: "extend device setup-code <device_id> <code>",
        who: "Carbon (owner)",
        purpose: "Enter the 4-digit code an Apple TV shows during setup.",
        used_with: "`extend device setup <device_id>` says when the Apple TV is showing a code.",
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
        path: "device banner",
        usage: "extend device banner <device_id> on|off",
        who: "Carbon (owner)",
        purpose: "Show or hide the device's in-use banner, notification and icon change. The choice applies to every Carbon's pair of this device.",
        used_with: "On: the banner shows for 10 seconds per session. Off: the app and website still show who is using the device, with Stop. Requests waiting for a Carbon still appear; Android's quiet running notification and Apple's Automation Running banner remain.",
        flags: &[],
        examples: &["extend device banner 7c1e09ab off"],
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
        purpose: "Stop the Silicon using the device right now, whichever Carbon gave it access: every Carbon who paired a device can stop it.",
        used_with: "See who is using it with `extend device show <device_id>`. A Silicon another Carbon gave access to isn't named. On a computer it also stops the devices it carries that you paired; a carried device only another Carbon paired is stopped from the computer's Silicon Extend app (exit 6 says so).",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "device rm",
        usage: "extend device rm <device_id> --yes",
        who: "Carbon (owner)",
        purpose: "Remove the device: ends any session and every Silicon's access, and unpairs it. Devices paired through it go too.",
        used_with: "Without --yes it only says what would happen. The activity log stays readable: `extend device ls --removed`, then `extend device activity <device_id>`.",
        flags: &[("--yes", "confirm")],
        examples: &[],
    },
    Node {
        path: "device access",
        usage: "extend device access ls <device_id> | grant <device_id> <si:id>... | revoke <device_id> <si:id>...",
        who: "Carbon (owner)",
        purpose: "See, give and take away which Silicons can use a device. Taking access away ends that Silicon's running session at once.",
        used_with: "Any Silicon, named by its si: id or uuid; it doesn't have to accept, and it and its custodian see the grant. `extend silicon ls` lists the Silicons you look after and the ones you gave access to. After a grant it warns when Ting doesn't know Extend's notification types (`extend ting status`).",
        flags: &[],
        examples: &[
            "extend device access grant 7c1e09ab si:chef",
            "extend device access revoke 7c1e09ab si:chef",
        ],
    },
    Node {
        path: "device activity",
        usage: "extend device activity <device_id> [--silicon <si:id>] [--session <id>] [--since <time>] [--until <time>] [--limit <n>]",
        who: "Carbon (owner)",
        purpose: "Every action on the device, which Silicon did it, and when. Typed text is redacted.",
        used_with: "Only your own side: actions through your pair of the device. What other Carbons' Silicons do there is in their logs.",
        flags: &[
            ("--silicon <si:id>", "only this Silicon's actions"),
            ("--session <id>", "only this session's actions"),
            ("--since/--until", "RFC 3339, or relative like 30m, 2h, 3d"),
            ("--limit <n>", "at most n entries, 1–100 (default 50)"),
        ],
        examples: &["extend device activity 7c1e09ab --since 2h"],
    },
    Node {
        path: "device requests",
        usage: "extend device requests <device_id>",
        who: "Carbon (owner)",
        purpose: "Requests Silicons sent for this device, with their reasons.",
        used_with: "It also lists requests sent to you (TO says you): a Silicon asked for the device while a Silicon you gave access to was using it, and the two aren't on the same side (your pair, the same custodian). FROM names the Silicon that asked. Stop your Silicon with `extend device stop <device_id>` to let it in.",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "device wake",
        usage: "extend device wake <device_id> --reason \"<text>\" | extend device wake <device_id> --cancel",
        who: "Silicon",
        purpose: "Ask the Carbon who gave you access to wake a device that isn't awake. Extend never wakes a device itself.",
        used_with: "`extend device ls` and `extend device show` say whether a device is awake. The device shows your name and reason where it can (a phone's lock screen, a computer's notifications), and your Carbon gets it through Ting when notifications are on. When the device wakes you get a Ting (or the Carbon says so), then run `extend session new <device_id>`. \
Asking again after 5 minutes refreshes the request (sooner: exit 12); a request expires 30 minutes after the last ask. --cancel withdraws it. Exit 6 when the device is already awake, another Silicon is using it, or its Carbon turned wake requests off; 4 without access; 5 for an unknown device. The terminal and Android debugging keep working while a device isn't awake: you don't need to wake it for them.",
        flags: &[
            (
                "--reason <text>",
                "1–300 characters, shown to the Carbon exactly as written",
            ),
            ("--cancel", "withdraw your open request instead"),
        ],
        examples: &[
            "extend device wake 0d44e1f2 --reason \"Need the TV on to check the new menu\"",
            "extend device wake 0d44e1f2 --cancel",
        ],
    },
    Node {
        path: "device wake-requests",
        usage: "extend device wake-requests ls <device_id> [--open] | answer <device_id> woken|declined [--wake-id <id>]... | mute <device_id> [--silicon <si:id>] | unmute <device_id> [--silicon <si:id>]",
        who: "Carbon (owner)",
        purpose: "See and answer requests from Silicons to wake your device, or turn them off.",
        used_with: "`answer woken` says the device is awake now: it ends every open request on it, and each Silicon that asked gets a Ting. Use it where Extend can't tell when a device wakes (iPhone, iPad, an app older than 1.1). `answer declined` ends the requests on your pair only (or the --wake-id ones), and each Silicon gets a Ting. \
`mute` turns wake requests off for the device, or for one Silicon, and withdraws the open ones; `unmute` turns them on again. A device sounds at most once every 15 minutes, and you get at most one wake Ting per device every 15 minutes and 6 an hour; later asks wait for the hour's window.",
        flags: &[
            ("--open", "ls: only open requests"),
            ("--wake-id <id>", "answer declined: only this request; repeatable"),
            ("--silicon <si:id>", "mute/unmute: only this Silicon's requests"),
        ],
        examples: &[
            "extend device wake-requests ls 0d44e1f2 --open",
            "extend device wake-requests answer 0d44e1f2 woken",
            "extend device wake-requests mute 0d44e1f2 --silicon si:chef",
        ],
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
        used_with: "Add --connect to connect at once. On a device that isn't awake the session still starts, with a note: the terminal and Android debugging work, and commands that need its screen fail until its Carbon wakes it (`extend device wake`).",
        flags: &[("--connect", "also connect to the new session")],
        examples: &[],
    },
    Node {
        path: "session connect",
        usage: "extend session connect <session_id>",
        who: "Silicon",
        purpose: "Make this the session later commands run in, and load its device's commands into --help.",
        used_with: "The command list is refreshed on every device command and `extend session status`, and forgotten when the session ends.",
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
        used_with: "Also refreshes what `extend --help` lists for the connected device.",
        flags: &[],
        examples: &[],
    },
    Node {
        path: "session ls",
        usage: "extend session ls [--device <device_id>] [--state active|paused|ended] [--silicon <si:id>]",
        who: "Carbon or Silicon",
        purpose: "Silicon: your sessions. Carbon: sessions on your devices, or with --silicon those of a Silicon you look after.",
        used_with: "A custodian ends one of its Silicon's sessions with `extend session end <session_id>`.",
        flags: &[
            ("--device <device_id>", "only sessions on this device"),
            ("--state <state>", "active, paused or ended"),
            ("--silicon <si:id>", "Carbons: the sessions of a Silicon you look after"),
        ],
        examples: &["extend session ls --silicon si:chef --state active"],
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
        usage: "extend request send <device_id> --reason \"<text>\" | extend request ls [--sent|--received] [--device <id>] [--silicon <si:id>]",
        who: "Silicon (send), Carbon or Silicon (ls)",
        purpose: "Ask for a device another Silicon is using, with your reason exactly as written (1–300 characters).",
        used_with: "Use after `extend session new` says the device is in use. When the Silicon using it was given access through the same Carbon's pair and has your custodian, the request goes to it, and you see which Silicon it is. Otherwise it goes to the Carbon who gave that Silicon access, who sees your id and reason and can stop the session; you only see that it is in use. Requests are delivered through Ting when notifications are on, and always show on the website and in `extend request ls`.",
        flags: &[
            ("--reason <text>", "send: 1–300 characters, required"),
            ("--sent / --received", "ls: only requests you sent, or received"),
            ("--device <id>", "ls: only requests for this device"),
            (
                "--silicon <si:id>",
                "ls, Carbons: the requests of a Silicon you look after",
            ),
        ],
        examples: &["extend request send 7c1e09ab --reason \"Need 2 minutes to read an OTP\""],
    },
    Node {
        path: "ting",
        usage: "extend ting status | extend ting on",
        who: "Carbon or Silicon",
        purpose: "Whether Extend's notifications reach you through Ting, and which of Extend's notification types Ting doesn't know yet.",
        used_with: "Ting delivers wake requests, answers to them, and requests for devices in use. Status is on, off (you turned Extend's notifications off in Ting) or pending (not registered yet). When this Extend server sends no notifications through Ting, it says so: requests and wake requests still show on the website, in the CLI and on the device.",
        flags: &[],
        examples: &["extend ting status", "extend ting on"],
    },
    Node {
        path: "ting status",
        usage: "extend ting status [--json]",
        who: "Carbon or Silicon",
        purpose: "Whether Extend's notifications reach you through Ting, and the notification types Ting doesn't know yet.",
        used_with: "",
        flags: &[],
        examples: &["extend ting status --json"],
    },
    Node {
        path: "ting on",
        usage: "extend ting on",
        who: "Carbon or Silicon",
        purpose: "Turn Extend's notifications on again: registers you with Ting and sends the notifications that waited.",
        used_with: "Run it after turning Extend's notifications off in Ting, or when `extend ting status` says pending.",
        flags: &[],
        examples: &["extend ting on"],
    },
    Node {
        path: "file",
        usage: "extend file ls [--session <id>] [--device <id>] [--kind <kind>] [--silicon <si:id>] | show <file_id> | get <file_id> [--out <path>] | keep <file_id>",
        who: "Carbon or Silicon",
        purpose: "Files made in sessions (screenshots, recordings, logs, scripts), stored in Briefcase. They self-destruct after 1 day unless made with --ttl or kept.",
        used_with: "Device commands that make files also take --ttl <1m–30d>, --keep and --out <path>. `extend file get` downloads through Extend, which reads Briefcase for you, so it works for the Silicon that made the file, the Carbon whose device made it, and the Silicon's custodian.",
        flags: &[
            ("--session <id>", "ls: only files from this session"),
            ("--device <id>", "ls: only files from this device"),
            ("--kind <kind>", "ls: screenshot, recording, log, replay_script or diff"),
            (
                "--silicon <si:id>",
                "ls, Carbons: only the files of a Silicon you look after",
            ),
            (
                "--out <path>",
                "get: a file or directory to save to (default: download_dir, else here)",
            ),
        ],
        examples: &[
            "extend file get 6e1f2a9c-... --out ./shot.png",
            "extend file keep 6e1f2a9c-...",
        ],
    },
    Node {
        path: "report",
        usage: "extend report \"<message>\" [--pr <url>]",
        who: "Carbon or Silicon",
        purpose: "Report a bug to Extend's maintainers. If you already fixed it, attach the pull request.",
        used_with: "Extend is open source: reproduce, patch and open a PR at https://github.com/teamofsilicons/silicon-extend, then report with --pr.",
        flags: &[("--pr <url>", "link to your pull request")],
        examples: &[],
    },
    Node {
        path: "version",
        usage: "extend version [--json]",
        who: "anyone",
        purpose: "CLI, client crate and negotiated API versions, and whether this CLI is current, deprecated or sunset.",
        used_with: "Reads Extend's compatibility matrix (GET /api/v2/contracts). Status is current, deprecated (update before the API is retired), sunset (update now), unsupported (this CLI is outside the versions the API works with) or unknown (Extend couldn't be asked). Silicon Apps keeps extend up to date on its own; `silicon-apps update extend` checks now.",
        flags: &[(
            "--json",
            "{\"cli\", \"client_crate\", \"api_version\", \"api_url\", \"service_version\", \"status\", \"api_state\", \"cli_range\", \"message\", ...}",
        )],
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
    Node {
        path: "help",
        usage: "extend help [<command>...]",
        who: "anyone",
        purpose: "The same as `extend [<command>...] --help`.",
        used_with: "",
        flags: &[],
        examples: &["extend help device ls"],
    },
];

pub const GLOBAL_FLAGS: &[(&str, &str)] = &[
    (
        "--json",
        "one JSON document: on success the data itself on stdout, on failure {\"error\": {code, message, hint, request_id, docs_url, details, exit_code}} on stderr",
    ),
    (
        "--session <session_id>",
        "use this session instead of the connected one (also EXTEND_SESSION)",
    ),
    ("--timeout <ms>", "device command deadline, 1000–300000 (default 30000)"),
    ("-h, --help", "help for this part of the tree"),
    ("-V, --version", "versions, and whether this CLI is current"),
    (
        "-v, --verbose",
        "on stderr: each call to Extend with its time and outcome, and the request id of a failed one",
    ),
];

pub fn find(path: &str) -> Option<&'static Node> {
    NODES.iter().find(|n| n.path == path)
}

/// The usage line for `extend <path>`, or its parent's.
pub fn usage_of(path: &str) -> &'static str {
    find(path)
        .or_else(|| path.rsplit_once(' ').and_then(|(parent, _)| find(parent)))
        .or_else(|| find(path.split(' ').next().unwrap_or(path)))
        .map_or("extend --help", |n| n.usage)
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
    s.push_str("\nGlobal flags work here too; see `extend --help`.\n");
    s
}

fn first_sentence(s: &str) -> &str {
    s.split(". ").next().unwrap_or(s).trim_end_matches('.')
}

/// The session this CLI is connected to, as last read from Extend.
pub struct Connected<'a> {
    pub session_id: &'a str,
    pub device: &'a str,
    pub os: &'a str,
    pub commands: &'a [String],
    pub missing: &'a [crate::store::MissingNote],
    /// Unix seconds.
    pub refreshed_at: i64,
}

impl Connected<'_> {
    fn when(&self) -> String {
        if self.refreshed_at <= 0 {
            return "when you connected".into();
        }
        time::OffsetDateTime::from_unix_timestamp(self.refreshed_at)
            .ok()
            .and_then(|t| {
                t.format(&time::macros::format_description!("[hour]:[minute]:[second]Z"))
                    .ok()
            })
            .map_or_else(|| "when you connected".into(), |t| format!("at {t}"))
    }
}

/// Why the connected device can't run `c`, or `None` when it can.
fn not_available(c: &CommandSpec, on: &Connected) -> Option<String> {
    if on.commands.iter().any(|x| x == c.name) {
        return None;
    }
    let caps = help_capabilities(c, Some(on));
    let reasons: Vec<String> = on
        .missing
        .iter()
        .filter(|m| caps.contains(&m.capability.as_str()))
        .map(|m| format!("{}: {}", m.capability, m.reason))
        .collect();
    Some(format!(
        "Not available on {} ({}) in session {}: it needs {}, which the device {} ({}).{}",
        on.device,
        on.os,
        on.session_id,
        caps.join(" or "),
        if on.commands.is_empty() {
            "didn't report"
        } else {
            "doesn't have"
        },
        on.when(),
        if reasons.is_empty() {
            String::new()
        } else {
            format!(" {}.", reasons.join("; ").trim_end_matches('.'))
        }
    ))
}

fn help_capabilities(c: &CommandSpec, connected: Option<&Connected>) -> Vec<&'static str> {
    let required = if connected.is_some_and(|on| on.os == "android_tv") {
        c.any_of_for(extend_protocol::DeviceOs::AndroidTv)
    } else {
        c.any_of
    };
    required.iter().map(|x| x.as_str()).collect()
}

pub fn render_device_command(name: &str, connected: Option<&Connected>) -> Option<String> {
    let c = COMMANDS.iter().find(|c| c.name == name)?;
    let caps = help_capabilities(c, connected);
    let origin = match c.origin {
        Origin::AgentDevice => "a command of the device engine, run on the session's device through Extend",
        Origin::Extend => "a command Extend adds around the device engine",
    };
    let placement = match name {
        "adb" => ADB_ARGUMENTS,
        "install" | "reinstall" => INSTALL_ARGUMENTS,
        _ => {
            "They may come before or after the command's arguments. `--` ends Extend's flags: everything after it goes to the device as written, even `-h` or `--json`.\n"
        }
    };
    let more = match (name, c.origin) {
        ("click", _) => format!("\nOn Android TV, element clicks need the Extend app 1.1 or newer and Accessibility. They use the element's click action, then remote selection or Android debugging where available; they do not require a mouse or touch screen. Older TV apps can use `find <text> click`. Coordinate, repeated and held clicks still depend on Android accepting gesture injection.\nArgument details: {DOCS}/cli\n"),
        ("display", _) => "\nFor --image and --video, pass a local file, a public media URL, or file:<file_id> (the bare UUID and the stored Extend/Briefcase link also work). Extend reads stored files as you: only unexpired files you may read are allowed. Stored and local files share the command's 8-file, 8-MiB attachment limit.\nExample: extend display show --image file:<file_id>\n".into(),
        (_, Origin::AgentDevice) => format!("\nArgument details: {DOCS}/cli\n"),
        (_, Origin::Extend) => String::new(),
    };
    let note = connected
        .and_then(|on| not_available(c, on))
        .map(|n| {
            format!("\n{n}\n`extend --help` lists what works there; `extend session status` refreshes that list.\n")
        })
        .unwrap_or_default();
    Some(format!(
        "extend {name}\n\n{}.\n{note}\nUsage:\n  extend {}\n\nThis is {origin}. It needs a session (`extend session new <device_id> --connect`) and a device with: {}.\n\nFlags for any device command:\n  --session <id>          which session (default: the connected one)\n  --timeout <ms>          deadline, 1000–300000 (default 30000)\n  --json                  full structured result\n  --ttl <1m–30d>          self-destruct for files this command makes (default 1d)\n  --keep                  keep files this command makes permanently\n  --out <path>            also save the files locally (their Briefcase links are printed first)\n\n{placement}{more}",
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

/// How `extend adb` reads its arguments (see `VERBATIM_COMMANDS` in args.rs).
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
pub fn render_top(connected: Option<&Connected>) -> String {
    let mut s = String::from(
        "extend — let a Silicon use the devices a Carbon has paired, and let the Carbon manage them.\n\n\
Getting started\n\
  silicon-apps install extend         install the CLI (Silicon Apps keeps it up to date)\n\
  extend login                        Carbons: sign in with Silicon Accounts by approving a code\n\
  silicon-accounts login --app extend -q | extend login --slt-stdin\n\
                                      Silicons: sign in with a short-lived token\n\
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
        Some(on) => {
            s.push_str(&format!(
                "\nDevice commands — connected to session {} on {} ({}); only commands that work there are shown (as the device reported {}; `extend session status` refreshes this)\n",
                on.session_id,
                on.device,
                on.os,
                on.when()
            ));
            let mut any = false;
            for c in COMMANDS.iter().filter(|c| on.commands.iter().any(|x| x == c.name)) {
                any = true;
                s.push_str(&format!("  {:<14} {}\n", c.name, c.summary));
            }
            if !any {
                s.push_str("  (none right now: the device reported nothing it can do, usually because it is offline or still being set up. `extend session status` checks again.)\n");
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
    fn tv_click_help_uses_element_requirements_without_offering_gestures() {
        let commands = vec!["snapshot".to_owned(), "click".to_owned()];
        let on = Connected {
            session_id: "a3f",
            device: "TV",
            os: "android_tv",
            commands: &commands,
            missing: &[],
            refreshed_at: 0,
        };
        let help = render_device_command("click", Some(&on)).unwrap();
        assert!(help.contains("a device with: screen.read."), "{help}");
        assert!(!help.contains("Not available"));
        assert!(help.contains("app 1.1 or newer"));
        assert!(help.contains("Older TV apps can use `find <text> click`"));
        assert!(
            render_device_command("hover", Some(&on))
                .unwrap()
                .contains("Not available")
        );
        assert!(
            render_device_command("click", None)
                .unwrap()
                .contains("input.pointer or input.touch")
        );
    }

    #[test]
    fn install_help_matches_the_contract() {
        // The Extend 4 review copy of understanding/cli.yaml (the original changes only with a Carbon).
        let contract = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/migration/contracts/cli.yaml"),
        )
        .unwrap();
        for name in ["install", "reinstall"] {
            let help = render_device_command(name, None).unwrap();
            let usage_line = help.lines().skip_while(|l| *l != "Usage:").nth(1).unwrap();
            assert!(
                !usage_line.contains("file_id"),
                "`extend {name} --help` offers a file id: {usage_line}"
            );
            let line = format!("extend {}", usage(COMMANDS.iter().find(|c| c.name == name).unwrap()));
            assert!(help.contains(&format!("Usage:\n  {line}\n")), "{help}");
            assert!(
                contract.contains(&line),
                "docs/migration/contracts/cli.yaml has no `{line}`"
            );
            assert!(
                help.contains("at most\n8 MiB") && help.contains("extend file get <file_id> --out ./app.apk"),
                "{help}"
            );
        }
    }

    /// Every flag an Extend command accepts is documented in its help node.
    #[test]
    fn every_accepted_flag_is_in_help() {
        // `iam` is the hidden alias of `accounts` (no flags of its own), kept for the Silicon runtime.
        for spec in crate::args::SPECS.iter().filter(|s| s.path != "iam") {
            let node = find(spec.path)
                .or_else(|| spec.path.rsplit_once(' ').and_then(|(p, _)| find(p)))
                .or_else(|| find(spec.path.split(' ').next().unwrap()))
                .unwrap_or_else(|| panic!("no help for `extend {}`", spec.path));
            for (flag, _) in spec.flags {
                let documented = node.usage.contains(flag) || node.flags.iter().any(|(f, _)| f.contains(flag));
                assert!(
                    documented,
                    "`extend {}` accepts {flag}, but `extend {} --help` doesn't mention it",
                    spec.path, node.path
                );
            }
        }
    }

    #[test]
    fn help_marks_commands_the_connected_device_cannot_run() {
        let commands = vec!["snapshot".to_owned(), "terminal".to_owned()];
        let missing = vec![crate::store::MissingNote {
            capability: "input.remote".into(),
            reason: "Only TVs have a remote.".into(),
        }];
        let on = Connected {
            session_id: "a3f",
            device: "CLI box",
            os: "linux",
            commands: &commands,
            missing: &missing,
            refreshed_at: 0,
        };
        let tv = render_device_command("tv-remote", Some(&on)).unwrap();
        assert!(
            tv.contains(
                "Not available on CLI box (linux) in session a3f: it needs input.remote, which the device doesn't have (when you connected). input.remote: Only TVs have a remote."
            ),
            "{tv}"
        );
        let snap = render_device_command("snapshot", Some(&on)).unwrap();
        assert!(!snap.contains("Not available"), "{snap}");
        assert!(
            !render_device_command("tv-remote", None)
                .unwrap()
                .contains("Not available")
        );

        let empty: Vec<String> = vec![];
        let offline = Connected { commands: &empty, ..on };
        assert!(render_top(Some(&offline)).contains("(none right now"));
    }
}
