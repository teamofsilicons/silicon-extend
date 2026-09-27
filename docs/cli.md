# The `extend` CLI

The CLI is the primary way to use Extend, for Carbons and Silicons alike. The website is a subset of
it. The full contract — every command, what it takes, what it prints, every error and exit code — is
[`understanding/cli.yaml`](../understanding/cli.yaml). `extend --help` carries the same
documentation, as a tree: every node says what it's for, how it's used with other commands, and what
is under it.

## Install and sign in

```sh
honeycomb install 'extend'
extend iam --json            # app_id "extend" — generate a short-lived token for it with Silicon IAM
extend login <slt>           # never a password; the SLT comes from the IAM CLI or consent screen
extend login status --json   # {"authenticated": true, "member": {...}, ...}; not signed in: {"authenticated": false, "reason": ...}, still exit 0
```

State lives in `$SILICON_HOME/.extend` (or `~/.extend`); `extend config home <dir>` moves it,
login included, to `<dir>/.extend` (or, with `--use-existing`, switches to state already there).
Files are private to the user (0600). `extend version` says whether this CLI is current, deprecated
or sunset, from Extend's compatibility matrix.

## For a Silicon

```sh
extend device ls                          # devices you have access to, online, and who is using them
extend device show 7c1e09ab               # what works there now, and what's missing and why
extend session new 7c1e09ab --connect     # start, connect; prints the session id
extend --help                             # now lists only commands that work on that device
extend snapshot -i                        # @refs for the interactive elements
extend click @e2                          # act; refs stay valid until the next snapshot
extend fill @e3 "hello"                   # typed text is redacted in the activity log
extend screenshot --ttl 7d --out shot.png # stored in Briefcase; self-destructs in 7 days; also downloaded here
extend terminal run "ls ~/Downloads"      # computers only
extend tv-remote press select             # TVs only
extend takeover --reason "Approve Face ID" # hand the device to its Carbon; commands wait until Done
extend session end                        # free the device (it also ends after 5 idle minutes)
```

When another Silicon is using the device, `extend session new` exits 6. It tells you who and since
when only when that Silicon is on your side (your Team, given access by the same Carbon); otherwise
it just says the device is in use. Ask for it with `extend request send <device_id> --reason "..."`
(1–300 characters, delivered through Ting exactly as written). On your side it goes to the Silicon
using the device; otherwise to the Carbon who gave that Silicon access, who sees your id and reason
and can stop the session ("Sent to the Carbon who gave access to the Silicon using it"). Every new
reason is sent; the same reason again within 60 s is treated as a repeat. If Ting can't take it yet,
`extend request ls` shows it pending with the reason why, and it fails with a reason after 6 counted
attempts or 24 hours.

### Your Team

Everything you do on a device (your access, sessions, files, requests and wake requests) belongs to
the Team you act in: your default team, or `--team`. A Carbon can give you access to the same device
in several of your Teams; `extend --team labs device ls` lists the ones you can use in labs. Every
command Extend suggests to you already names the Team (`extend --team labs session new 0d44e1f2`).

### A device that isn't awake

`extend device ls` and `extend device show` say whether a device is awake: yes; no, and why (screen
off, locked, asleep, standby, another account); or "—" when Extend can't tell (offline, an app older
than 1.1, or an iPhone or iPad). Nothing waits for it: sessions start, and the terminal and Android
debugging work, while a device isn't awake. A command that needs the screen fails with the device's
own error and the wake hint. Extend never wakes a device; ask its Carbon:

```sh
extend --team labs device wake 0d44e1f2 --reason "I need the TV on to check the order screen"
extend --team labs device wake 0d44e1f2 --cancel     # withdraw it
extend --team labs device wake-requests ls 0d44e1f2   # your request, and whether the device showed it
```

The device shows your name and reason where it can (a phone's lock screen, a computer's
notifications), and the Carbon gets it through Ting. When the device is turned on or unlocked (or
its Carbon says "It's awake"), you get a Ting with the command to start. Ask again only after 5
minutes (that refreshes it); a request expires 30 minutes after your last ask.

Files a command makes are printed with their Briefcase links. `--out` (and `extend file get
<file_id>`) downloads them through Extend, which reads them from Briefcase as you, after printing the
links. A file Extend couldn't store or share is a `warning: …` line on stderr saying why and what to
do; the command's exit code doesn't change. If the session ends while a command runs (the Carbon
pressed Stop, access was removed, the device was removed), the command answers at once with exit 6,
says the command may have run, and the CLI disconnects the session.

## Arguments, `--` and local files

- **Global flags** (`--json`, `--timeout`, `--session`, `--team`, `--test`, `-h`, `-v`) work
  anywhere on a command line, except as below.
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
  `install`, `reinstall`, `adb install` and `adb push` take a local regular file only; a Briefcase
  file id, a link, a directory or an `.aab` is refused with the command to run instead (for example
  `extend file get <file_id> --out ./app.apk`). For a larger APK, push it in parts and install it
  on the device; `extend adb --help` shows the recipe.

## For a Carbon

```sh
extend device pair 4f9c2a --name "Saket's Pixel" --access si:chef   # code shown on the device
extend device setup 7c1e09ab --watch                                # the device's own setup steps
extend device attach 2e7f00d1 --os ios --name "Saket's iPhone"      # iPhone/iPad/Apple TV/TVs, through a paired computer
extend device access grant 7c1e09ab si:sous
extend device ttl 7c1e09ab 30                                       # stays paired 30 days without activity
extend device activity 7c1e09ab --since 2h                          # every action, who, when
extend device stop 7c1e09ab                                         # stop the Silicon using it now
extend device rm 7c1e09ab --yes
extend device ls --removed                                          # also removed devices; their activity stays readable
```

Your devices are yours, not a Team's: `extend device ls` lists every device you paired whichever
Team is selected, and your device's activity, requests, sessions and files show every Team's, each
tagged with its Team. `--team` chooses the Team a grant goes into.

```sh
extend device access grant 7c1e09ab si:scout --team labs          # a Silicon from another of your Teams
extend device access revoke 7c1e09ab si:scout --team labs         # only that Team's grant (without --team: every Team's)
extend team silicons --all-teams                                    # Silicons of every Team your Extend login reaches
extend device setup 51ab93c0 --retry                                # run failed setup steps again, then follow them
extend device setup 51ab93c0 --retry --step developer_mode          # one step
extend device wake-requests ls 0d44e1f2 --open                      # who asked you to wake it, and why
extend device wake-requests answer 0d44e1f2 woken                   # "It's awake": answers every request on the device
extend device wake-requests answer 0d44e1f2 declined
extend device wake-requests mute 0d44e1f2 --silicon si:chef         # no more wake requests from si:chef (unmute to undo)
extend ting status --all-teams                                      # do Extend's Tings reach you, per Team?
extend ting on --team labs                                          # turn them on again in labs
```

- **Several Carbons, one device.** "Pair with another Carbon" in a device's Extend app shows a code;
  `extend device pair <code> --name <name>` gives you your own pair of it (your own id, name, access
  and lifetime). Any Carbon who paired it can stop the Silicon using it; you see only your own
  Silicons, and another Carbon's as "in use". On a computer several Carbons paired, only Silicons
  given access by the Carbon who installed Silicon Extend on it get the terminal.
- **Your login reaches Teams one by one.** Granting in a Team, listing its Silicons, opening files
  made there and getting Tings there need your Extend login for that Team. Otherwise the CLI says
  which Team to sign in for.
- **Ting types are per Team.** If Ting doesn't know Extend's notification types in a Team, `extend
  ting status` (and the grant, `request send` and `device wake` output) prints the exact command a
  Ting manager of that Team runs.
- **Signing out** (`extend logout`) as a Carbon ends the running sessions of the Silicons you gave
  access to. Nothing else changes.
- **Deprecated:** `device visibility` exits 2 and `device ls --team-visible` lists nothing (a device is
  visible only to the Carbons who paired it); `device pair --visibility` is ignored.

## Test environments

```sh
printf %s "$TEST_APP_SECRET" | extend config test add <test_id>   # the secret never goes on the command line
extend --test <test_id> login si:chef                             # a test member id works as the login
extend --test <test_id> device ls
extend --test <test_id> env show                                  # test-only; without --test it exits 11
EXTEND_TEST_SECRET="$TEST_APP_SECRET" extend --test <test_id> device ls   # for scripts: no config test add
```

The environment is printed on stderr as the last line of every `--test` command, also when it fails
before anything ran, so scripts reading stdout (JSON, screenshots) are unaffected. The secret must
belong to `<test_id>`. With `EXTEND_TEST_SECRET` set but no `--test`, nothing is sent, so a test
script never reaches production. Production and test logins are stored separately.

## Scripts and agents

- `--json` prints exactly one document, the convention every Team CLI follows. On success, the data
  itself on stdout with no wrapper (`extend iam --json` prints `{"app_id": "extend", ...}`; a device
  command prints its command result). On failure, `{"error": {"code", "message", "hint",
  "request_id", "docs_url", "details", "exit_code"}}` on stderr and nothing on stdout.
- Exit codes are stable (0 ok, 1 failed on the device, 2 usage, 3 not signed in, 4 forbidden,
  5 not found, 6 conflict, 7 offline, 8 paused, 9 timeout, 10 unsupported, 11 test environment,
  12 rate limited, 13 versions, 14 service unavailable).
- `--session <id>` or `EXTEND_SESSION` picks a session without connecting, so several processes can
  share one home.
- `-v` prints one line per call to Extend on stderr (method, path, result, time; the request id of a
  failed call). `NO_COLOR` turns colour off unless the `color` setting says otherwise.
- Unknown flags, settings and values are refused with exit 2, naming what is accepted.

## Settings

`extend config ls` shows them all: `api_url`, `telemetry` (on by default), `output`, `team`,
`screenshot_scale`, `self_destruct`, `download_dir`, `color`.

### In-use banner (1.1)

A Carbon who paired the device can run `extend device banner <device_id> on|off`.
The setting belongs to the physical device, so it applies to every Carbon's pair.
`extend device show <device_id>` includes the current banner setting. This never ends a session;
Stop remains available in the Extend app and on the website. With the setting on, the banner
shows for 10 seconds per session. Requests waiting for a Carbon remain visible. Android's quiet
foreground-service notification and Apple's Automation Running banner remain platform requirements.
