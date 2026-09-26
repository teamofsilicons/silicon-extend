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
extend login status --json   # {"authenticated": true, "member": {...}, ...}; exit 3 when not signed in
```

State lives in `$SILICON_HOME/.extend` (or `~/.extend`); move it with `extend config home <dir>`.
Files are private to the user (0600).

## For a Silicon

```sh
extend device ls                          # devices you have access to, online, and who is using them
extend device show 7c1e09ab               # what works there now, and what's missing and why
extend session new 7c1e09ab --connect     # start, connect; prints the session id
extend --help                             # now lists only commands that work on that device
extend snapshot -i                        # @refs for the interactive elements
extend click @e2                          # act; refs stay valid until the next snapshot
extend fill @e3 "hello"                   # typed text is redacted in the activity log
extend screenshot --ttl 7d --out shot.png # stored in Briefcase; self-destructs in 7 days; also saved locally
extend terminal run "ls ~/Downloads"      # computers only
extend tv-remote press select             # TVs only
extend takeover --reason "Approve Face ID" # hand the device to its Carbon; commands wait until Done
extend session end                        # free the device (it also ends after 5 idle minutes)
```

When another Silicon is using the device, `extend session new` exits 6 and tells you who and since
when. Ask for it with `extend request send <device_id> --reason "..."` (1–300 characters, delivered
through Ting exactly as written).

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
```

## Test environments

```sh
printf %s "$TEST_APP_SECRET" | extend config test add <test_id>   # the secret never goes on the command line
extend --test <test_id> login si:chef                             # a test member id works as the login
extend --test <test_id> device ls
extend --test <test_id> env show                                  # test-only; without --test it exits 11
```

The environment is printed on stderr after every `--test` command, so scripts reading stdout (JSON,
screenshots) are unaffected. Production and test logins are stored separately.

## Scripts and agents

- `--json` prints exactly one document: `{"ok": true, "data": ...}` or
  `{"ok": false, "error": {"code", "message", "hint", "request_id", "exit_code"}}`.
- Exit codes are stable (0 ok, 1 failed on the device, 2 usage, 3 not signed in, 4 forbidden,
  5 not found, 6 conflict, 7 offline, 8 paused, 9 timeout, 10 unsupported, 11 test environment,
  12 rate limited, 13 versions, 14 service unavailable).
- `--session <id>` or `EXTEND_SESSION` picks a session without connecting, so several processes can
  share one home.

## Settings

`extend config ls` shows them all: `api_url`, `telemetry` (on by default), `output`, `team`,
`screenshot_scale`, `self_destruct`, `download_dir`, `color`.
