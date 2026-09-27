# extend-agent: Silicon Extend for Mac, Windows and Linux

One Rust binary is the Extend app on all three desktops. It shows a pairing code until a Carbon
claims it. Then it keeps the computer connected to Extend, runs the commands Silicons send, and
shows which Silicon is using the computer, with a **Stop** button. The wire contract is
[`docs/device-protocol.md`](../../docs/device-protocol.md), and the frame types come from
`extend-protocol`.

```
             ┌────────────── extend-agent ──────────────────────────────────────────────┐
 Extend  ◄───┤ enroll.rs    POST /enrollments + enrollment socket (code, rotations, paired) │
 service ◄───┤ agent.rs     device socket: hello, frames, ping/pong, backoff, close codes   │
         ◄───┤ dispatch.rs  per-device queues, deadline/cancel, attachments, uploads       │
             │ hosted.rs    attach → extend_hosted::driver_for → routes `target` commands   │
             │ drivers/     local.rs = platform driver + terminal.rs                        │
             │   agent_device.rs  Mac/Linux: node vendor/agent-device … --json             │
             │   probe_macos.rs / probe_linux.rs   what works right now, and why not        │
             │   windows/         Extend's own Windows driver (UI Automation, SendInput)   │
             │ status.rs    one status value → tray, window, banner, headless, `status`     │
             │ ui/          tray-icon + tao + wry: menu, window, in-use banner              │
             └─────────────────────────────────────────────────────────────────────────────┘
```

## Running it

```
extend-agent                      # = run: tray icon + window (Mac, Windows, Linux with a screen)
extend-agent run --headless       # servers and CI: no UI, status lines on stdout
extend-agent run [--autostart | --no-autostart]   # also turn start at login on or off, for good
extend-agent status [--json]      # what the running app is doing (reads {state}/status.json)
extend-agent probe [--json]       # what this computer can do right now (the `hello` it would send)
extend-agent stop                 # Stop the Silicon using this computer (POST /api/v1/device/stop)
extend-agent revoke [--yes]       # Revoke pair, after a typed confirmation (DELETE /api/v1/device)
extend-agent install-autostart [--headless] [--systemd]   # start at login (remembered)
extend-agent uninstall-autostart                          # stop starting at login (remembered)
extend-agent exec [--session a3f] [--timeout-ms N] [--out DIR] [--end-session] <command> [args…]
                                  # run one command through the local driver, without Extend
```

Global flags are `--service-url`, `--credential-store auto|keyring|file`, `--home`,
`--download-url` and `-v`.

| Setting | Where | Default |
|---|---|---|
| Service URL | `--service-url`, `EXTEND_API_URL`, `config.json` `service_url` | `https://backend.extend.teamofsilicons.com` |
| Home | `--home`, `SILICON_HOME` | the OS home |
| Credential store | `--credential-store`, `EXTEND_AGENT_CREDENTIAL_STORE`, `config.json` | `auto` |
| agent-device | `EXTEND_AGENT_DEVICE` (path to `bin/agent-device.mjs` or an executable), `config.json` `agent_device` (argv), the copy bundled with the app, then `vendor/agent-device` in a source checkout | — |
| Node | `EXTEND_NODE`, the bundled copy, `PATH`, `/opt/homebrew/bin`, `/usr/local/bin` | — |
| Terminal shell | `EXTEND_TERMINAL_SHELL` (argv before the command) | `$SHELL -l -c` (bash, zsh, fish, ksh), else `/bin/sh -c`; `cmd.exe /D /S /C` on Windows |
| Update download page | `--download-url`, `EXTEND_DOWNLOAD_URL`, `config.json` `download_url` (http or https) | `https://extend.teamofsilicons.com/download/mac`, `/windows` or `/linux` |

The state lives in `{home}/.extend-agent/`. The directory is 0700 and every file in it is 0600:

- `credential.json`: the device credential, only when the file store is in use. With `auto`, the
  credential goes to the OS secret store first: the macOS Keychain, Windows Credential Manager, or
  the Secret Service on Linux. The `keyring` crate keeps it under the service `Silicon Extend`,
  account `device-credential@<service host>`. The file is the fallback when there's no secret
  store, as on a headless Linux server.
- `status.json`: the live status, read by `extend-agent status`.
- `agent.lock`: the single-instance lock, so two copies never fight over one credential.
- `attached.json`, `hosted/<device_id>/`: devices this computer carries.
- `start-at-login.json`: the start-at-login choice (`{"start_at_login": bool, "by": "default"|"carbon"}`).
- `agent-device/`: agent-device's own daemon state (`AGENT_DEVICE_STATE_DIR`).
- `sessions/<session_id>/`: recordings and armed replay scripts that outlive a single command;
  `live-session` while the running app has that session in use, and `cleanup-pending.json` while a
  failed cleanup waits to be retried.
- `agent-device/extend-runtime-root.json`: the install path of the runtime that started the
  agent-device daemon (written by the packaged entry, `apps/desktop/runtime-entry.mjs`).
- `work/<command_id>/`: each command's scratch directory, removed once its result is sent.
- `logs/extend-agent.log`: the log, rotated at 10 MB.

## The protocol, as implemented

- **Enrollment.** The agent sends `POST /api/v1/enrollments` with the OS, OS version, model, app
  version and agent-device version, then opens the enrollment socket with `Extend-Enrollment`. It
  follows `code` rotations and answers `ping` with `pong`. On `paired` it stores the credential and
  moves to the device socket. If the socket drops, it reconnects with backoff and first polls
  `GET /enrollments/{id}`, which catches a pairing that happened while it was down. A 401 or 404
  starts a new enrollment. A code that expires without a rotation triggers a re-read. On quit it
  sends `DELETE /enrollments/{id}`.
- **Device socket.** It connects with `Authorization: Extend-Device …` and
  `Silicon-Extend-API-Version: 1`, and sends `hello` first. It then reads `GET /api/v1/device` for
  the name, owner, team, in-use state and environment, and re-reads it on `refresh`.
  - It reconnects with exponential backoff from 1 s to 60 s with full jitter. The backoff resets
    after a connection stays up for 60 s.
  - It treats 60 s without anything from the service as a dead connection.
  - Close codes: `4401` (or a 401/403/404 on upgrade) forgets the credential and goes back to
    pairing. `4409`/`superseded` stops reconnecting until the Carbon chooses "Connect this copy
    instead". `4426` shows "update needed" and retries hourly.
- **Probing.** The agent probes again every 30 s, and every 5 s while a setup step still needs the
  Carbon. It sends a new `hello` when capabilities or `missing` change, and `setup_progress` when
  only the setup changed.
- **Commands.** Each device (this computer, or a device it carries) has one queue: its commands,
  and its sessions' setup and cleanup, run one at a time and in order. Different devices run side by
  side. A command that arrives for a session that already ended is answered `session_ended` at once
  and never runs (the last 256 ended sessions are remembered).
  - Before anything runs, the device checks the command itself. It returns `unknown_command` for
    names not in `COMMANDS` (and for `NOT_EXPOSED` names, naming the replacement). It returns
    `invalid_args` for any `RESERVED_FLAGS`. It also refuses flags that would reach outside the
    session: `--config`, `--remote-config`, `--reporter`, `--daemon-*`, `--state-dir`,
    `--session-lock` and the rest. `--json` is dropped because the agent adds it itself.
  - **Deadline.** `timeout_ms` is clamped to 1–300 s, and time spent queued counts against it. A
    fifth of the deadline, at most 5 s, is kept back for uploads. When the deadline passes, the
    command's cancel token fires. The driver then gets 3 s to wind down and the answer is
    `command_timeout`. A command whose deadline passed while it waited says what it waited on: the
    previous session's cleanup, this session's setup, or the command before it.
  - **Cancel.** `cancel` stops a running or queued command, and the answer is `cancelled`.
  - **Attachments.** Each attachment is decoded into `work/<id>/attachments/`. Every
    `attachment:<name>` argument, and every `--flag=attachment:<name>`, becomes that local path.
  - **Files.** Each file is uploaded with the next unused upload id
    (`PUT /api/v1/device/artifacts/{id}` with `Content-Type`, `X-Content-SHA256` and
    `X-File-Name`), retried on network and 5xx errors, before the `result` is sent. An upload
    that fails makes the result `ok:false` with `upload_failed`. Files beyond the upload ids are
    named in the text.
- **Sessions.** `session_started`/`session_ended` update the indicator and call the driver's
  hooks in the device's queue. Setup finishes before its first command; ending a session cancels
  its queued and running commands and finishes cleanup before the next session starts. Setup hooks
  are cut off after 150 s and cleanup hooks after 180 s, and the cut is logged. A `session_started`
  the service re-sends after a reconnect does not reset the session, so a running recording or log
  capture is still stopped at session end. `takeover` shows the reason and **Done**, which sends
  `takeover_done`. `environment` shows or clears the test-environment banner.
- **Revoke, unpair and Quit.** Revoke pair (in the app or offline), being unpaired (4401, the
  `unpaired` frame, the device removed) and Quit first cancel the running and queued commands of
  every session, then clean each session up on its device's queue; Quit waits up to 10 s.
  `unpaired` then forgets the credential.
- **Cleanup on Mac and Linux.** Ending a session stops a recording and a log capture that are
  still running, then closes the agent-device session (`SESSION_NOT_FOUND` counts as closed, and a
  session where agent-device never ran is not closed at all). If `close` fails twice, the running
  app forces a release: `agent-device daemon stop`, then `device release --stale --platform <p>`
  (claims live per user in `~/.agent-device/device-claims`; `--stale` releases only claims whose
  owner is provably gone). If that fails too, the session is kept in
  `sessions/<id>/cleanup-pending.json` and the computer is **held**: setup stays complete and the
  service keeps it `ready`, but every capability only agent-device provides moves to `missing` with
  one reason (what happened, why, that the release is retried, and that restarting the computer
  clears it). `terminal` and `takeover` keep working, and a new session can start. Background
  retries run after 15 s, 30 s, 1 min, 2 min, then every 5 min, up to 12 per run of the app; they
  force the release only while no session is in use, and every new session start forces it first.
  After a local session's setup or cleanup the app re-checks its capabilities at once. If the app
  restarts mid-session, the session the service announces again is left alone (recording and log
  state from before the restart is lost); a session that ended while the app was down is closed
  when the next one starts. One-off `extend-agent exec --end-session` and `probe` never force and
  never retry; they leave the pending cleanup for the running app.
- **Devices this computer carries.** On `attach`, the agent calls
  `extend_hosted::driver_for(HostedDevice { …, state_dir: hosted/<id>, agent_device: <argv> })`.
  It then sends `attached` with that driver's probe; a device the driver can't carry goes offline,
  with the reason as a failed step. Commands whose `target` is that id go to that driver. An
  attach with `removed: true` drops the device. `setup_code` goes to the driver's
  `setup_code`, and a failure shows up as a failed step. The list survives restarts.

## Drivers and what each computer reports

**Terminal (all three).** `terminal run <command> [--cwd <dir>] [--env K=V]…` runs through the
Carbon's login shell with plain pipes, so stdout and stderr stay separate. It returns
`{stdout, stderr, exit_code, duration_ms}`.
- A non-zero exit returns `ok:false`, `command_failed`, with `details.exit_code`.
- The command runs in its own process group, and the whole group is killed on a timeout or cancel
  (`taskkill /T` on Windows).
- Each stream is inline up to 256 KiB. Past that, the full stream is uploaded as a `log` file.
- One command token is used exactly as given (`terminal run "ls | wc -l"`); several are joined with
  spaces.

**Mac and Linux: agent-device.** Each command becomes
`agent-device.mjs <command> <args> --platform macos|linux --session extend-<session_id> --json`,
run by node. When agent-device is a node script (always, as packaged) node gets the arguments as
JSON over stdin through a small loader, so `ps` shows only
`node --input-type=module -e <loader> -- …/agent-device.mjs` and typed text is in no process's
command line. If `EXTEND_AGENT_DEVICE` or `config.json` names a plain executable instead, its
arguments are on its command line. It runs with `AGENT_DEVICE_STATE_DIR`, and with
`AGENT_DEVICE_JSON_TEXT=1`, a fork addition (see `vendor/agent-device/FORK.md`) so one run returns
both the JSON and the text the CLI would print.
- **Output paths are always chosen here**, inside the work directory, and come back as files:
  - `screenshot [name]` writes a PNG as a `screenshot`.
  - `diff screenshot --baseline …` reads its inputs from attachments and returns a `diff`.
  - `record start|stop` keeps the video in the session and returns a `recording` on stop.
  - `logs stop` copies `app.log` and returns it as a `log`.
  - `open`/`close --save-script` returns a `replay_script`; the script records the app's path.
  - `test` returns its artifacts, and `--out` / `--report-junit` are rewritten into the work
    directory.
- Input files (replay scripts, baselines, step files) must come as attachments. A path on the
  Silicon's machine is refused with a precise message.
- agent-device's error codes map to Extend's: `INVALID_ARGS` becomes `invalid_args`, and
  `UNSUPPORTED_*` becomes `unsupported_on_device`. Every other code is lowercased. The original
  code and hint stay in `details`.
- **Mac app names.** `open <app>` and `close <app>` find the app by its bundle file name, as
  `open -a` does, ignoring case, a trailing `.app` and invisible marks, and pass agent-device its
  absolute `.app` path (so result text may show `/Applications/Visual Studio Code.app`). Search
  order: `/Applications` (3 levels), `~/Applications` (3), `/System/Applications` (2), then
  `/System/Cryptexes/App/System/Applications`, `/System/Library/CoreServices/Applications` and
  `/System/Library/CoreServices` (1 each). If agent-device still answers `APP_NOT_INSTALLED`,
  Spotlight (`mdfind`) is asked and the command retried. Names are resolved after the command is
  planned, so `--save-script` in any position keeps the app. Bundle ids, `settings`, links and
  paths are passed unchanged. Limit: an app whose display name differs from its file name
  (`OBS Studio`, `Code`, `iTerm2`) relies on agent-device's own display-name match.
- **Recording quality.** `record start --quality normal|high` (`cli.yaml`) is passed as agent-device's
  `medium|high` (`medium` is accepted too; anything else is `invalid_args` naming both values). On
  a Mac it picks the export quality; on Linux the fork encodes at 8 Mbit/s (medium) or 20 Mbit/s
  (high), as Android's screenrecord does. The same mapping applies to iPhones and iPads a Mac
  carries (`extend-hosted`).
- **`--`.** Flags the agent adds (`--platform`, `--session`, `--json`) go before a `--` in the
  Silicon's arguments, since agent-device reads every token after it as text (`type -- --json`).

**macOS probe.**
- It checks Accessibility (`AXIsProcessTrusted`) and Screen Recording
  (`CGPreflightScreenCaptureAccess`) without prompting. Those two are the setup steps, and the
  window's **Open** buttons take the Carbon to the right System Settings page.
- Every session uses agent-device's native helper, whatever the state of Xcode or UI Automation
  (there is no XCTest runner gate any more). A session starts on the frontmost app
  (`open --surface frontmost-app`); `open <link>` opens with the system (`/usr/bin/open`) and then
  follows the frontmost app.
- Typing (`fill`/`type`/`focus`) uses the native helper with Accessibility and keyboard events.
  The helper checks field ownership and focus before each key event; a focus failure stops the
  command. Text is passed over stdin rather than command-line arguments.
- Recording uses the helper's ScreenCaptureKit recorder: `screen.record` is reported whenever
  Screen Recording is granted and agent-device supports `record`. Neither Xcode nor UI Automation
  is needed. (`gather()` still runs `automationmodetool` and `xcode-select`, although nothing uses
  their results any more.)
- Permissions belong to the app that launched the agent: Silicon Extend.app when installed, or the
  terminal during development.
- A locked Mac (`CGSSessionScreenIsLocked` in `CGSessionCopyCurrentDictionary`), one showing the
  login window or another account (`kCGSSessionOnConsoleKey` false), or one whose attached main
  display is asleep, reports everything that needs the screen as missing with "This Mac is
  locked…" (or "…showing the login window or another account…", "…asleep…"); the terminal,
  `apps.list`, `logs` and `takeover` stay. It isn't a setup step, so sessions still start.

**Linux probe.**
- Without `DISPLAY`/`WAYLAND_DISPLAY` it reports only `terminal` (`UNDERSTANDING.md`: a computer
  without a screen only gets the terminal); everything else is missing with that reason.
- A session whose lock screen is up (logind's `LockedHint`, from `loginctl show-session` for
  `XDG_SESSION_ID` or the user's display session) reports everything that needs the screen as
  missing with "This computer is locked…", as on a Mac.
- With a screen, it checks:
  - the AT-SPI bus (the same Python/GI calls agent-device's dumper makes), for `screen.read`
  - xdotool (X11) or ydotool (Wayland), for input
  - gnome-screenshot, scrot or `import` (grim on Wayland), for capture
  - xclip or xsel (wl-clipboard), for the clipboard
  - xdg-open, for links
- Recording (X11 only) needs python3, ffmpeg with ffprobe, and xwininfo (x11-utils), and an
  ffmpeg whose `-encoders` list has libx264 and whose `-devices` list has x11grab; the ffmpeg check
  is cached until the binary changes. What is missing is named (libx264, x11grab or the tool), with an install hint that also
  mentions Fedora's `ffmpeg-free` and RPM Fusion. On Wayland `screen.record` is missing (no portal
  recording yet). `logs` is reported missing on Linux.
- Nothing on Linux blocks sessions: every gap is in `missing`, with the package to install.

**Windows.** This is Extend's own driver, in `drivers/windows/`.
- UI Automation builds the element tree, with the same snapshot text and JSON shape and the same
  `@eN` refs as agent-device's desktop snapshots. `SendInput` drives the mouse and keyboard, GDI
  takes screenshots, and Win32 handles the clipboard.
- `open`, `close` and `apps` use the Start menu (`Get-StartApps`) and the shell.
- `record`, `logs`, `alert` and `replay`/`test`/`batch` are reported as `missing`.
- A locked computer reports input and screen capabilities as `missing` ("This computer is locked…").
- **Lock changes.** The agent asks `drivers::screen_lock::current()` every 3 s (Mac, Linux, Windows)
  and checks the computer again as soon as the answer changes, so a lock or unlock reaches Extend
  in seconds rather than at the next 30-second check.
- **It is compile-checked (`cargo check`/`clippy --target x86_64-pc-windows-msvc`) and its pure
  logic is unit-tested on macOS/Linux. It has never run on Windows.**

## The UI

- **Look.** The window and banner follow Silicon Interface's system (paper and cobalt, IBM Plex
  Sans and Mono, Source Serif 4 titles), with the fonts inlined in `ui/page.html` (SIL Open Font
  License 1.1). The tray icon is Extend's mark.
- **Menu bar / tray.**
  - The icon changes with state: Extend's mark, faint while unpaired or when something needs the
    Carbon, solid when paired, and risograph orange-red (`#E0452B`) while a Silicon is using the
    computer.
  - The menu shows the headline ("Pairing code: 4F9C2A", "Paired to c:alice", "si:chef is using
    this Mac") and the device name.
  - It also shows the test environment, **Stop si:chef**, **Done** during a takeover, and the
    devices this computer carries, each in use with its own **Stop si:x on <name>** (and **Done on
    <name>** during a takeover): one click, `stop` with its `target`.
  - The rest of the menu: Show Silicon Extend…, Start at login, Revoke pair…, Quit.
- **Window** (a wry webview):
  - the big pairing code with a countdown and where to enter it
  - the setup steps with **Open**
  - "Not available yet": each missing capability with its reason (while a computer is held, the
    same reason appears once per withheld capability)
  - the device name, the Carbon it's paired to and the team
  - the test-environment banner and devices carried (each in use with **Stop**)
  - **Start at login** with a switch (Turn off / Turn on)
  - while Extend needs a newer app: **Download the update**, which opens the configured download
    page (`download_url`) in the default browser
  - **Revoke pair…** with an in-window confirmation. A carried device's pair can't be revoked from
    here yet (the device API has no route for it); the window says to remove it on the website.
- It opens by itself when enrollment starts, including when a connection error prevents a pairing code.
- **Banner.** A small always-on-top strip at the bottom centre of the screen reads "si:chef is using
  this Mac" with **Stop**, or "si:chef needs you: <reason>" with **Done**.
  - It is shown for as long as a session lasts, on this computer or on a device it carries. Each
    carried device in use adds a row ("si:x is using <name>" with its own **Stop**, or **Done**),
    up to four rows; the window grows by 38 px per row.
  - Drag its grip (the dots) to move it. **−** collapses it to a small pill that keeps **Stop** (or **Done**
    during a takeover) and the test-environment tag, so stopping stays one tap; click the pill to
    restore it. Sizes: 420x52 expanded, 250x44 collapsed (360x44 with an environment). The
    collapsed state resets when the session ends, when the in-use session changes and when a new
    takeover arrives; the position survives status updates.
  - The menu bar keeps **Stop** / **Done** available and includes **Show activity banner**.
  - It never takes focus, so it doesn't steal the Silicon's typing.
  - Its button works on the first click.

## Start at login

`UNDERSTANDING.md`: the app starts on its own when the device starts. Once the computer is paired,
the app with a window turns start at login on (`autostart::after_pairing`), pointing the entry at
this copy (again, if the app was moved), unless the Carbon turned it off: the window's switch, the
menu's **Start at login**, `run --no-autostart` or `uninstall-autostart`. That choice is kept in
`start-at-login.json` and never overridden. A copy running from App Translocation or a disk image
is never registered (the window says to move the app to Applications), and neither, by itself, is
a development build in a Cargo `target/` directory. `run --headless` leaves it
alone unless given `--autostart` (a systemd user unit on Linux).

`install-autostart` writes one of:
- macOS: `~/Library/LaunchAgents/com.teamofsilicons.extend-agent.plist` (RunAtLoad, restart on a
  crash, Aqua sessions only)
- Windows: `HKCU\…\Run\Silicon Extend`
- Linux: `~/.config/autostart/silicon-extend.desktop`, or with `--systemd`, a user unit
  `silicon-extend.service`

## Tests

```
cargo test -p extend-agent            # 164 unit + 10 integration (2026-09-27, macOS)
cargo clippy -p extend-agent --all-targets -- -D warnings
cargo check --target x86_64-pc-windows-msvc -p extend-agent
```

- `tests/fake_service.rs` runs the whole life of a computer against an in-process axum service:
  - enrollment, rotation, ping/pong, paired (the credential stored at 0600) and `hello`
  - device details, a session, and a screenshot uploaded (digest checked) *before* its result
  - a reserved flag refused, cancel, Stop, a takeover and Done, the session ending
  - the environment banner, attach plus a routed TV command, an unknown target, `refresh`
  - `unpaired` followed by a new enrollment
  - Revoke pair, 4401, 4409 (no reconnect until asked), 4426, reconnect after a drop, and a
    refused credential
  - capabilities re-checked right after a session's setup and cleanup
  - a lock and an unlock reported within seconds through the screen watch
- The unit tests cover, among others: interrupting a running command at session end, idle worker
  restart, a hung cleanup cut off, late commands for an ended session, `SESSION_NOT_FOUND` on close,
  held-computer reporting, forced and non-forced background retries, restart mid-session, a session
  that ended while the app was down, named Mac apps with `--save-script`, and banner collapse and
  restore.
- `apps/desktop/linux-e2e/` runs the Linux driver for real in Docker. `apps/desktop/README.md`
  has the macOS real-run notes.
- Not run on a real desktop (2026-09-27): the banner's collapse, restore and drag in real windows on
  macOS, Windows and Linux; named apps opened by the real helper (`open 'Visual Studio Code'`,
  `open iTerm`, `open WhatsApp`); `open <URL>` then `snapshot`; a forced release after a real stuck
  recording.
