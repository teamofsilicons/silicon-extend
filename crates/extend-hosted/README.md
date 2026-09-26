# extend-hosted

Drivers for devices that can't run the Extend app themselves: they are operated by a paired Mac or
computer (the "host"). The desktop agent calls `driver_for(HostedDevice)` when the service sends an
`attach` frame and then talks to the driver only through `extend_driver::Driver`. `discover(os, timeout)`
lists candidates on the network (or, for iPhone and iPad, attached to this Mac).

| Device | `DeviceOs` | Host | How |
|---|---|---|---|
| iPhone, iPad | `ios`, `ipados` | Mac only | agent-device's physical-iOS driver (XCTest runner on the device) |
| Apple TV | `tvos` | Mac only | Companion protocol (native Rust: HAP pairing, OPACK) + AirPlay |
| Samsung TV | `samsung_tv` | Mac, Windows, Linux | Tizen remote-control WebSocket + REST |
| LG TV | `lg_tv` | Mac, Windows, Linux | webOS SSAP WebSocket + pointer socket |

Construction does no I/O; drivers connect on the first `probe` or `run`. Every driver keeps its
state (tokens, keys, discovered addresses) as JSON in `HostedDevice::state_dir`, written atomically
and readable only by the user.

## iPhone and iPad (`ios.rs`)

Each command runs `<agent_device argv> <command> <args> --platform ios --udid <udid> --json --session extend-<session id>`.

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
  or a lost session end), the driver closes that session and retries once. Sessions started outside
  Extend are never touched.
- `session_ended` stops a running recording and closes the agent-device session.

Setup steps (probe, via `xcrun devicectl`):

1. `connect` — "Plug the iPhone into this Mac and tap Trust" (device listed and `pairingState: paired`).
   With no UDID from the service, the driver adopts the iPhone of that name, or the only one.
2. `developer_mode` — "Turn on Developer Mode (Settings › Privacy & Security)" (`device info details`).
3. `helper` — "Extend helper installed": looks for agent-device's runner in `device info apps`; if
   it's missing, the driver runs `prepare ios-runner` in the background (in progress → done or
   failed, with the signing hint: `AGENT_DEVICE_IOS_TEAM_ID` / `AGENT_DEVICE_IOS_BUNDLE_ID`).

Development override: `EXTEND_HOSTED_ALLOW_SIMULATOR=1` accepts a Simulator UDID (and `discover`
lists Simulators); the three steps report done.

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
| iPhone path on an **iOS 18.4 Simulator** (iPhone 16e): probe, `open com.apple.Preferences --relaunch`, `snapshot -i`, `click @e9` (General), `screenshot general` → PNG `LocalFile`, `snapshot`, `record start/stop` → MP4 `LocalFile`, `devices` refused | `EXTEND_HOSTED_ALLOW_SIMULATOR=1 EXTEND_HOSTED_SIM_UDID=<udid> cargo test -p extend-hosted -- --ignored --nocapture simulator` | pass |
| `devicectl` parsing against real output (anonymised fixtures from a paired iPhone: list, details, a locked-device error) | `ios::tests::parses_devicectl_output` | pass |
| `cargo clippy -p extend-hosted --all-targets -- -D warnings`; `cargo check --target x86_64-pc-windows-msvc -p extend-hosted` | | clean |

Not verified (no hardware here):

- **Physical iPhone/iPad**: `prepare ios-runner` install (needs signing), commands over CoreDevice,
  and the probe steps against a device being trusted or having Developer Mode turned on. The probe's
  parsing was checked against real `devicectl` output only.
- **Real Samsung and LG TVs**: the TLS paths (8002, 3001) and the real prompts. Protocol details
  follow samsungtvws and aiowebostv; only mocks were exercised.
- **Real Apple TV**: pairing with a real tvOS, and every AirPlay step (`/photo` on current tvOS is
  the least certain; transient pairing needs AirPlay access set to "Anyone on the Same Network",
  otherwise the Companion credentials are tried).
