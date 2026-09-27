# extend-hosted

Drivers for devices that can't run the Extend app themselves: they are operated by a paired Mac or
computer (the "host"). The desktop agent calls `driver_for(HostedDevice)` when the service sends an
`attach` frame and then talks to the driver only through `extend_driver::Driver`. `discover(os, timeout)`
lists candidates on the network (or, for iPhone and iPad, attached to this Mac).

| Device | `DeviceOs` | Host | How |
|---|---|---|---|
| iPhone, iPad | `ios`, `ipados` | Mac only | agent-device's physical-iOS driver (XCTest runner on the device, only while a Silicon is working on it) |
| Apple TV | `tvos` | Mac only | Companion protocol (native Rust: HAP pairing, OPACK) + AirPlay |
| Samsung TV | `samsung_tv` | Mac, Windows, Linux | Tizen remote-control WebSocket + REST |
| LG TV | `lg_tv` | Mac, Windows, Linux | webOS SSAP WebSocket + pointer socket |

Construction does no I/O; drivers connect on the first `probe` or `run`. Every driver keeps its
state (tokens, keys, discovered addresses) as JSON in `HostedDevice::state_dir`, written atomically
and readable only by the user.

## iPhone and iPad (`ios.rs`)

Each command runs `<agent_device argv> <command> <args> --platform ios --udid <udid> --json --session extend-<session id>`.
The flags Extend adds go before a `--` in the Silicon's arguments, since agent-device reads every
token after it as text (`type -- --json` types "--json").

- `record start --quality normal|high` (`cli.yaml`) is passed as agent-device's `medium|high`
  (`medium` is accepted too); anything else is refused with the two values to use.
- Refuses agent-device's reserved flags (`--udid`, `--platform`, `--session`, …) and the commands
  Extend doesn't expose (`devices` → "use `extend device ls`", code `unknown_command`).
- Files come back as `LocalFile`s by handing agent-device explicit paths in the workdir:
  `screenshot [name]` (→ `<name>.png`), `diff screenshot` (`--baseline`/current resolved from
  attachments, `--out` in the workdir), `record start [name]` (written under `state_dir`, moved into
  the workdir on `record stop`), `close --save-script [path]`, `test` suite artifacts. Anything else
  the command leaves in the workdir is returned too.
- `replay`/`test`/`batch --steps-file` read their scripts from the command's attachments.
- `text` is rendered by the driver (agent-device's `--json` carries data, not its CLI text): snapshots
  as `@e9 [cell] "General"` lines, screenshots as `Screenshot general.png (390x844)`, otherwise
  agent-device's `message`.
- If agent-device says the device is claimed by another `extend-…` session (left behind by a crash
  or a lost session end, or the `extend-setup` session of a helper install Extend stopped in the
  middle of), the driver closes that session and retries once. agent-device names a session of
  another daemon in `details.owner` and one of the same daemon in `details`; both count. Sessions
  started outside Extend are never touched.
- `session_ended` closes the agent-device session, which finishes a running recording first (see
  below).

Setup steps (probe, via `xcrun devicectl`):

1. `connect` — "Plug the iPhone into this Mac and tap Trust" (device listed and `pairingState: paired`).
   With no UDID from the service, the driver adopts the iPhone of that name, or the only one.
2. `developer_mode` — "Turn on Developer Mode (Settings › Privacy & Security)" (`device info details`).
3. `helper` — "Extend helper installed": looks for agent-device's runner in `device info apps`
   (again every 10 minutes). Only a device that answers without it gets it: the driver then runs
   `prepare ios-runner` in the background (in progress → done or failed, with the signing hint:
   `AGENT_DEVICE_IOS_TEAM_ID` / `AGENT_DEVICE_IOS_BUNDLE_ID`; tried again a minute after a failure).
   A check that fails (a locked iPhone) never installs: the step stays done if the helper was seen
   before (`helper_verified` in `ios.json`), else it's to do with devicectl's message. Not while a
   Silicon's session may be live either ("Extend puts its helper back … once the Silicon's session
   there ends").

Development override: `EXTEND_HOSTED_ALLOW_SIMULATOR=1` accepts a Simulator UDID (and `discover`
lists Simulators); the three steps report done.

### "Automation Running": the runner only while a Silicon is working on the device

While agent-device's XCTest runner (`xcodebuild test-without-building … AgentDeviceRunnerUITests …
-destination platform=iOS,id=<udid>`) runs, iOS shows "Automation Running" on the device. Apple draws
it for every XCTest UI automation and nothing may hide it, so Extend keeps the runner to the time a
Silicon is working on the device:

- **Setup never leaves a runner.** Probes use `devicectl` only. The helper install is
  `close`, bare `open` (binds the `extend-setup` session to the device; launches nothing, starts no
  runner), `prepare ios-runner` (starts the runner to prove the install), `close`, all with
  `--session extend-setup`; then the driver makes sure the runner is gone (below). `ios.json` notes
  the setup session as open (`setup_open`) from the `open` until the last `close` has been answered,
  so a setup session left open by Extend stopping in the middle of an install (it claims the device,
  refusing every Silicon command `DEVICE_IN_USE`, and keeps agent-device's daemon from idling out)
  is closed by the driver's next look after the device, or by the first command that meets it.
- **A session's end stops it.** Every session end the agent hears of (`session_ended` for any
  reason: the Silicon ended it, idle timeout, the Carbon's Stop, access removed, …; the device
  removed, which ends every session the driver has open; the Mac app quitting, which closes the
  carried devices' sessions too; and a session the service ended while the Mac was away, found at
  reconnect because the service's greeting attaches the device again without announcing it)
  closes the agent-device session. That close finishes a running recording and, on an iPhone or
  iPad, stops the runner (agent-device never keeps a physical device's runner warm); `--shutdown`
  isn't passed, since on a Simulator it shuts the Simulator down. Then the driver checks the
  processes: it waits up to 10 s for this device's runner to exit and otherwise stops exactly that
  runner (`kill -TERM`, then `-KILL` after 5 s), matched by the UDID in its `-destination` (and, on a
  Simulator, the runner app under `CoreSimulator/Devices/<udid>/`, which runs under the Simulator's
  launchd, not under `xcodebuild`), so never another device's. On a Simulator agent-device keeps the
  runner warm after `close` on purpose; the driver stops it at once, as on the device. A runner that
  agent-device's lease (`~/.agent-device/apple-runner/leases/<udid>.json`) says a session outside
  Extend started is never stopped. A session's entry in `ios.json` goes only once agent-device has
  answered its `close`, so a close cut short (the app quitting) is done again later.
- **A runner agent-device is still starting is never stopped.** agent-device takes a start cut short
  for a failed one: it builds and starts another runner, with no session left to close it (seen on a
  Simulator when a session ended right after an `open`, whose runner agent-device starts in the
  background). The driver stops a runner only once agent-device has finished starting it: no
  `xcodebuild build-for-testing` for the device runs, and each runner process has answered a command
  since it said it listens (its lease's log, `AGENT_DEVICE_RUNNER_PORT=<port>` then
  `AGENT_DEVICE_RUNNER_COMMAND_COMPLETED`), or has listened for 30 s, or is older than 2 minutes. A
  runner left alone at a session's end is stopped by the next look (below) once started.
- **The driver looks after the device every 20 s**, connected to Extend or not (a task each iOS
  driver starts when it's built inside a tokio runtime, until the driver is dropped), and only while
  nothing else uses the device (no command, session end or helper install):
  - the runner is stopped (once started) when no session is live, or **when no command has used the
    device for a minute, inside a live session too**, unless it is recording. agent-device keeps the
    session: its next command starts the runner again (3–5 s on a Simulator; about 15 s on an
    iPhone, judging by the runner restarts in agent-device's log of a real iPad), with the session's
    app still bound. So "Automation Running" leaves the device about a minute after the Silicon's
    last action (1–1.3 minutes, with the look every 20 s), also while the Silicon's session is still
    live or the Carbon has taken over. On an iPhone, the `close` of a session whose runner was
    stopped this way can take up to about 15 s longer (from agent-device's code: its `close` first
    asks the runner to shut down, for up to 15 s; not measured on a device).
  - a setup session an interrupted install left open is closed (above);
  - backstop for a session whose end never reached this Mac (it stays offline, say): a session no
    command, takeover start or takeover end has come for in longer than any live session can go
    without one (5 min idle + 30 min takeover + offline grace, ≈ 43 min) is closed. The agent passes
    each takeover's start and end to the driver (`Driver::session_active`), so a run of takeovers
    never looks like that.
- For a daemon it starts, the driver (and the Mac app's `runtime-entry.mjs`) sets
  `AGENT_DEVICE_IOS_RUNNER_IDLE_STOP_MS=30000` (a Simulator runner kept warm after `close` stops
  after 30 s, not 5 minutes) and `AGENT_DEVICE_IOS_RUNNER_DETACH=0` (a daemon that exits stops its
  runners instead of handing them to the next daemon, where a physical device's runner kept running
  for up to a day). A value already in the environment wins; a daemon already running keeps its own
  until it's replaced.
- State every driver built for a device shares (the service attaches a device again on every
  reconnect): one lock, which keeps a runner from being stopped under a running command; when the
  last command ended; and the recordings running.

Which commands need the runner (`runner_use`, from the fork's CoreDevice paths):

| | Commands |
|---|---|
| No runner (`devicectl`) | `apps`, `appstate`, `install`, `reinstall`, `logs`, `close`, `screenshot` (not with `--overlay-refs` or `--crop-on`), `diff screenshot` (not with `--overlay-refs`), bare `open` |
| Launch without it, runner warmed in the background | `open <app\|url>`, `open --relaunch`: agent-device prewarms the runner for the next read, so the banner can appear at `open` (and goes a minute later if nothing uses it) |
| Runner | `snapshot`, `diff snapshot`, `click`, `fill`, `type`, `press`, `longpress`, `focus`, `scroll`, `swipe`, `gesture`, `hover`, `back`, `home`, `app-switcher`, `get`, `find`, `is`, `wait`, `alert`, `keyboard`, `clipboard`, `record`, `replay`, `test`, `batch` |

`screenshot` still needs an agent-device session (`open`, bare or with an app). What the fork can't
do without the runner, Extend doesn't change: no private API hides or avoids the banner.

## Samsung TV (`samsung.rs`)

- Socket: `wss://<ip>:8002/api/v2/channels/samsung.remote.control?name=<base64 "Silicon Extend">&token=<token>`,
  falling back to `ws://<ip>:8001/…` (2016 models, no token). The first connection makes the TV ask
  the Carbon to allow the connection; the token from `ms.channel.connect` is kept in `samsung.json`.
- `tv-remote press|longpress <button> [--duration-ms n]`: `KEY_UP/DOWN/LEFT/RIGHT`, select `KEY_ENTER`,
  back `KEY_RETURN`, `KEY_HOME`, `KEY_MENU`, `KEY_VOLUP/VOLDOWN`, `KEY_MUTE`, `KEY_POWER`; play-pause
  alternates `KEY_PAUSE`/`KEY_PLAY` (Tizen has no toggle key). Holds send `Press`, wait, `Release`
  (the release is sent even if the command is cancelled).
- `back`, `home`; `apps` (`ed.installedApp.get`); `open <name|app id>` (REST
  `POST :8001/api/v2/applications/<id>`, socket `ed.apps.launch` as fallback); `open <url>` (browser
  `org.tizen.browser`, `NATIVE_LAUNCH`, URL as `metaTag`); `open <app> <url>` (`DEEP_LINK`);
  `close [app]` (REST `DELETE`; without a name, the app Extend last opened); `appstate` (REST
  `visible` per installed app); `replay`, `test`, `batch`.
- Probe: `GET http://<ip>:8001/api/v2/` (model, OS, `PowerState`). Steps: `network`, `approve`
  ("Approve the connection on the TV"; failed with the TV's Device Connection Manager path if denied).
- Without an address from the service, the TV is found by SSDP (`urn:samsung.com:device:RemoteControlReceiver:1`) and matched by name.

## LG TV (`lg.rs`)

- Socket: `ws://<ip>:3000`, else `wss://<ip>:3001`. `register` with the standard manifest and
  `pairingType: PROMPT`; the TV asks the Carbon to accept; the `client-key` is kept in `lg.json`. A
  key the TV no longer accepts is dropped and pairing starts again.
- Buttons through `getPointerInputSocket` (`type:button\nname:UP\n\n`): UP, DOWN, LEFT, RIGHT,
  ENTER, BACK, HOME, MENU, VOLUMEUP, VOLUMEDOWN, MUTE, PLAY/PAUSE alternating; power is
  `ssap://system/turnOff`. `longpress` is `unsupported_on_device` (the pointer socket has no hold).
- `apps` (`listLaunchPoints`), `open <name|id>` (`system.launcher/launch`), `open <url>`
  (`com.webos.app.browser` with `target`, falling back to `system.launcher/open`), `open <app> <url>`
  (`contentId`), `close [app]` (`system.launcher/close`, default the foreground app), `appstate`
  (`getForegroundAppInfo`), `back`, `home`, `replay`, `test`, `batch`.
- Probe: `system/getSystemInfo` (model) and `getCurrentSWInformation` (webOS version). SSDP
  (`urn:lge-com:service:webos-second-screen:1`) finds TVs without an address.

## Apple TV (`appletv/`)

Native Rust, no Python at run time:

- `opack.rs` — OPACK encode/decode (decodes pyatv-style object references; encodes literals only).
- `srp.rs` — SRP-6a, 3072-bit group, SHA-512, byte conventions of `srptools` (what pyatv pairs real
  Apple TVs with).
- `hap.rs` — TLV8, Pair-Setup M1–M6 (with signature checks both ways), Pair-Verify M1–M4, HKDF-SHA512,
  ChaCha20-Poly1305 session ciphers, credentials (`to_string()` is pyatv's credential format).
- `companion.rs` — frames, encryption after Pair-Verify, `_systemInfo`/`_touchStart`/`_sessionStart`,
  `_hidC`, `_launchApp`, `FetchLaunchableApplicationsEvent`, `FetchAttentionState`.
- `airplay.rs` — transient HAP pairing (PIN 3939) or Pair-Verify with the Companion credentials,
  HAP-encrypted control connection, AirPlay 2 URL playback (`SETUP`, event channel, NTP timing
  replies, `RECORD`, `/play`, `/rate`, `/feedback`), `PUT /photo`, and a local HTTP server (byte
  ranges) for videos sent as files.

Pairing: the first probe connects and sends Pair-Setup M1, so the Apple TV shows a 4-digit code. The
`code` setup step has `input: "code"`, so the website shows a code field; the code arrives through
`setup_code`. A wrong code is refused and a fresh code is put on screen. Credentials go to
`appletv.json`; if the Apple TV later rejects them, they're dropped and the code step comes back.

Commands: `tv-remote` (up/down/left/right/select, back and menu → Menu, home, play-pause,
volume-up/down, power → sleep or wake from `FetchAttentionState`; mute is unsupported; holds send
down, wait, up), `back`, `home`, `app-switcher` (Home twice), `apps`, `open <name|bundle id|app URL scheme>`
(`http(s)` links are refused: no browser), `close` (the protocol can't quit apps; it goes Home and
says so), `display show --image|--video <file|url>`, `display clear` (`--url`/`--text` are
unsupported: pictures and videos only), `replay`, `test`, `batch`. `appstate` is unsupported.
Ports come from mDNS (`_companion-link._tcp`, `_airplay._tcp`, which also give model and tvOS
version); an address of `ip:port` names the Companion port directly.

## Shared

- `script.rs` runs `replay <script.ad>`, `test <scripts…>` and `batch --steps '<json>'` on TVs, one
  step at a time through the driver's own `run` (`.ad`: `context` lines skipped, `env NAME="v"`
  substitutions, `wait <ms>` pauses).
- `tls.rs` uses the platform TLS (`native-tls`), accepting the TVs' self-signed certificates only on
  connections to the TVs. On Linux this links OpenSSL (`libssl-dev` to build).
- `discover.rs`: mDNS (`mdns-sd`) and SSDP; iPhones and iPads from `devicectl`.
- Error codes: `invalid_args` (as `docs/device-protocol.md` says), `unsupported_on_device`,
  `device_offline`, `device_not_ready` (setup not finished), `command_timeout`, `command_failed`,
  `unknown_command`.

## Verification status

| What | How | Status |
|---|---|---|
| Unit tests: key maps, messages, parsers, URLs, base64 name, TLV8, OPACK, HKDF, ChaCha20, SRP | `cargo test -p extend-hosted` | pass |
| SRP against the published RFC 5054 Appendix B vector (k, x, u, v, A, B, S) | `srp::tests::rfc5054_vector` | pass |
| SRP/OPACK/TLV8/HKDF/ChaCha20 byte-for-byte against pyatv 0.18 + srptools | `tests/fixtures/hap-vectors.json` (generator next to it) | pass |
| Companion client against **pyatv's own server-side pairing and framing** (independent implementation): pair-setup with PIN, pair-verify, encrypted `_systemInfo`, `_hidC`, `_launchApp`, app list | `EXTEND_HOSTED_PYATV_PYTHON=<python with pyatv> cargo test -p extend-hosted -- --ignored interop` | pass |
| Samsung driver end to end over real sockets: mock TV with prompt/token, keys, holds, REST launch/close, browser launch, app list, denial, offline | `samsung::tests` | pass |
| LG driver end to end: mock TV with PROMPT/registered, pointer socket, launch/close, turnOff, key reuse after restart, denial | `lg::tests` | pass |
| Apple TV driver end to end against a mock (pairing with wrong and right code, buttons, apps, open, AirPlay video, unpairing) | `appletv::tests` | pass (mock shares crypto code with the client) |
| AirPlay flow against a mock receiver; file server with byte ranges | `appletv::airplay::tests` | pass (mock only) |
| iPhone path on an **iOS 18.4 Simulator** (iPhone 16 Plus): probe, `open com.apple.Preferences --relaunch`, `snapshot -i`, `click` on the General row's button ref (agent-device refuses the row's own `@ref`, since its child controls cover its touch point, and on some models `label="General"` matches both), `screenshot general` → PNG `LocalFile`, `snapshot`, `record start/stop` → MP4 `LocalFile`, `devices` refused | `EXTEND_HOSTED_ALLOW_SIMULATOR=1 EXTEND_HOSTED_SIM_UDID=<udid> cargo test -p extend-hosted -- --ignored --nocapture simulator_end_to_end` (with the same `AGENT_DEVICE_*` directories as below to keep off the Mac app's daemon) | pass |
| `devicectl` parsing against real output (anonymised fixtures from a paired iPhone: list, details, a locked-device error) | `ios::tests::parses_devicectl_output` | pass |
| Runner lifecycle decisions: which commands need the runner; the runner found by exact UDID (not another device's, not a project's own tests, not one a non-Extend session leased; a Simulator's runner app included); a runner still starting is never stopped (building, just launched, not listening, listening under 30 s without an answer); when the look after the device stops it (no session live; a minute unused; never while recording); setup installs only on a device that answered without the helper, never on a failed check or under a live session; against a stand-in agent-device: a session end closes `extend-<id>`, a device's removal closes every open session, setup runs `close`/`open`/`prepare`/`close` in `extend-setup` and leaves `setup_open` cleared, an install cut short leaves `setup_open` set and the next look closes that session, a command the same daemon refuses `DEVICE_IN_USE` because `extend-setup` holds the device closes it and runs, an idle-forever session is closed and a live one isn't, a takeover's start or end keeps a session from that backstop | `cargo test -p extend-hosted ios::` | pass |
| Runner lifecycle on an **iOS 18.4 Simulator** (iPhone 16 Plus), with a monitor printing every change of the Simulator's runner processes, run twice: with the driver's daemon defaults, and with agent-device's own idle stop off (`AGENT_DEVICE_IOS_RUNNER_IDLE_STOP_MS=0`, so every stop is the driver's). Runner up during `open`/`snapshot`/`scroll`; with no command for a minute it went though the session was live (74 s after the last command both times, the look being every 20 s), and the session's next `scroll up` started it again (2.7 s / 5.0 s) with Settings still the session's app; gone within 0.5 s of `session_ended`; none after `apps`, bare `open`, `screenshot`; a session ended 0.15 s after `open com.apple.Preferences` (runner still starting): not cut short, gone 30 s later (agent-device's idle stop) / 46 s later (the driver, once started: its log shows the port line, then `BUILD INTERRUPTED`), and no build or runner came back in the remaining 100–120 s; after the helper install gone within 35 s | `EXTEND_HOSTED_ALLOW_SIMULATOR=1 EXTEND_HOSTED_SIM_UDID=<udid> AGENT_DEVICE_STATE_DIR=<dir> AGENT_DEVICE_IOS_RUNNER_LEASE_DIR=<dir>/leases AGENT_DEVICE_CLAIMS_DIR=<dir>/claims cargo test -p extend-hosted -- --ignored --nocapture simulator_runner` | pass |
| agent-device's `close` finishes a running recording (why a session end no longer runs `record stop` first): `record start --scope device`, then `close` 3 s later: the Simulator's `recordVideo` process gone, the MP4 written, close took 2 s | by hand on the Simulator, own state directory | pass |
| `cargo clippy -p extend-hosted --all-targets -- -D warnings`; `cargo check --target x86_64-pc-windows-msvc -p extend-hosted` | | clean |

Not verified (no hardware here):

- **Physical iPhone/iPad**: `prepare ios-runner` install (needs signing), commands over CoreDevice,
  and the probe steps against a device being trusted or having Developer Mode turned on. The probe's
  parsing was checked against real `devicectl` output only. The runner lifecycle wasn't run against
  a device; what is known from a real iPad (agent-device's own logs on the Mac): a plain `close` of
  its session stopped its runner and removed its lease within about a second; when agent-device
  restarted the iPad's runner mid-session (`BUILD INTERRUPTED`, then a new start), the new runner
  listened about 13 s after the old one's last answer; and the runner's log carries the port line
  the driver reads. Before this change,
  setup's `prepare ios-runner` left a runner up with no session (and re-ran it ten minutes later),
  which is what kept "Automation Running" on an idle iPhone.
- **Real Samsung and LG TVs**: the TLS paths (8002, 3001) and the real prompts. Protocol details
  follow samsungtvws and aiowebostv; only mocks were exercised.
- **Real Apple TV**: pairing with a real tvOS, and every AirPlay step (`/photo` on current tvOS is
  the least certain; transient pairing needs AirPlay access set to "Anyone on the Same Network",
  otherwise the Companion credentials are tried).
