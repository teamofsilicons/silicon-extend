# The `extend` CLI

The CLI is the primary way to use Extend, for Carbons and Silicons alike. The website is a subset of
it. The full contract — every command, what it takes, what it prints, every error and exit code — is
[`docs/migration/contracts/cli.yaml`](migration/contracts/cli.yaml) (the Extend 4 review copy of
`understanding/cli.yaml`). `extend --help` carries the same documentation, as a tree: every node says
what it's for, how it's used with other commands, and what is under it.

## Install and sign in

```sh
silicon-apps install extend          # Silicon Apps keeps it up to date; `silicon-apps update extend` checks now
extend login                         # Carbons: open the link it prints, check the code, approve
silicon-accounts login --app extend -q | extend login --slt-stdin    # Silicons: a short-lived token
extend login status --json           # {"authenticated": true, "uuid", "id", "kind", ...}; signed out: {"authenticated": false}
extend accounts --json               # app_id "extend", the Silicon Accounts and Extend URLs; no network, always exit 0
extend logout
```

Everyone signs in with Silicon Accounts, with a personal account. Extend never asks for a password.

- **Carbons** approve a code: `extend login` prints a link and a code; open the link on any device,
  check it is Extend asking, and approve. The CLI waits (up to 10 minutes). `--open` opens the link
  here; `--label <text>` names this sign-in on the approval page; `--json` prints one JSON line per
  event (`device_code`, `slow_down`, `retrying`, then `signed_in`).
- **Silicons** hand over a short-lived token: single use, 2 minutes, for Extend only. Pass it on
  stdin (`--slt-stdin`) so it stays out of the process list; `--slt <token>` and `extend login
  <token>` work too. A refused token says exactly why (already used, expired, minted for another app,
  unknown; exit 3, `details.reason`), so mint a fresh one.
- Before a token is spent or a code shown, the CLI checks that the state directory can hold the
  sign-in and that the Extend service trusts the same Silicon Accounts (`accounts_mismatch`
  otherwise).
- The sign-in is kept in `$SILICON_HOME/.extend/auth.json` (or `~/.extend`), mode 0600, written
  atomically. The access token lasts 30 minutes and is refreshed on its own when less than a minute
  is left, one process at a time under `refresh.lock` (refresh tokens rotate, and presenting a used
  one would end the sign-in). A refused refresh means the sign-in is over: the file is deleted and
  the command says to sign in again.
- A state directory holds one sign-in. Signing in as another account signs the previous one out;
  use another `SILICON_HOME` (or `extend config home <dir>`) for each account.
- `extend logout` asks Extend to sign out (ending a Silicon's running sessions, or the sessions of
  the Silicons a Carbon gave access to); when Extend can't be reached, the CLI revokes the sign-in at
  Silicon Accounts itself. Signed out already, it says so and exits 0.
- `extend login status --json` always exits 0; without `--json` it exits 1 when signed out.
  `verified` says whether Extend confirmed the sign-in just now; `--offline` reads only the file.

`ACCOUNTS_URL` (default `https://accounts.teamofsilicons.com`) and `EXTEND_API_URL` (default
`https://backend.extend.teamofsilicons.com`) choose where to sign in and which Extend to use; plain
http is accepted only for this machine. A sign-in is used only for the Extend and Silicon Accounts it
was made for. `extend version` says whether this CLI is current, deprecated or sunset, from Extend's
compatibility matrix.

## For a Silicon

```sh
extend device ls                          # devices you have access to, online, and who is using them
extend device show 7c1e09ab               # what works there now, and what's missing and why
extend session new 7c1e09ab --connect     # start, connect; prints the session id
extend --help                             # now lists only commands that work on that device
extend snapshot -i                        # @refs for the interactive elements
extend snapshot --raw                     # entire accessibility tree exposed by the device
extend click @e2                          # act; refs stay valid until the next snapshot
extend fill @e3 "hello"                   # typed text is redacted in the activity log
extend screenshot --ttl 7d --out shot.png # stored in Briefcase; self-destructs in 7 days; also downloaded here
extend terminal run "ls ~/Downloads"      # computers only
extend tv-remote press select             # TVs only
extend takeover --reason "Approve Face ID" # hand the device to its Carbon; commands wait until Done
extend session end                        # free the device (it also ends after 5 idle minutes)
```

Successful text snapshots without `--raw` end with: "Run --raw to get the entire accessibility
tree." Use `extend snapshot --raw` to inspect nodes omitted by the normal or interactive view.
The tree contains what the device exposes; it cannot reveal controls the app or Android withholds.
Depth and scope options still limit the output when supplied. JSON output has no footer.

On Android, a visible control can be absent even from the raw tree. A keyboard can also be visible
while its focused field is unavailable to accessibility. With Android debugging connected, `type`
can send supported text to that field and explicitly reports that readback is unavailable;
without debugging it reports `text_input_unavailable` and sends no text. Inspect a fresh screenshot
before retrying. Plain coordinate taps use Extend's existing Android debugging connection when
connected, otherwise accessibility gestures. Debugging must be connected inside the Extend app; a computer's USB ADB
connection is separate. See [Android input limitations](../apps/android/README.md#controls-hidden-from-accessibility).

When another Silicon is using the device, `extend session new` exits 6. It tells you who and since
when only when that Silicon is on your side (given access through the same Carbon's pair, and
looked after by your custodian); otherwise it just says the device is in use. Ask for it with
`extend request send <device_id> --reason "..."` (1–300 characters, kept exactly as written). On your
side it goes to the Silicon using the device; otherwise to the Carbon who gave that Silicon access,
who sees your id and reason and can stop the session ("Sent to the Carbon who gave access to the
Silicon using it"). Requests are delivered through Ting when it is on, and always show on the website
and in `extend request ls`.

### A device that isn't awake

`extend device ls` and `extend device show` say whether a device is awake: yes; no, and why (screen
off, locked, asleep, standby, another account); or "—" when Extend can't tell (offline, an app older
than 1.1, or an iPhone or iPad). Nothing waits for it: sessions start, and the terminal and Android
debugging work, while a device isn't awake. A command that needs the screen fails with the device's
own error and the wake hint. Extend never wakes a device; ask its Carbon:

```sh
extend device wake 0d44e1f2 --reason "I need the TV on to check the order screen"
extend device wake 0d44e1f2 --cancel     # withdraw it
extend device wake-requests ls 0d44e1f2   # your request, and whether the device showed it
```

The device shows your name and reason where it can (a phone's lock screen, a computer's
notifications), and the Carbon gets it through Ting when notifications are on. When the device is
turned on or unlocked (or its Carbon says "It's awake"), you get a Ting with the command to start.
Ask again only after 5 minutes (that refreshes it); a request expires 30 minutes after your last ask.

Files a command makes are printed with their Briefcase links. `--out` (and `extend file get
<file_id>`) downloads them through Extend, which reads them from Briefcase as you, after printing the
links. A file Extend couldn't store or share is a `warning: …` line on stderr saying why and what to
do; the command's exit code doesn't change. If the session ends while a command runs (the Carbon
pressed Stop, access was removed, the device was removed), the command answers at once with exit 6,
says the command may have run, and the CLI disconnects the session.

## Arguments, `--` and local files

- **Global flags** (`--json`, `--timeout`, `--session`, `-h`, `-V`, `-v`) work anywhere on a command
  line, except as below.
- **`extend adb` is verbatim.** From the first `adb` argument on, every token reaches the device
  exactly as typed, including `-h`, `-v`, `--json`, `--timeout` and `--out`. Put Extend's own flags
  before that argument: `extend --json adb shell df -h`, `extend adb --timeout 60000 shell sleep 40`,
  `extend adb --out shot.png exec-out screencap -p`. A `--` straight after `adb` ends Extend's flags
  and is not sent. `extend adb -h` with nothing after it is Extend's help for `adb`.
- **`adb pull`** sends only `pull <device path>`; the local path is a second path
  (`extend adb pull /sdcard/x ./x`) or `--out <path>`, anywhere. Two local paths are refused.
- **`--` elsewhere.** For other device commands `--` stops Extend reading `--ttl`, `--keep` and
  `--out` and is passed on, so the device reads what follows as text (`extend type -- -v`). For
  Extend's own commands (`device`, `session`, `config`, …) everything after `--` is positional.
- **Local files** (replay and test scripts, display media, APKs, `adb push` sources) travel with the
  command: at most 8 files and 8 MiB in total, checked before anything is read or sent.
  For stored Extend images/videos, use `extend display show --image file:<file_id>` or
  `--video file:<file_id>`. A bare file UUID, the stored Briefcase link, or an Extend file-content
  URL also works. Extend reads it as the requesting Silicon, checks ownership and the self-destruct
  time, then forwards an attachment. It must fit the same combined 8-file/8-MiB limit. Public media
  URLs continue to load directly on the device. `file:` explicitly selects a stored file; a bare
  value matching a local file is uploaded by the CLI instead.
  `install`, `reinstall`, `adb install` and `adb push` take a local regular file only; a Briefcase
  file id, a link, a directory or an `.aab` is refused with the command to run instead (for example
  `extend file get <file_id> --out ./app.apk`). For a larger APK, push it in parts and install it
  on the device; `extend adb --help` shows the recipe.

## For a Carbon

```sh
extend device pair 4f9c2a --name "Saket's Pixel" --access si:chef   # code shown on the device
extend device setup 7c1e09ab --watch                                # the device's own setup steps
extend device attach 2e7f00d1 --os ios --name "Saket's iPhone"      # iPhone/iPad/Apple TV/TVs, through a paired computer
extend device access grant 7c1e09ab si:sous                         # any Silicon, by si: id or uuid
extend device access revoke 7c1e09ab si:sous
extend device ttl 7c1e09ab 30                                       # stays paired 30 days without activity
extend device activity 7c1e09ab --since 2h                          # every action, who, when
extend device stop 7c1e09ab                                         # stop the Silicon using it now
extend device rm 7c1e09ab --yes
extend device ls --removed                                          # also removed devices; their activity stays readable
extend device setup 51ab93c0 --retry                                # run failed setup steps again, then follow them
extend device wake-requests ls 0d44e1f2 --open                      # who asked you to wake it, and why
extend device wake-requests answer 0d44e1f2 woken                   # "It's awake": answers every request on the device
extend device wake-requests mute 0d44e1f2 --silicon si:chef         # no more wake requests from si:chef (unmute to undo)
extend ting status                                                  # do Extend's notifications reach you through Ting?
```

- **Your devices are yours.** A device is private to the Carbon who paired it and the Silicons they
  give access to. A grant is your decision: the Silicon doesn't have to accept it, and it and its
  custodian see it.
- **Several Carbons, one device.** "Pair with another Carbon" in a device's Extend app shows a code;
  `extend device pair <code> --name <name>` gives you your own pair of it (your own id, name, access
  and lifetime). Any Carbon who paired it can stop the Silicon using it; you see only your own
  Silicons, and another Carbon's as "in use". On a computer several Carbons paired, only Silicons
  given access by the Carbon who installed Silicon Extend on it get the terminal.
- **The Silicons you look after.** When you are a Silicon's custodian (in Silicon Accounts), you see
  and can stop what it does in Extend, but never act as it:

  ```sh
  extend silicon ls                                   # the Silicons you look after, and the ones you gave access to
  extend silicon show si:chef                         # every device it can use, whoever gave the access
  extend session ls --silicon si:chef --state active  # its sessions; stop one with `extend session end <id>`
  extend file ls --silicon si:chef                    # its files (`extend file get` downloads them)
  extend request ls --silicon si:chef                 # its requests
  extend silicon renounce si:chef 7c1e09ab            # give up its access to one device
  ```
- **Ting.** If Ting doesn't know Extend's notification types, `extend ting status` (and the output
  of a grant) lists them with what each is for. While the Extend server sends no notifications
  through Ting, it says so; requests and wake requests still show on the website, in the CLI and on
  the device.
- **Signing out** (`extend logout`) as a Carbon ends the running sessions of the Silicons you gave
  access to, on your own pairs. Nothing else changes.

## What Extend 4 removed

Devices aren't shared through groups any more, and there are no test environments. These Extend 3
spellings are refused with exit 2, saying what to do instead: `--team`, `--test`, `extend team …`, `extend permission …`, `extend env`,
`extend config test …`, `extend login contexts|use`, `extend device import|importable|visibility`,
and the flags `--visibility`, `--team-visible`, `--all-teams` and `--only-team`. A script that still
sets `EXTEND_TEST_SECRET` is refused before anything is sent. A sign-in Extend 3 saved is never used:
sign in again, which replaces it and deletes Extend 3's `contexts/` and `test/`.

## Scripts and agents

- `--json` prints exactly one document, the convention every Silicon Apps CLI follows. On success,
  the data itself on stdout with no wrapper (`extend accounts --json` prints `{"app_id": "extend",
  ...}`; a device command prints its command result). On failure, `{"error": {"code", "message",
  "hint", "request_id", "docs_url", "details", "exit_code"}}` on stderr and nothing on stdout.
  (`extend login --json` is the one stream: one JSON line per event.)
- Exit codes are stable (0 ok, 1 failed on the device, 2 usage, 3 not signed in or the sign-in
  ended, 4 forbidden, 5 not found, 6 conflict, 7 offline, 8 paused, 9 timeout, 10 unsupported,
  12 rate limited, 13 versions, 14 service unavailable; 11 is no longer used).
- `--session <id>` or `EXTEND_SESSION` picks a session without connecting, so several processes can
  share one home; the sign-in is refreshed for all of them at once.
- `-v` prints one line per call on stderr (method, path, result, time; the request id of a failed
  call). `NO_COLOR` turns colour off unless the `color` setting says otherwise.
- Unknown flags, settings and values are refused with exit 2, naming what is accepted.

## Settings

`extend config ls` shows them all: `api_url`, `accounts_url`, `telemetry` (on by default), `output`,
`screenshot_scale`, `self_destruct`, `download_dir`, `color`. `EXTEND_API_URL` and `ACCOUNTS_URL`
override the two URLs.

### In-use banner (1.1)

A Carbon who paired the device can run `extend device banner <device_id> on|off`.
The setting belongs to the physical device, so it applies to every Carbon's pair.
`extend device show <device_id>` includes the current banner setting. This never ends a session;
Stop remains available in the Extend app and on the website. With the setting on, the banner
shows for 10 seconds per session. Requests waiting for a Carbon remain visible. Android's quiet
foreground-service notification and Apple's Automation Running banner remain platform requirements.
