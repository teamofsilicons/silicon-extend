# Silicon Bridge: technical documentation

> Contract file. Agents: propose changes to a Carbon and get approval before editing.
> Product intent lives in `UNDERSTANDING.md`; wire contracts live in `api.yaml` and `cli.yaml`.
> Where this file and `UNDERSTANDING.md` disagree, `UNDERSTANDING.md` wins and this file is corrected.

Status: **draft for Carbon review, 2026-09-26; implemented the same night.** Section 13 lists where
the build differs from the first draft and why. Every value below that
`UNDERSTANDING.md` does not state is a proposal, and each one is listed again in
[Open questions](#open-questions) so it can be confirmed or changed.

---

## 1. Identifiers and values

Every identifier, token and bounded value that crosses a wire. Regexes are anchored. "World"
means one isolated data plane: production, or one Honeycomb test environment.

### Bridge-issued identifiers

| Name | Example | Format | Lifetime and uniqueness |
|---|---|---|---|
| `pairing_code` | `4F9C2A` | `^[0-9A-F]{6}$`, 6 hexadecimal characters. Shown uppercase; accepted in any case (`4f9c2a` is the same code). | Valid for 300 s, then rotated. Works once. One live code per enrollment. Unique among live codes across **all** worlds, because the device doesn't know which world it will be paired into until the Carbon claims the code. |
| `enrollment_id` | `01926f3a-5c1e-7b2d-9a40-3e5f8c7d1b22` | UUIDv7 | One per unpaired app install. Ends when the device pairs or the app discards it. |
| `enrollment_secret` | `bes_Q2hh…` (47 chars) | `^bes_[A-Za-z0-9_-]{43}$`, 32 random bytes base64url | Held only by the unpaired app. Proves it owns the enrollment. Stored hashed (SHA-256). |
| `device_id` | `7c1e09ab` | `^[0-9a-f]{8}$`, 8 lowercase hexadecimal characters, random | Unique within its world. Never reused, even after removal. |
| `device_credential` | `bdc_x9Lk…` (47 chars) | `^bdc_[A-Za-z0-9_-]{43}$`, 32 random bytes base64url | Issued once when pairing completes. Valid until the pair ends. Stored hashed (SHA-256). On the device it lives in the OS secret store (Keychain, Android Keystore, DPAPI, libsecret). |
| `session_id` | `a3f` | `^[0-9a-f]{3,}$`, lowercase hexadecimal, 3 characters to start | Unique within its world and **never reused**. Allocation picks at random among the unused ids of the shortest length that still has any left. When all 4,096 three-character ids are used, new ids have 4 characters (65,536), then 5, and so on. |
| `command_id` | UUIDv7 | UUIDv7 | One per command sent into a session. Also the activity-log entry id for that command. |
| `request_id` | UUIDv7 | UUIDv7 | One per Silicon-to-Silicon request. |
| `activity_id` | UUIDv7 | UUIDv7 | One per activity-log entry that isn't a command (session started, access granted, pair revoked…). |
| `upload_id` | UUIDv7 | UUIDv7 | One per file a device uploads to Bridge on its way to Briefcase. Single use. |
| `takeover_id` | UUIDv7 | UUIDv7 | One per takeover hold. |
| Device `version` | `7` | Integer ≥ 1 | Increments on every change to a device's settings. Sent as `ETag` and required as `If-Match` on changes. |

### Values Bridge bounds

| Name | Type and range | Default |
|---|---|---|
| Device `name` | 1–64 Unicode scalar values after trimming. No control characters. Not unique. | Set by the Carbon at pairing |
| Device `visibility` | `team` or `personal` | `team` |
| `pair_ttl_days` | Integer 1–30. Days without activity before the pair ends on its own. | `14` |
| Session idle timeout | Fixed 300 s after the last command finished, or after the session started if no command was sent | 300 s |
| Request `reason` | 1–300 Unicode scalar values after trimming. Delivered exactly as sent. | required |
| File self-destruct | 1 minute to 30 days (43,200 minutes), in whole minutes | 1 day (1,440 minutes) |
| `timeout_ms` on a command | Integer 1,000–300,000 | 30,000 |
| Test environment paired devices | At most 5 per environment | — |
| Active test environments | At most 10 across the whole deployment | — |
| Page `limit` | Integer 1–100 | 50 |

### Identifiers owned by other services

Bridge stores and passes these. It never mints or parses beyond the prefix rules below.

| Name | Example | Format | Owner |
|---|---|---|---|
| Carbon id | `c:alice` | `c:` + IAM handle | Silicon IAM |
| Silicon id | `si:chef` | `si:` + IAM handle | Silicon IAM |
| Membership id | `c:alice[acme]` | `{member id}[{team handle}]` | Silicon IAM |
| Team handle (wire name `org_id`, header `X-Org-ID`) | `acme` | IAM handle. IAM names this an organization; Bridge keeps IAM's wire names. | Silicon IAM |
| `app_id` | `bridge` | Bare IAM application id. Treat as opaque. | Silicon IAM |
| `app_secret` | `ask_…` | `^ask_[A-Za-z0-9_-]{43}$` | Silicon IAM. A **test** app secret also selects the test environment (§8). |
| Short-lived token (SLT) | `oac_…` | Opaque. The field is named `slt`; never infer anything from its prefix. In a test environment an existing test member id (`c:alice`, `si:chef`) is also accepted. | Silicon IAM |
| Access token | `oat_…` | Opaque, starts `oat_` | Silicon IAM, via Bridge login |
| Refresh token | `ort_…` | Opaque, starts `ort_` | Silicon IAM, via Bridge login |
| `environment_id` (CLI calls it `test_id`) | `9b3e…` | UUID | Honeycomb. The same id across every service. |
| Honeycomb `testing_key` | 32 chars | `^[A-Za-z0-9]{32}$` | Honeycomb. Administrative control of the test world. Never used to select a world for normal use. |
| `operation_id` | UUID | UUID | Honeycomb lifecycle instruction |
| `environment_revision`, `generation`, `key_version` | `3` | Integer ≥ 1 | Honeycomb |
| `Idempotency-Key` | `b1f0c9d2-…` | `^[!-~]{8,255}$` | Caller. The CLI sends a UUIDv4. |
| IAM webhook signature | `v1=9f86…` | `^v1=[0-9a-f]{64}$`, HMAC-SHA256 over `{timestamp}.{exact body}` | Silicon IAM |
| Briefcase `entry_id` (Bridge calls it `file_id`) | UUID | UUID | Briefcase |
| Element ref | `@e12` | `^@e[0-9]+$` | agent-device. Valid until the next `snapshot` in the same session. |
| Selector | `role="button" label="Continue"` | agent-device selector expression | agent-device |

### Formats used everywhere

- Timestamps: RFC 3339, UTC, millisecond precision (`2026-09-26T10:04:12.391Z`).
- Durations on the wire are integers with the unit in the name: `_ms`, `_s`, `_minutes`, `_days`.
- Every JSON body is an envelope `{"type": "<kind>", "data": {...}}`. Errors are `{"type": "error", "data": {code, message, hint, docs_url, request_id, details}}`.
- Every response carries `X-Request-ID` (UUIDv7) and `Silicon-Bridge-API-Version`.

---

## 2. Architecture

```
 Silicon ──bridge CLI──┐                                 ┌── Bridge app (Android, Android TV, Mac, Windows, Linux)
 Carbon ──website──────┤  HTTPS / JSON                   │     └── agent-device (forked) operates the device
 other apps ─client────┴──►  Bridge service  ◄── WSS ────┤
                              │   │    │    │            └── Bridge app on a Mac or computer, acting as host
                              │   │    │    │                  └── iPhone, iPad, Apple TV, Samsung/LG TV
                              │   │    │    └── Space Station (telemetry)
                              │   │    └── Ting (requests between Silicons)
                              │   └── Briefcase (files, through OBO)
                              └── Silicon IAM (login, live authorization, webhooks)
```

Parts, matching `UNDERSTANDING.md`:

| Part | Tech | Where |
|---|---|---|
| Bridge service | Rust, `axum` + `tokio`, PostgreSQL through `sqlx`, official `silicon-iam-client` crate | `backend.bridge.teamofsilicons.com` |
| Configuration website | Uses the Bridge client over the same public API. A subset of the CLI. | `bridge.teamofsilicons.com` |
| Bridge client | Rust crate `silicon-bridge-client`, stateless | crates.io |
| `bridge` CLI | Rust, built only on `silicon-bridge-client`, keeps its state on disk | `honeycomb install 'bridge'` |
| Bridge apps | Per OS, see §7. Each embeds the forked agent-device. | Downloads on the website |

The device and the Silicon never talk directly. Both connect outward to the Bridge service, so
neither needs to be reachable from the internet and they never need to share a network.

### Why a relay and not agent-device's own remote mode

agent-device can already run against a daemon on another machine. That mode expects the client to
reach the daemon's address, which personal devices behind home routers can't offer. Bridge
inverts it: the app keeps one outbound WebSocket open, and the service pushes commands down it.
The same connection also carries the stop button, the in-use indicator and revocation, which have
to reach the device immediately and can't wait for the device to poll.

---

## 3. Data model

One PostgreSQL schema per world: `bridge` for production and `bridge_test_<environment_id without dashes>`
for each test environment. The same migrations run in each. Cleaning a test environment truncates
its schema; permanent removal drops it. Keeping worlds in separate schemas makes it impossible for a
missing `WHERE` clause to leak data between them.

The two tables that must span worlds live in `bridge_global`:

- `enrollments` and live `pairing_codes`, because the world is only chosen when the code is claimed.
- `test_environments`: id, name, state, revision, generation, key version, and the IAM app binding.

Tables per world:

| Table | Holds |
|---|---|
| `devices` | `device_id`, team handle, owner Carbon id, name, OS, kind, visibility, `pair_ttl_days`, `last_activity_at`, `paired_at`, `version`, `host_device_id` (for devices paired through a computer), setup state, app version, credential hash |
| `device_access` | (`device_id`, Silicon id), granted by, granted at |
| `device_locks` | `device_id` primary key → `session_id`. The unique key is what enforces one Silicon at a time. |
| `session_ids` | Every `session_id` ever allocated, so none is reused |
| `sessions` | `session_id`, `device_id`, Silicon id, state, started, last command, ended, end reason |
| `commands` | `command_id`, `session_id`, command name, redacted input, outcome, timings, file ids |
| `activity` | Non-command events on a device |
| `requests` | `request_id`, device, from Silicon, to Silicon, reason, Ting delivery state |
| `files` | Briefcase `entry_id`, session, kind, self-destruct time, permanent flag |
| `iam_events` | IAM webhook event ids, for de-duplication, plus the latest aggregate version applied |
| `honeycomb_operations` | Lifecycle receipts (§8) |

---

## 4. Pairing

```
 Bridge app                        Bridge service                         Website (Carbon)
 ──────────                        ──────────────                         ────────────────
 POST /enrollments ──────────────► creates enrollment + code
          ◄──── enrollment_id, enrollment_secret, pairing_code
 opens WSS /enrollments/{id}/connect
          ◄──── new code every 300 s
                                                   ◄──── POST /pairings {pairing_code, name}
                                   checks code, team, test limit
                                   creates device in the Carbon's team and world
          ◄──── "paired" {device_id, device_credential, world}
 stores credential, drops enrollment
 opens WSS /device/connect ──────► device online
 runs setup steps, reports each ─► setup state shown on the website
```

- The code is 6 hexadecimal characters, so 16,777,216 values. Guessing is limited by rate: a Carbon
  gets 5 failed claims per 10 minutes, and an IP address 30 per hour. A failed claim never says
  whether the code existed.
- A claim is atomic. Two Carbons entering the same code at once: one wins, the other gets
  `pairing_code_invalid`.
- Pairing in a test environment happens exactly the same way while the website or CLI is in that
  environment. If the environment already has 5 devices the claim fails with
  `In test environment you are limited to 5 paired devices per environment.`
- A device belongs to one world at a time. Pairing it elsewhere means revoking the pair first.
- iPhone, iPad, Apple TV and smart TVs pair through a host: the Carbon picks a paired Mac or
  computer on the website, Bridge creates the new device in `setup` with `host_device_id` set, and
  the host's app walks the setup (§7).

### When a pair ends

| Cause | Trigger |
|---|---|
| Revoked on the device | `DELETE /api/v1/device` from the app |
| Removed on the website or CLI | `DELETE /api/v1/devices/{device_id}` by the owner |
| No activity | `last_activity_at + pair_ttl_days` passes. A scheduler checks every minute. |
| Owner leaves the team | IAM webhook, then confirmed by introspection |

Activity means a command in a session, or the owner changing the device's settings. A device
that's merely online doesn't count.

When a pair ends, Bridge ends any session with the matching reason, removes all access, sends
`unpaired` down the device's connection, invalidates the credential, and keeps the device row and
activity log (marked removed) so the log stays readable.

---

## 5. Sessions and commands

### Lifecycle

```
bridge session new 7c1e09ab   → POST /sessions            → lock taken, session "active", device shows the Silicon
bridge session connect a3f    → GET  /sessions/a3f        → CLI remembers a3f and caches the device's capabilities
bridge snapshot -i            → POST /sessions/a3f/commands {"command":"snapshot","args":["-i"]}
bridge click @e2              → POST /sessions/a3f/commands {"command":"click","args":["@e2"]}
bridge session end a3f        → POST /sessions/a3f/end    → lock released, device indicator cleared
```

Session states: `active` → `paused` (during a takeover) → `active` → `ended`.
End reasons: `ended_by_silicon`, `idle_timeout`, `stopped_by_carbon`, `access_removed`,
`device_removed`, `pair_revoked`, `pair_expired`, `silicon_logged_out`, `left_team`,
`device_offline`, `environment_disabled`, `environment_cleaned`.

### One Silicon at a time

`device_locks` has the device id as its primary key. Starting a session inserts a row in the same
transaction that creates the session, so a second Silicon gets `409 device_in_use` with the current
holder's Silicon id, session start time and the `bridge request send` command to ask for the device.

### Relaying a command

1. The CLI posts `{command, args, timeout_ms}` with the Silicon's access token. `args` are the
   command-line tokens after the name, exactly as agent-device's CLI takes them.
2. The service introspects the token with IAM (cached no longer than 30 s, and dropped immediately
   on a relevant IAM webhook), then checks: the session is the caller's, it is `active`, the caller
   still has access, the device is online, and the command is in the device's capabilities.
3. The service sends `{"type":"command","id":<command_id>,"timeout_ms":…,"command":…,"args":[…],"upload_ids":[…]}`
   down the device's WebSocket and waits for the matching `result`.
4. The app runs it through agent-device and replies. Files it produced (screenshots, recordings,
   logs, replay scripts) are uploaded to `PUT /api/v1/device/artifacts/{upload_id}` first, using
   upload ids the service included with the command.
5. The service stores each file in Briefcase on the Silicon's behalf (§6), writes the command to
   the activity log, resets the idle timer, and returns the result with Briefcase links.

Commands in one session run one at a time, in order. A second command sent while one is in flight
waits for it; this matches agent-device, whose own daemon serialises a session.

If the deadline passes, the service answers `504 command_timeout` and tells the device to cancel.
If the device drops mid-command, the answer is `503 device_offline`, and the command's outcome is
logged as unknown, because it may have run.

### Ending on its own

- **Idle:** 300 s after the last command finished (or the start, with no commands). A command in
  flight holds the timer.
- **Offline:** if the device stays disconnected for 120 s during a session, the session ends with
  `device_offline`, so a device that loses power doesn't stay locked.
- **Revocation:** see §9.

### Takeover

A Silicon hands the device to the Carbon (for Face ID, a payment, or an admin prompt) with
`bridge takeover --reason "..."`. The session becomes `paused`: commands are refused with
`423 session_paused`, the device and website show the reason and a "Done" button, and the idle
timer stops for up to 30 minutes. The Carbon's "Done" or `bridge takeover release` resumes it.
After 30 minutes paused, the session ends with `idle_timeout`.

### What the CLI shows in `--help`

On `session connect`, the CLI saves the device's `capabilities` list next to the session. While
that session is connected, `bridge --help` lists only the commands whose capability is in that
list. The same list is what the service checks in step 2, so help and enforcement can't disagree.

Capabilities:

| Capability | Commands | Android | Android TV / Fire OS | Mac | Windows | Linux | iPhone / iPad | Apple TV | Samsung / LG TV |
|---|---|---|---|---|---|---|---|---|---|
| `screen.read` | `snapshot`, `diff snapshot`, `get`, `find`, `is`, `wait` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| `screen.capture` | `screenshot`, `diff screenshot` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| `screen.record` | `record start`, `record stop` | ✓ | — | ✓ | ✓ | ✓ | ✓ | — | — |
| `input.touch` | `click`, `press`, `longpress`, `swipe`, `scroll`, `gesture` | ✓ | — | — | — | — | ✓ | — | — |
| `input.pointer` | `click`, `press`, `hover`, `scroll` | — | — | ✓ | ✓ | ✓ | — | — | — |
| `input.text` | `fill`, `type`, `focus` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| `input.keyboard` | `keyboard` | ✓ | — | — | — | — | ✓ | — | — |
| `input.remote` | `tv-remote` | — | ✓ | — | — | — | — | ✓ | ✓ |
| `nav.system` | `back`, `home`, `app-switcher` | ✓ | ✓ | — | — | — | ✓ | ✓ | ✓ |
| `apps.launch` | `open`, `close`, `appstate` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `apps.list` | `apps` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `apps.install` | `install`, `reinstall` | ✓ | ✓ | — | — | — | — | — | — |
| `alerts` | `alert` | ✓ | ✓ | ✓ | ✓ | — | ✓ | — | — |
| `clipboard` | `clipboard` | ✓ | — | ✓ | ✓ | ✓ | — | — | — |
| `logs` | `logs` | ✓ | ✓ | ✓ | ✓ | ✓ | — | — | — |
| `replay` | `replay`, `test`, `batch` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `takeover` | `takeover` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `notifications` | `notifications` | ✓ | — | — | — | — | — | — | — |
| `adb` | `adb` | ✓ | ✓ | — | — | — | — | — | — |
| `terminal` | `terminal` | — | — | ✓ | ✓ | ✓ | — | — | — |
| `display` | `display` | — | ✓ | — | — | — | — | ✓ (pictures, videos) | — |
| `links` | `open <url>` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |

A Linux computer without a screen reports only `terminal`, `apps.launch` and `replay`. The final
list is whatever the device's app reports in its `hello` message, intersected with this table, so
a device missing a permission (say, Screen Recording on a Mac) simply lacks that capability until
the Carbon grants it.

### agent-device commands Bridge does not expose

The parts made for app developers: `boot`, `shutdown`, `web …`, `viewport`, `react-native …`,
`react-devtools …`, `metro …`, `cdp …`, `perf …`, `trace …`, `network dump`, `debug symbols`,
`audio probe`, `push`, `trigger-app-event`, `install-from-source`, `settings …` (simulator helpers),
`fold`, `orientation`, `action-button`, `prepare ios-runner`, `mcp`, `doctor`. Discovery and connection are replaced by Bridge's own commands:
`devices` → `bridge device ls`, `connect`/`disconnect` → `bridge session connect`/`disconnect`,
`session list` → `bridge session ls`. Any of them sent through Bridge returns
`404 unknown_command` naming the Bridge replacement where there is one.

---

## 6. Files

Every file a command produces is stored in Briefcase for the Silicon, through Briefcase's OBO
endpoints:

1. Bridge exchanges the Silicon's access token with IAM for an OBO proof bound to
   `briefcase.files.create`, the SHA-256 of the exact bytes, and metadata
   `{path: "", name, content_type}`. An empty path puts the file in Bridge's private folder for that
   Silicon: `apps/bridge/private/{silicon id}/`.
2. Bridge sends the bytes to Briefcase `POST /api/v1/obo/files`.
3. Bridge shares the file with the device owner Carbon (create, read, update, not delete) through
   `POST /api/v1/obo/invitations`, which Briefcase classes as critical, so it needs Briefcase's
   approval of Bridge once, in Honeycomb.
4. Bridge records the `entry_id`, and returns the Briefcase permanent URL,
   `https://briefcase.teamofsilicons.com/org/{team}/…`, to the CLI.

Self-destruct defaults to 1 day. `--ttl` on the command that makes the file sets 1 minute to 30
days. `bridge file keep <file_id>` makes a file permanent before it goes.

In a test environment every Briefcase call uses Briefcase's test environment with the same
`environment_id`. **See Open questions 1–3:** Briefcase's OBO endpoints don't currently take a
self-destruct time or offer "make permanent".

Sizes: a single file up to 1 GiB. Recordings stop at 1 GiB or 30 minutes, whichever comes first.

---

## 7. The Bridge apps

Every app does four things: keep the WebSocket to the service open, run commands through
agent-device, show the in-use indicator with a Stop button, and offer "Revoke pair". It starts at
boot and reconnects on its own.

### Connection protocol (`WSS /api/v1/device/connect`)

- Auth: `Authorization: Bridge-Device <device_credential>` on the upgrade request.
- Frames are flat JSON text: `{"type": ..., <fields>}` (docs/device-protocol.md). Files never travel on the socket.
- Heartbeat: the service pings every 15 s; the device is **offline** after 45 s without a pong.
- Reconnect: exponential backoff from 1 s to 60 s with full jitter.
- One live connection per device. A new connection replaces the old one, which gets `superseded`.

| Direction | `type` | Meaning |
|---|---|---|
| device → service | `hello` | App version, OS and version, agent-device version, capabilities, setup state |
| device → service | `setup_progress` | One setup step changed |
| device → service | `result` | Answer to a `command`, same `id` |
| device → service | `stop` | The Carbon tapped Stop |
| device → service | `takeover_done` | The Carbon tapped Done on a takeover |
| service → device | `command` | Run this, with `deadline` and `upload_ids` |
| service → device | `cancel` | Stop the command with this `id` |
| service → device | `session_started` / `session_ended` | Show or clear the indicator (Silicon id, since) |
| service → device | `takeover` | Show the takeover reason and Done button |
| service → device | `display` | TV: show or clear a link, image, video or text |
| service → device | `unpaired` | Forget the credential, return to the pairing screen |
| service → device | `environment` | Test environment name for the banner, or `null` for production |
| service → device | `superseded` | Another connection took over; close without reconnecting |

### Per device

| Device | How commands are carried out | Indicator |
|---|---|---|
| **Android phone and tablet** | The app connects to the device's own ADB over wireless debugging (loopback, paired once through Android's pairing code), then runs agent-device's Android driver, which installs its helper apps for the accessibility tree and text input on first use. | Foreground-service notification with Stop |
| **Android TV, Google TV, Fire OS** | Same, over network debugging on the TV's own ADB port. The display screen is an activity inside the app. | Overlay badge in a corner (needs "display over other apps") |
| **Mac** | agent-device's macOS driver: Accessibility API for the element tree and input, ScreenCaptureKit for screenshots and recording. Terminal commands run in a PTY as the logged-in user. | Menu bar icon changes; banner; Stop in the menu |
| **Windows** (built by Bridge) | UI Automation for the element tree, `SendInput` for mouse and keyboard, Windows.Graphics.Capture for screenshots, Media Foundation for recordings, ConPTY for the terminal. Mapped onto the same agent-device command set and snapshot shape. | Tray icon changes; banner with Stop |
| **Linux** | AT-SPI2 for the element tree. On Wayland, the XDG desktop portal's RemoteDesktop and ScreenCast (the one-time "screen sharing and remote control" approval) with PipeWire; on X11, XTest and XShm. PTY for the terminal. | Banner with Stop |
| **iPhone, iPad** (via Mac) | The Mac's app runs agent-device's physical-iOS driver: its XCTest runner is installed on the iPhone once over USB, then reached over Wi-Fi. | On the Mac's app and the website |
| **Apple TV** (via Mac) | The Companion protocol for apps and remote buttons, and AirPlay for pictures and videos, from the Mac on the same network. The Apple TV shows a code the Carbon enters once. | On the Mac's app and the website |
| **Samsung TV** (via computer) | Tizen's local remote-control WebSocket (ports 8001/8002). The TV asks the Carbon to allow the connection once and issues a token. | On the host's app and the website |
| **LG TV** (via computer) | webOS's local SSAP WebSocket (ports 3000/3001). The TV asks the Carbon to accept once and issues a client key. | On the host's app and the website |

A device paired through a host is offline whenever its host is offline or can't reach it.

---

## 8. Test environments

- **Selecting one:** the caller sends the test application's `app_secret` in
  `X-Testing-Application-Secret`. Bridge asks IAM which environment that secret belongs to (IAM
  `testing_context`), maps it to its own world, and refuses with `401 testing_secret_invalid` if
  the secret is invalid, revoked, or belongs to an environment that isn't active. It never falls
  back to production.
- **Signing in:** `POST /api/v1/auth/login` in a test environment accepts an IAM-issued test SLT or
  an existing test member id (`c:alice`, `si:chef`). Unknown or inactive ids are refused. In
  production the member-id form is refused outright, before IAM is asked.
- **Permissions:** a test session is a normal session for that member. The secret selects the
  world; it grants nothing.
- **Isolation:** its own PostgreSQL schema; its own session-id sequence; Ting's and Briefcase's test
  environments with the same `environment_id`; no real email or push; the Honeycomb `testing_key`
  never used for normal access.
- **The device app** gets `environment` on its socket and shows a permanent banner with the name.

### Honeycomb lifecycle

`PUT /internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}`,
authenticated with Honeycomb's service credential, with the same body and receipt shape the other
services use. Actions: `prepare`, `rotate-key`, `clean`, `disable`, `restore`, `purge`.

| Action | Bridge does |
|---|---|
| `prepare` | Creates the schema, records the environment as `preparing`. Test access opens only after Honeycomb confirms every service is ready and IAM reports the environment ready. |
| `rotate-key` | Records the new `key_version`. |
| `clean` | Blocks access, ends sessions, sends `unpaired` to every device in the world, truncates the schema, bumps `generation`. Completes only when all of that is done. Requests, jobs and webhooks carrying an older revision or generation are dropped. |
| `disable` | Blocks access immediately and ends every running session. Data is kept. |
| `restore` | Makes retained data available again; needs a free slot of the 10. Can't undo a clean. |
| `purge` | Drops the schema and forgets the environment. |

Each receipt is `pending`, `completed` or `failed`, repeating the identical instruction returns the
stored receipt, and a changed instruction under the same `operation_id` is `409`. These work even
while the environment is disabled. Bridge reports the environment's last activity to Honeycomb;
Honeycomb, not Bridge, decides when it expires.

---

## 9. Revocation

Anything that ends access takes effect on the next command at the latest, and usually before it:

| Event | Source | Effect |
|---|---|---|
| Owner removes a Silicon's access | API | That Silicon's session on the device ends at once (`access_removed`) |
| Device removed or pair revoked | API | Session ends; pair ends (§4) |
| Carbon taps Stop | Device socket or API | Session ends (`stopped_by_carbon`) |
| Silicon logs out, or its IAM session is revoked | IAM webhook | Its sessions end (`silicon_logged_out`) |
| Silicon removed from the team or deleted | IAM webhook | Its sessions end (`left_team`); its access grants are removed |
| Owner Carbon leaves the team | IAM webhook | All the Carbon's devices in that team are unpaired |

Webhooks are verified by the official IAM client over the exact raw body, de-duplicated on event id,
and applied only if their aggregate version is newer than the last one applied. A webhook is a
prompt, not a decision: Bridge confirms with IAM introspection before acting, and re-checks on
every command, so a lost webhook delays nothing.

---

## 10. Versioning

- The API is versioned in the path (`/api/v1`). Breaking changes get a new major; additive
  changes don't.
- **Negotiation:** the client sends `Silicon-Bridge-Supported-API-Versions: 1, 2` to
  `GET /api/version`. The service answers with the highest major both support, in the body and in
  `Silicon-Bridge-API-Version`. The client pins that version for its lifetime. A request whose pin
  disagrees with its path gets `400 api_version_mismatch`. The client crate does this when it's
  built, so the CLI never runs against an incompatible service.
- **Compatibility matrix:** `GET /api/v1/contracts` lists every API major with its state
  (`current`, `deprecated`, `sunset`), and the client, CLI and app versions that work with it.
- **Device apps** report their version in `hello`; the service refuses one below the minimum with
  close code `4426 upgrade_required`, and the app shows an update prompt.
- **Deprecation:** a deprecated major answers with `Deprecation` and `Sunset` headers. It is sunset
  after **7 consecutive days with zero requests**, then answers `410 api_version_sunset`.
- **Consumer-driven contract tests:** the client crate, CLI and each app publish the requests they
  make as contract fixtures; the service's CI replays every fixture of every supported version.

---

## 11. CLI and client internals

- **Home:** `$SILICON_HOME` if set, else `~`. State lives in `{home}/.bridge/`. `bridge config home <dir>`
  moves it; it refuses a path that isn't an existing directory (`not a directory: <path>`). The
  chosen location is recorded in `{default home}/.bridge/home` so later runs find it.
- **Files** (all `0600`, directory `0700`):
  `auth.json` (tokens, team), `config.toml`, `sessions/current`, `sessions/{session_id}.json`
  (device, capabilities), `test/{environment_id}.json` (test app secret and that world's tokens).
- **Token refresh** is serialised with an advisory file lock on `auth.json`, and the replacement
  pair is written atomically (write temporary, `fsync`, rename), as IAM's client docs require.
- **No daemon** is needed: each command is one HTTPS request. See Open question 7.
- **Session selection**, first match wins: `--session <id>`, then `BRIDGE_SESSION`, then the
  connected session.
- **Output:** text by default; `--json` gives exactly one JSON document on stdout. Progress,
  warnings and the test-environment line go to stderr so stdout stays safe for scripts and binary
  output.
- **Exit codes:** listed in `cli.yaml`. The same code always means the same kind of failure.

---

## 12. Telemetry, reports, logs

- Telemetry goes to Space Station, on by default, off with `bridge config set telemetry off` or the
  website's settings. The CLI posts events to `POST /api/v1/telemetry` so no ingest key ships in
  the CLI. Each event carries source (`cli`, `client`, `app`, `service`, `web`), step, outcome,
  duration, command name, device OS, and the session and command ids. Never typed text, clipboard
  contents, screen contents, tokens, codes or secrets.
- `bridge report "<message>" [--pr <url>]` stores the report and emails it through Postmark to the
  three addresses in `UNDERSTANDING.md`. In a test environment the email is simulated.
- **Redaction in the activity log:** text typed with `fill` and `type`, and text written with
  `clipboard write`, is replaced with `[redacted N chars]`. Everything else is logged as sent.
- Secrets never appear in URLs, logs, audit rows, telemetry or stored webhook bodies. Credentials and
  enrollment secrets are stored as SHA-256 digests. Pairing codes are stored in plain text: the
  unpaired app has to be able to fetch its current code by polling, a code lives 5 minutes, works
  once, and guessing is rate limited.

---

## 13. As built (2026-09-26)

Differences from the first draft, each deliberate:

- **Commands are CLI tokens, not structured input.** A command is `{command, args}` where `args` are
  agent-device's command-line tokens. agent-device's structured inputs differ per command and change
  between versions; forwarding tokens keeps its own parser the authority. Flags that pick a device or
  session inside agent-device are refused. Files a caller sends (a replay script, an image for a TV)
  travel as `attachments` and are referenced in args as `attachment:<name>`.
- **Self-destruct is enforced by Bridge.** Briefcase's delegated upload takes no self-destruct time,
  so Bridge records the time, deletes the file through Briefcase's delegated trash when it passes,
  and `bridge file keep` cancels that. Open questions 1–2 still stand for Briefcase itself.
- **One service instance.** Device sockets and waiting commands live in the process
  (`docs/operations.md`).
- **Android reads the screen with an AccessibilityService**, not agent-device's helper over
  on-device ADB: an app can't drive its own device's ADB without pairing tricks, and the
  accessibility tree gives the same element list. See `apps/android/README.md` for what uses
  wireless debugging.
- **Local stand-ins** for Silicon IAM, Briefcase and Ting (`BRIDGE_IAM_MODE=local` etc.) exist for
  development and tests and are refused in production.
- **Extra endpoints:** `GET /api/v1/team/silicons` (access picker), `iam_login_url` in
  `GET /api/v1/iam`, and `input` on setup steps (`"code"` for an Apple TV).
- **ISI:** when the CLI runs with `ISI` set, it's sent as `X-Silicon-ISI` and recorded with session
  starts and commands in the activity log. Nothing depends on it.
- **Telemetry** goes to each world's outbox table and is exported to Space Station when
  `BRIDGE_SPACE_STATION_KEY` (and, per test environment, `BRIDGE_TEST_TELEMETRY_KEYS`) is set.

## Open questions

Proposals in this file that `UNDERSTANDING.md` doesn't settle, or where it conflicts with another
service. Each needs a Carbon's decision.

1. **Briefcase self-destruct through OBO.** Briefcase's OBO upload (`/obo/files`) takes only `path`,
   `name` and `content_type`. Self-destruct (`self_destruct_minutes`) exists only on its direct
   upload. Bridge needs it on the OBO path.
2. **Making a file permanent through OBO.** Briefcase has no OBO endpoint for it. Its own rule, that
   only the creator, admins and owners can make a file permanent, fits Bridge acting for the
   creating Silicon, but the endpoint is missing.
3. **Where files live.** `UNDERSTANDING.md` says "the private folder of the Silicon". Briefcase OBO
   can only write inside the app's own folder, so files land in `apps/bridge/private/{silicon id}/`.
   Is that acceptable?
4. **"Logging out … ends it immediately."** Read here as the *Silicon* logging out. Ending every
   Silicon's access when the owning *Carbon* logs out of the website would break sessions each
   time a browser signs out. Confirm.
5. **Team-visible devices.** Proposed: other Carbons in the team see the device's name, OS, owner
   and whether it is online, read-only. They can't grant access or see the activity log.
6. **Can Carbons start sessions?** Proposed: no; sessions are for Silicons, as `UNDERSTANDING.md`
   describes them. Carbons manage devices.
7. **Daemon.** The house style suggests a CLI daemon. Bridge doesn't need one for correctness;
   proposed to ship without it and add one only if per-command connection setup proves slow.
8. **Values not in `UNDERSTANDING.md`:** `device_id` as 8 hexadecimal characters; pairing-code rate
   limits (5 per Carbon per 10 min, 30 per IP per hour); offline session end after 120 s; takeover
   pause up to 30 min; command timeout 30 s by default and 300 s at most; 1 GiB file and
   30-minute recording caps; activity-log redaction of typed text.
9. **Activity-log retention.** Not stated. Proposed: keep for the life of the device plus 90 days
   after it is removed.
10. **Physical Fire TV.** `UNDERSTANDING.md` says Bridge adds it. agent-device's Fire TV support is
    limited to Amazon's virtual device, so screen reading on a physical Fire TV depends on Bridge's
    fork of the Android helper working on Fire OS. Needs a hardware check early.
11. **iPhone signing.** agent-device's physical-iOS runner is an XCTest app that must be signed with
    an Apple development team and installed from a Mac with Xcode tools. The setup guide needs to
    say which Apple account signs it: the Carbon's own, or a team one.
