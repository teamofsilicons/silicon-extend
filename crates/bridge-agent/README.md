# bridge-agent: Silicon Bridge for Mac, Windows and Linux

One Rust binary is the Bridge app on all three desktops. It shows a pairing code until a Carbon
claims it. Then it keeps the computer connected to Bridge, runs the commands Silicons send, and
shows which Silicon is using the computer, with a **Stop** button. The wire contract is
[`docs/device-protocol.md`](../../docs/device-protocol.md), and the frame types come from
`bridge-protocol`.

```
             ┌────────────── bridge-agent ──────────────────────────────────────────────┐
 Bridge  ◄───┤ enroll.rs    POST /enrollments + enrollment socket (code, rotations, paired) │
 service ◄───┤ agent.rs     device socket: hello, frames, ping/pong, backoff, close codes   │
         ◄───┤ dispatch.rs  per-session queues, deadline/cancel, attachments, uploads      │
             │ hosted.rs    attach → bridge_hosted::driver_for → routes `target` commands   │
             │ drivers/     local.rs = platform driver + terminal.rs                        │
             │   agent_device.rs  Mac/Linux: node vendor/agent-device … --json             │
             │   probe_macos.rs / probe_linux.rs   what works right now, and why not        │
             │   windows/         Bridge's own Windows driver (UI Automation, SendInput)   │
             │ status.rs    one status value → tray, window, banner, headless, `status`     │
             │ ui/          tray-icon + tao + wry: menu, window, in-use banner              │
             └─────────────────────────────────────────────────────────────────────────────┘
```

## Running it

```
bridge-agent                      # = run: tray icon + window (Mac, Windows, Linux with a screen)
bridge-agent run --headless       # servers and CI: no UI, status lines on stdout
bridge-agent status [--json]      # what the running app is doing (reads {state}/status.json)
bridge-agent probe [--json]       # what this computer can do right now (the `hello` it would send)
bridge-agent stop                 # Stop the Silicon using this computer (POST /api/v1/device/stop)
bridge-agent revoke [--yes]       # Revoke pair, after a typed confirmation (DELETE /api/v1/device)
bridge-agent install-autostart [--headless] [--systemd]   # start at login
bridge-agent uninstall-autostart
bridge-agent exec [--session a3f] [--timeout-ms N] [--out DIR] [--end-session] <command> [args…]
                                  # run one command through the local driver, without Bridge
```

Global flags are `--service-url`, `--credential-store auto|keyring|file`, `--home` and `-v`.

| Setting | Where | Default |
|---|---|---|
| Service URL | `--service-url`, `BRIDGE_API_URL`, `config.json` `service_url` | `https://backend.bridge.teamofsilicons.com` |
| Home | `--home`, `SILICON_HOME` | the OS home |
| Credential store | `--credential-store`, `BRIDGE_AGENT_CREDENTIAL_STORE`, `config.json` | `auto` |
| agent-device | `BRIDGE_AGENT_DEVICE` (path to `bin/agent-device.mjs` or an executable), `config.json` `agent_device` (argv), the copy bundled with the app, then `vendor/agent-device` in a source checkout | — |
| Node | `BRIDGE_NODE`, the bundled copy, `PATH`, `/opt/homebrew/bin`, `/usr/local/bin` | — |
| Terminal shell | `BRIDGE_TERMINAL_SHELL` (argv before the command) | `$SHELL -l -c` (bash, zsh, fish, ksh), else `/bin/sh -c`; `cmd.exe /D /S /C` on Windows |

The state lives in `{home}/.bridge-agent/`. The directory is 0700 and every file in it is 0600:

- `credential.json`: the device credential, only when the file store is in use. With `auto`, the
  credential goes to the OS secret store first: the macOS Keychain, Windows Credential Manager, or
  the Secret Service on Linux. The `keyring` crate keeps it under the service `Silicon Bridge`,
  account `device-credential@<service host>`. The file is the fallback when there's no secret
  store, as on a headless Linux server.
- `status.json`: the live status, read by `bridge-agent status`.
- `agent.lock`: the single-instance lock, so two copies never fight over one credential.
- `attached.json`, `hosted/<device_id>/`: devices this computer carries.
- `agent-device/`: agent-device's own daemon state (`AGENT_DEVICE_STATE_DIR`).
- `sessions/<session_id>/`: recordings and armed replay scripts that outlive a single command.
- `work/<command_id>/`: each command's scratch directory, removed once its result is sent.
- `logs/bridge-agent.log`: the log, rotated at 10 MB.

## The protocol, as implemented

- **Enrollment.** The agent sends `POST /api/v1/enrollments` with the OS, OS version, model, app
  version and agent-device version, then opens the enrollment socket with `Bridge-Enrollment`. It
  follows `code` rotations and answers `ping` with `pong`. On `paired` it stores the credential and
  moves to the device socket. If the socket drops, it reconnects with backoff and first polls
  `GET /enrollments/{id}`, which catches a pairing that happened while it was down. A 401 or 404
  starts a new enrollment. A code that expires without a rotation triggers a re-read. On quit it
  sends `DELETE /enrollments/{id}`.
- **Device socket.** It connects with `Authorization: Bridge-Device …` and
  `Silicon-Bridge-API-Version: 1`, and sends `hello` first. It then reads `GET /api/v1/device` for
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
- **Commands.** Commands run one at a time per (target, session) and in order; different sessions
  run side by side.
  - Before anything runs, the device checks the command itself. It returns `unknown_command` for
    names not in `COMMANDS` (and for `NOT_EXPOSED` names, naming the replacement). It returns
    `invalid_args` for any `RESERVED_FLAGS`. It also refuses flags that would reach outside the
    session: `--config`, `--remote-config`, `--reporter`, `--daemon-*`, `--state-dir`,
    `--session-lock` and the rest. `--json` is dropped because the agent adds it itself.
  - **Deadline.** `timeout_ms` is clamped to 1–300 s, and time spent queued counts against it. A
    fifth of the deadline, at most 5 s, is kept back for uploads. When the deadline passes, the
    command's cancel token fires. The driver then gets 3 s to wind down and the answer is
    `command_timeout`.
  - **Cancel.** `cancel` stops a running or queued command, and the answer is `cancelled`.
  - **Attachments.** Each attachment is decoded into `work/<id>/attachments/`. Every
    `attachment:<name>` argument, and every `--flag=attachment:<name>`, becomes that local path.
  - **Files.** Each file is uploaded with the next unused upload id
    (`PUT /api/v1/device/artifacts/{id}` with `Content-Type`, `X-Content-SHA256` and
    `X-File-Name`), retried on network and 5xx errors, before the `result` is sent. An upload
    that fails makes the result `ok:false` with `upload_failed`. Files beyond the upload ids are
    named in the text.
- **Sessions.** `session_started`/`session_ended` update the indicator and call the driver's
  hooks. On macOS and Linux, ending a session stops a recording and a log capture that are still
  running, then closes the agent-device session. `takeover` shows the reason and **Done**, which
  sends `takeover_done`. `environment` shows or clears the test-environment banner. `unpaired`
  forgets the credential.
- **Devices this computer carries.** On `attach`, the agent calls
  `bridge_hosted::driver_for(HostedDevice { …, state_dir: hosted/<id>, agent_device: <argv> })`.
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
`node …/agent-device.mjs <command> <args> --platform macos|linux --session bridge-<session_id> --json`.
It runs with `AGENT_DEVICE_STATE_DIR`, and with `AGENT_DEVICE_JSON_TEXT=1`, a fork addition (see
`vendor/agent-device/FORK.md`) so one run returns both the JSON and the text the CLI would print.
- **Output paths are always chosen here**, inside the work directory, and come back as files:
  - `screenshot [name]` writes a PNG as a `screenshot`.
  - `diff screenshot --baseline …` reads its inputs from attachments and returns a `diff`.
  - `record start|stop` keeps the video in the session and returns a `recording` on stop.
  - `logs stop` copies `app.log` and returns it as a `log`.
  - `open`/`close --save-script` returns a `replay_script`.
  - `test` returns its artifacts, and `--out` / `--report-junit` are rewritten into the work
    directory.
- Input files (replay scripts, baselines, step files) must come as attachments. A path on the
  Silicon's machine is refused with a precise message.
- agent-device's error codes map to Bridge's: `INVALID_ARGS` becomes `invalid_args`, and
  `UNSUPPORTED_*` becomes `unsupported_on_device`. Every other code is lowercased. The original
  code and hint stay in `details`.

**macOS probe.**
- It checks Accessibility (`AXIsProcessTrusted`) and Screen Recording
  (`CGPreflightScreenCaptureAccess`) without prompting. Those two are the setup steps, and the
  window's **Open** buttons take the Carbon to the right System Settings page.
- Typing (`fill`/`type`/`focus`) uses the native helper with Accessibility and keyboard events.
  The helper checks field ownership and focus before sending text; a focus failure stops the
  command. Text is passed over stdin rather than command-line arguments.
- `record` still uses XCUITest and needs Xcode and **UI Automation** (`automationmodetool`).
  Without that setup, `screen.record` is reported missing with the exact fix. These are not
  setup steps because other commands remain useful without recording.
- Permissions belong to the app that launched the agent: Silicon Bridge.app when installed, or the
  terminal during development.

**Linux probe.**
- It reports only `terminal`, `apps.launch` and `replay` without `DISPLAY`/`WAYLAND_DISPLAY`.
- With a screen, it checks:
  - the AT-SPI bus (the same Python/GI calls agent-device's dumper makes), for `screen.read`
  - xdotool (X11) or ydotool (Wayland), for input
  - gnome-screenshot, scrot or `import` (grim on Wayland), for capture
  - xclip or xsel (wl-clipboard), for the clipboard
  - xdg-open, for links
- agent-device doesn't record or read logs on Linux, so `screen.record` and `logs` are reported as
  `missing`.
- Nothing on Linux blocks sessions: every gap is in `missing`, with the package to install.

**Windows.** This is Bridge's own driver, in `drivers/windows/`.
- UI Automation builds the element tree, with the same snapshot text and JSON shape and the same
  `@eN` refs as agent-device's desktop snapshots. `SendInput` drives the mouse and keyboard, GDI
  takes screenshots, and Win32 handles the clipboard.
- `open`, `close` and `apps` use the Start menu (`Get-StartApps`) and the shell.
- `record`, `logs`, `alert` and `replay`/`test`/`batch` are reported as `missing`.
- A locked computer reports input and screen capabilities as `missing` ("This computer is locked…").
- **It is compile-checked (`cargo check`/`clippy --target x86_64-pc-windows-msvc`) and its pure
  logic is unit-tested on macOS/Linux. It has never run on Windows.**

## The UI

- **Menu bar / tray.**
  - The icon changes with state: an outline while unpaired, solid when paired, orange while a
    Silicon is using the computer, and amber when something needs the Carbon.
  - The menu shows the headline ("Pairing code: 4F9C2A", "Paired to c:alice", "si:chef is using
    this Mac") and the device name.
  - It also shows the test environment, **Stop si:chef**, **Done** during a takeover, and the
    devices this computer carries.
  - The rest of the menu: Show Silicon Bridge…, Start at login, Revoke pair…, Quit.
- **Window** (a wry webview):
  - the big pairing code with a countdown and where to enter it
  - the setup steps with **Open**
  - "Not available yet": each missing capability with its reason
  - the device name, the Carbon it's paired to and the team
  - the test-environment banner and devices carried
  - **Revoke pair…** with an in-window confirmation
  - It opens by itself the first time a pairing code appears.
- **Banner.** A small always-on-top strip at the bottom centre of the screen reads "si:chef is using
  this Mac" with **Stop**, or "si:chef needs you: <reason>" with **Done**.
  - It is shown for as long as a session lasts.
  - It never takes focus, so it doesn't steal the Silicon's typing.
  - Its button works on the first click.

## Start at login

`install-autostart` writes one of:
- macOS: `~/Library/LaunchAgents/com.teamofsilicons.bridge-agent.plist` (RunAtLoad, restart on a
  crash, Aqua sessions only)
- Windows: `HKCU\…\Run\Silicon Bridge`
- Linux: `~/.config/autostart/silicon-bridge.desktop`, or with `--systemd`, a user unit
  `silicon-bridge.service`

## Tests

```
CARGO_TARGET_DIR=target/agent cargo test -p bridge-agent            # 107 unit + 7 integration
CARGO_TARGET_DIR=target/agent cargo clippy -p bridge-agent --all-targets -- -D warnings
CARGO_TARGET_DIR=target/agent cargo check --target x86_64-pc-windows-msvc -p bridge-agent
```

- `tests/fake_service.rs` runs the whole life of a computer against an in-process axum service:
  - enrollment, rotation, ping/pong, paired (the credential stored at 0600) and `hello`
  - device details, a session, and a screenshot uploaded (digest checked) *before* its result
  - a reserved flag refused, cancel, Stop, a takeover and Done, the session ending
  - the environment banner, attach plus a routed TV command, an unknown target, `refresh`
  - `unpaired` followed by a new enrollment
  - Revoke pair, 4401, 4409 (no reconnect until asked), 4426, reconnect after a drop, and a
    refused credential
- `apps/desktop/linux-e2e/` runs the Linux driver for real in Docker. `apps/desktop/README.md`
  has the macOS real-run notes.
