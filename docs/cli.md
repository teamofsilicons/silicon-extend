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

When another Silicon is using the device, `extend session new` exits 6 and tells you who and since
when. Ask for it with `extend request send <device_id> --reason "..."` (1–300 characters, delivered
through Ting exactly as written). Every new reason is sent; the same reason again within 60 s is
treated as a repeat. If Ting can't take it yet, `extend request ls` shows it pending with the reason
why, and it fails with a reason after 6 attempts.

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
