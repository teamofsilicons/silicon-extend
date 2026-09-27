# Silicon Extend: technical documentation

> Contract file. Agents: propose changes to a Carbon and get approval before editing.
> Product intent lives in `UNDERSTANDING.md`; wire contracts live in `api.yaml` and `cli.yaml`.
> Where this file and `UNDERSTANDING.md` disagree, `UNDERSTANDING.md` wins and this file is corrected.

Status: **draft for Carbon review, 2026-09-26; implemented the same night.** Section 13 lists where
the build differs from the first draft and why. Every value below that
`UNDERSTANDING.md` does not state is a proposal, and each one is listed again in
[Open questions](#open-questions) so it can be confirmed or changed.

On 2026-09-27 the as-built parts were brought up to date with the implementation: the Team handle
row (§1), file naming and sharing (§6), `cancel` (§7), the per-device table (§7), section 13 and
open questions 12–15. Those edits describe what was built; they await a Carbon's review like the
rest of this file.

Later on 2026-09-27, after the second round of fixes (an audit of the build against
`UNDERSTANDING.md`, then fixes in the service, CLI, website and apps, each checked by a separate
verifier), the as-built parts were updated again: request reasons and the idle timer (§1, §5),
world-bound pairing codes, removed devices and the device limit (§4), sessions ending mid-command
and on a refused login (§5, §9), file downloads, storage warnings and self-destruct (§6), close
code 4503 (§7), readiness, the lifecycle rules and the clean fence (§8), webhook ordering (§9),
versioning as built (§10), section 13, and the open questions. [Open questions](#open-questions)
now starts with the decisions a Carbon still has to make.

---

## 1. Identifiers and values

Every identifier, token and bounded value that crosses a wire. Regexes are anchored. "World"
means one isolated data plane: production, or one Honeycomb test environment.

### Extend-issued identifiers

| Name | Example | Format | Lifetime and uniqueness |
|---|---|---|---|
| `pairing_code` | `4F9C2A` | `^[0-9A-F]{6}$`, 6 hexadecimal characters. Shown uppercase; accepted in any case (`4f9c2a` is the same code). | Valid for 300 s, then rotated. Works once. One live code per enrollment. Unique among live codes across **all** worlds. As built, a code pairs only into the world its enrollment was started in (§4). |
| `enrollment_id` | `01926f3a-5c1e-7b2d-9a40-3e5f8c7d1b22` | UUIDv7 | One per unpaired app install. Ends when the device pairs or the app discards it. |
| `enrollment_secret` | `ees_Q2hh…` (47 chars) | `^ees_[A-Za-z0-9_-]{43}$`, 32 random bytes base64url | Held only by the unpaired app. Proves it owns the enrollment. Stored hashed (SHA-256). |
| `device_id` | `7c1e09ab` | `^[0-9a-f]{8}$`, 8 lowercase hexadecimal characters, random | Unique within its world. Never reused, even after removal. |
| `device_credential` | `edc_x9Lk…` (47 chars) | `^edc_[A-Za-z0-9_-]{43}$`, 32 random bytes base64url | Issued once when pairing completes. Valid until the pair ends. Stored hashed (SHA-256). On the device it lives in the OS secret store (Keychain, Android Keystore, DPAPI, libsecret). |
| `session_id` | `a3f` | `^[0-9a-f]{3,}$`, lowercase hexadecimal, 3 characters to start | Unique within its world and **never reused**. Allocation picks at random among the unused ids of the shortest length that still has any left. When all 4,096 three-character ids are used, new ids have 4 characters (65,536), then 5, and so on. |
| `command_id` | UUIDv7 | UUIDv7 | One per command sent into a session. Also the activity-log entry id for that command. |
| `request_id` | UUIDv7 | UUIDv7 | One per Silicon-to-Silicon request. |
| `activity_id` | UUIDv7 | UUIDv7 | One per activity-log entry that isn't a command (session started, access granted, pair revoked…). |
| `upload_id` | UUIDv7 | UUIDv7 | One per file a device uploads to Extend on its way to Briefcase. Single use. |
| `takeover_id` | UUIDv7 | UUIDv7 | One per takeover hold. |
| Device `version` | `7` | Integer ≥ 1 | Increments on every change to a device's settings. Sent as `ETag` and required as `If-Match` on changes. |

### Values Extend bounds

| Name | Type and range | Default |
|---|---|---|
| Device `name` | 1–64 Unicode scalar values after trimming. No control characters. Not unique. | Set by the Carbon at pairing |
| Device `visibility` | `team` or `personal` | `team` |
| `pair_ttl_days` | Integer 1–30. Days without activity before the pair ends on its own. | `14` |
| Session idle timeout | Fixed 300 s after the last command finished, or after the session started if no command was sent. A command in flight holds it (until 300 s after its deadline). | 300 s |
| Request `reason` | 1–300 Unicode scalar values after trimming, and at most 1,000 with the whitespace around it (as built; Open question C3). Stored and delivered exactly as sent, whitespace included. | required |
| File self-destruct | 1 minute to 30 days (43,200 minutes), in whole minutes | 1 day (1,440 minutes) |
| `timeout_ms` on a command | Integer 1,000–300,000 | 30,000 |
| Test environment paired devices | At most 5 per environment | — |
| Active test environments | At most 10 across the whole deployment | — |
| Page `limit` | Integer 1–100 | 50 |

### Identifiers owned by other services

Extend stores and passes these. It never mints or parses beyond the prefix rules below.

| Name | Example | Format | Owner |
|---|---|---|---|
| Carbon id | `c:alice` | `c:` + IAM handle | Silicon IAM |
| Silicon id | `si:chef` | `si:` + IAM handle | Silicon IAM |
| Membership id | `c:alice[acme]` | `{member id}[{team handle}]` | Silicon IAM |
| Team handle (wire name `org_id`, header `X-Org-ID`) | `acme` | IAM handle. Extend keeps IAM's wire names `org_id` and `X-Org-ID` for it. | Silicon IAM |
| `app_id` | `extend` | Bare IAM application id. Treat as opaque. | Silicon IAM |
| `app_secret` | `ask_…` | `^ask_[A-Za-z0-9_-]{43}$` | Silicon IAM. A **test** app secret also selects the test environment (§8). |
| Short-lived token (SLT) | `oac_…` | Opaque. The field is named `slt`; never infer anything from its prefix. In a test environment an existing test member id (`c:alice`, `si:chef`) is also accepted. | Silicon IAM |
| Access token | `oat_…` | Opaque, starts `oat_` | Silicon IAM, via Extend login |
| Refresh token | `ort_…` | Opaque, starts `ort_` | Silicon IAM, via Extend login |
| `environment_id` (CLI calls it `test_id`) | `9b3e…` | UUID | Honeycomb. The same id across every service. |
| Honeycomb `testing_key` | 32 chars | `^[A-Za-z0-9]{32}$` | Honeycomb. Administrative control of the test world. Never used to select a world for normal use. |
| `operation_id` | UUID | UUID | Honeycomb lifecycle instruction |
| `environment_revision`, `generation`, `key_version` | `3` | Integer ≥ 1 | Honeycomb |
| `Idempotency-Key` | `b1f0c9d2-…` | `^[!-~]{8,255}$` | Caller. The CLI sends a UUIDv4. |
| IAM webhook signature | `v1=9f86…` | `^v1=[0-9a-f]{64}$`, HMAC-SHA256 over `{timestamp}.{exact body}` | Silicon IAM |
| Briefcase `entry_id` (Extend calls it `file_id`) | UUID | UUID | Briefcase |
| Element ref | `@e12` | `^@e[0-9]+$` | agent-device. Valid until the next `snapshot` in the same session. |
| Selector | `role="button" label="Continue"` | agent-device selector expression | agent-device |

### Formats used everywhere

- Timestamps: RFC 3339, UTC, millisecond precision (`2026-09-26T10:04:12.391Z`). As built, many are
  sent with microseconds (`2026-09-26T21:25:04.826326Z`); parse any RFC 3339 fraction.
- Durations on the wire are integers with the unit in the name: `_ms`, `_s`, `_minutes`, `_days`.
- Every JSON body is an envelope `{"type": "<kind>", "data": {...}}`. Errors are `{"type": "error", "data": {code, message, hint, docs_url, request_id, details}}`.
- Every response carries `X-Request-ID` (UUIDv7) and `Silicon-Extend-API-Version`.

---

## 2. Architecture

```
 Silicon ──extend CLI──┐                                 ┌── Extend app (Android, Android TV, Mac, Windows, Linux)
 Carbon ──website──────┤  HTTPS / JSON                   │     └── agent-device (forked) operates the device
 other apps ─client────┴──►  Extend service  ◄── WSS ────┤
                              │   │    │    │            └── Extend app on a Mac or computer, acting as host
                              │   │    │    │                  └── iPhone, iPad, Apple TV, Samsung/LG TV
                              │   │    │    └── Space Station (telemetry)
                              │   │    └── Ting (requests between Silicons)
                              │   └── Briefcase (files, through OBO)
                              └── Silicon IAM (login, live authorization, webhooks)
```

Parts, matching `UNDERSTANDING.md`:

| Part | Tech | Where |
|---|---|---|
| Extend service | Rust, `axum` + `tokio`, PostgreSQL through `sqlx`, official `silicon-iam-client` crate | `backend.extend.teamofsilicons.com` |
| Configuration website | Uses the Extend client over the same public API. A subset of the CLI. | `extend.teamofsilicons.com` |
| Extend client | Rust crate `silicon-extend-client`, stateless | crates.io |
| `extend` CLI | Rust, built only on `silicon-extend-client`, keeps its state on disk | `honeycomb install 'extend'` |
| Extend apps | Per OS, see §7. Each embeds the forked agent-device. | Downloads on the website |

The device and the Silicon never talk directly. Both connect outward to the Extend service, so
neither needs to be reachable from the internet and they never need to share a network.

### Why a relay and not agent-device's own remote mode

agent-device can already run against a daemon on another machine. That mode expects the client to
reach the daemon's address, which personal devices behind home routers can't offer. Extend
inverts it: the app keeps one outbound WebSocket open, and the service pushes commands down it.
The same connection also carries the stop button, the in-use indicator and revocation, which have
to reach the device immediately and can't wait for the device to poll.

---

## 3. Data model

One PostgreSQL schema per world: `extend` for production and `extend_test_<environment_id without dashes>`
for each test environment. The same migrations run in each. Cleaning a test environment truncates
its schema; permanent removal drops it. Keeping worlds in separate schemas makes it impossible for a
missing `WHERE` clause to leak data between them.

The two tables that must span worlds live in `extend_global`:

- `enrollments` and live `pairing_codes`, because the world is only chosen when the code is claimed.
- `test_environments`: id, name, state, revision, generation, key version, and the IAM app binding.

As built (2026-09-27), `extend_global` holds:

- `enrollments`, each with its live pairing code and the world it was started in (`world_schema`,
  `environment_id`; the constraint `enrollments_claimed_in_own_world` backs §4's rule).
- `test_environments`: the above plus `last_operation_id` (only the latest operation may be retried)
  and IAM's test webhook key digest (routes signed test deliveries after a restart).
- `honeycomb_operations`: lifecycle receipts (§8).
- `test_secret_bindings`: which environment a test secret was confirmed for, by digest, so a
  refused secret gets a precise answer while its environment is being restored.
- `api_versions` and `api_version_usage`: each API major's lifecycle and its requests per UTC day
  (§10).
- `schema_versions`, and `local_test_apps` for the local IAM stand-in.

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
| `iam_events` | IAM webhook event ids, for de-duplication |
| `iam_aggregates` | The latest version applied per IAM aggregate (as built; §9) |
| `idempotency`, `uploads`, `reports`, `telemetry` | Stored responses per `Idempotency-Key`, upload slots, bug reports, the telemetry outbox |

As built, lifecycle receipts (`honeycomb_operations`) live in `extend_global`, not per world. A
clean truncates every per-world table except `iam_events` and `iam_aggregates` (Open question C5),
including `session_ids`, so session ids can repeat across a clean.

---

## 4. Pairing

```
 Extend app                        Extend service                         Website (Carbon)
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
  computer on the website, Extend creates the new device in `setup` with `host_device_id` set, and
  the host's app walks the setup (§7).

As built (2026-09-27):

- **Only the per-Carbon limit exists.** A Carbon gets 5 failed claims per 10 minutes; the per-address
  limit of 30 per hour above was never built (Open question 8). New enrollments are limited to 60 per
  hour per client address, read from `X-Forwarded-For` only behind a proxy listed in
  `EXTEND_TRUSTED_PROXY_CIDRS`.
- **A code pairs only into its own world.** An enrollment records the world its request selected
  (an app started with a test environment's app secret, or none for production). A signed-in Carbon
  of a team who enters a live code from another world gets `404 pairing_code_invalid` naming the
  direction and what to do, limited to 20 such answers per Carbon per 15 minutes; everyone else gets
  the answer a code that doesn't exist gets, so the check reveals nothing before sign-in. Reading,
  discarding or connecting to an enrollment with another world's secret is `401
  testing_secret_invalid`. A clean or purge deletes the environment's waiting codes.
- **The test device limit is atomic.** Adds to one test environment (claims and attachments) take
  turns: in memory within the service process (so waiting holds no database connection), and
  through a transaction-scoped advisory lock across processes; the count is taken under the turn.
  Concurrent claims never exceed 5. An add that waits more than 10 s for its turn, or 5 s on the
  lock, is `429 rate_limited` with a hint to retry. A committed pairing always answers 201.
- **Removed devices stay readable to their Carbon.** The row, its activity log and requests stay,
  marked removed. The Carbon who paired it lists it with `GET /api/v1/devices?scope=mine&
  include_removed=true` (with `removed_at` and `removed_reason`) and reads its detail, activity,
  requests, access (empty) and setup. Every change to it is `404 device_not_found`, telling that
  Carbon when and why it was removed and to pair it again; for anyone else it doesn't exist.
- **Setup codes** go only to an Apple TV paired through a Mac; other devices are refused with why.

### When a pair ends

| Cause | Trigger |
|---|---|
| Revoked on the device | `DELETE /api/v1/device` from the app |
| Removed on the website or CLI | `DELETE /api/v1/devices/{device_id}` by the owner |
| No activity | `last_activity_at + pair_ttl_days` passes. A scheduler checks every minute. |
| Owner leaves the team | IAM webhook, then confirmed by introspection |

Activity means a command in a session, or the owner changing the device's settings. A device
that's merely online doesn't count.

When a pair ends, Extend ends any session with the matching reason, removes all access, sends
`unpaired` down the device's connection, invalidates the credential, and keeps the device row and
activity log (marked removed) so the log stays readable (as built, through the read path above). A
command running when the pair ends answers its caller `409 session_ended` at once (§5).

---

## 5. Sessions and commands

### Lifecycle

```
extend session new 7c1e09ab   → POST /sessions            → lock taken, session "active", device shows the Silicon
extend session connect a3f    → GET  /sessions/a3f        → CLI remembers a3f and caches the device's capabilities
extend snapshot -i            → POST /sessions/a3f/commands {"command":"snapshot","args":["-i"]}
extend click @e2              → POST /sessions/a3f/commands {"command":"click","args":["@e2"]}
extend session end a3f        → POST /sessions/a3f/end    → lock released, device indicator cleared
```

Session states: `active` → `paused` (during a takeover) → `active` → `ended`.
End reasons: `ended_by_silicon`, `idle_timeout`, `stopped_by_carbon`, `access_removed`,
`device_removed`, `pair_revoked`, `pair_expired`, `silicon_logged_out`, `left_team`,
`device_offline`, `environment_disabled`, `environment_cleaned`.

### One Silicon at a time

`device_locks` has the device id as its primary key. Starting a session inserts a row in the same
transaction that creates the session, so a second Silicon gets `409 device_in_use` with the current
holder's Silicon id, session start time and the `extend request send` command to ask for the device.

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

As built (2026-09-27):

- **A session ending mid-command answers at once.** The service races the device's answer against
  the session ending (checked in memory every 200 ms, in the database every 2 s). If the session
  ends first (device removed, pair revoked, access removed, the Carbon's Stop, a logout, leaving the
  team, the test environment disabled or cleaned), the caller gets `409 session_ended`: "Session
  <id> ended while `<command>` was running on <device>: <why>. `<command>` may have run.", a hint
  for that reason, and `details {end_reason, command_id, may_have_run: true}`. The device gets
  `cancel` if it is still connected, and the activity log records the outcome as unknown. A dropped
  socket or a passed deadline whose session had ended answers the same way.
- **A queued command checks again.** A command that waited behind another checks the session again
  when its turn comes, and is refused (`session_ended` or `session_paused`) instead of relayed.
- **Storage problems are warnings.** A file the command made that Extend could not store, share
  or record is reported in the result's `warnings` (what, why, what to do); `ok` is unchanged (§6).

### Ending on its own

- **Idle:** 300 s after the last command finished (or the start, with no commands). A command in
  flight holds the timer. As built: relaying a command sets `idle_ends_at` to its start +
  `timeout_ms` + 2 s + 300 s; the answer, a timeout or the device dropping restarts the 300 s from
  then.
- **Offline:** if the device stays disconnected for 120 s during a session, the session ends with
  `device_offline`, so a device that loses power doesn't stay locked.
- **Revocation:** see §9.

### Takeover

A Silicon hands the device to the Carbon (for Face ID, a payment, or an admin prompt) with
`extend takeover --reason "..."`. The session becomes `paused`: commands are refused with
`423 session_paused`, the device and website show the reason and a "Done" button, and the idle
timer stops for up to 30 minutes. The Carbon's "Done" or `extend takeover release` resumes it.
After 30 minutes paused, the session ends with `idle_timeout`.

### What the CLI shows in `--help`

On `session connect`, the CLI saves the device's `capabilities` list next to the session. While
that session is connected, `extend --help` lists only the commands whose capability is in that
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

A Linux computer without a screen reports only `terminal` (as built since 2026-09-27, following
`UNDERSTANDING.md`; it used to add `apps.launch` and `replay`). The final
list is whatever the device's app reports in its `hello` message, intersected with this table, so
a device missing a permission (say, Screen Recording on a Mac) simply lacks that capability until
the Carbon grants it. As built, some ✓ above are still reported missing with a reason: on Windows
`screen.record`, `logs`, `alerts` and `replay`; on Linux `logs`, and `screen.record` on Wayland;
on Android, `adb`, `apps.install`, `logs` and `screen.record` until Android debugging is connected
(§7). A locked computer (a Mac also asleep, or showing the login window or another account;
Windows also behind an admin prompt's secure desktop) reports everything that needs the screen as
missing with that reason, and reports it again within seconds of a lock or unlock; the terminal
stays.

### agent-device commands Extend does not expose

The parts made for app developers: `boot`, `shutdown`, `web …`, `viewport`, `react-native …`,
`react-devtools …`, `metro …`, `cdp …`, `perf …`, `trace …`, `network dump`, `debug symbols`,
`audio probe`, `push`, `trigger-app-event`, `install-from-source`, `settings …` (simulator helpers),
`fold`, `orientation`, `action-button`, `prepare ios-runner`, `mcp`, `doctor`. Discovery and connection are replaced by Extend's own commands:
`devices` → `extend device ls`, `connect`/`disconnect` → `extend session connect`/`disconnect`,
`session list` → `extend session ls`. Any of them sent through Extend returns
`404 unknown_command` naming the Extend replacement where there is one.

---

## 6. Files

Every file a command produces is stored in Briefcase for the Silicon, through Briefcase's OBO
endpoints:

1. Extend exchanges the Silicon's access token with IAM for an OBO proof bound to
   `briefcase.files.create`, the SHA-256 of the exact bytes, and metadata
   `{path: "", name, content_type}`. An empty path puts the file in Extend's private folder for that
   Silicon: `apps/extend/private/{silicon id}/`.
   As built, every file gets its own Briefcase name, for example
   `screenshot-20260926-153307-2228fc1d.png`: Briefcase stores a repeated name as a new version of
   the same entry, so two screenshots named `screenshot.png` would share one `entry_id`. Extend's
   own record and `name` in its API keep the name the device gave.
2. Extend sends the bytes to Briefcase `POST /api/v1/obo/files`.
3. Extend shares the file with the device owner Carbon through `POST /api/v1/obo/invitations`,
   which Briefcase classes as critical, so it needs Briefcase's approval of Extend once, in
   Honeycomb. The first draft said create, read and update; as built the share is **read and
   update**, never delete, because Briefcase refuses `write` on a file (`invalid_access`) and allows
   create only on folders (Open question 12). A Carbon who owns the Team still gets delete from
   Briefcase's own rules.
4. Extend records the `entry_id`, and returns the Briefcase permanent URL,
   `https://briefcase.teamofsilicons.com/org/{team}/…`, to the CLI.

Self-destruct defaults to 1 day. `--ttl` on the command that makes the file sets 1 minute to 30
days. `extend file keep <file_id>` makes a file permanent before it goes. As built, both are Extend's
own records (§13): the file is trashed through Briefcase's `entries.trash` with an `operation_id`
derived from the file id, so a retry is the same deletion, and a 404 counts as already gone;
`keep` marks the file permanent in Extend and leaves the Briefcase entry alone.

In a test environment every Briefcase call uses Briefcase's test environment with the same
`environment_id`. **See Open questions 1–3:** Briefcase's OBO endpoints don't currently take a
self-destruct time or offer "make permanent".

Sizes: a single file up to 1 GiB. Recordings stop at 1 GiB or 30 minutes, whichever comes first.

As built (2026-09-27):

- **Downloading through Extend** (Open question 13, built). `GET /api/v1/files/{file_id}/content`
  serves a file's bytes to the Silicon that made it and the Carbon who owns its device, after
  Extend's own visibility check, reading it from Briefcase as the caller (`briefcase.files.read`
  over OBO), so Briefcase's sharing applies too. It sends the device's content type, a
  `Content-Disposition` with the file's name, and honours one byte range. `extend file get` and
  every `--out` use it, and print the Briefcase link first. The service holds the whole file in
  memory while it answers. The route was checked against a real Briefcase (the local harness) by
  calling it directly; the CLI's downloads through it have run only against the local file store.
- **Nothing is lost quietly.** A file the device listed but never uploaded, one under an upload id
  not issued for the command, one Briefcase refused to store, one stored but not shared with the
  device's Carbon, and one stored but not recorded (before, that failed the command with a 500)
  each become a line in the result's `warnings`.
- **Self-destruct keeps the record until Briefcase confirms.** When a file is due, Extend acts as the
  creating Silicon with the latest login it holds for it in that team and world (its authorization
  cache, else a running session; no session is needed) and deletes Extend's record only once
  Briefcase answers the trash (a 404 counts as gone). A failure is logged with the file id and
  retried, backing off from 1 minute to 1 hour; due files are hidden from lists and reads at once.
  Logins are held in memory, so after a restart, or once the Silicon's token has expired, the
  deletion waits until the Silicon uses Extend again (Open question C2).

---

## 7. The Extend apps

Every app does four things: keep the WebSocket to the service open, run commands through
agent-device, show the in-use indicator with a Stop button, and offer "Revoke pair". It starts at
boot and reconnects on its own.

### Connection protocol (`WSS /api/v1/device/connect`)

- Auth: `Authorization: Extend-Device <device_credential>` on the upgrade request.
- Frames are flat JSON text: `{"type": ..., <fields>}` (docs/device-protocol.md). Files never travel on the socket.
- Heartbeat: the service pings every 15 s; the device is **offline** after 45 s without a pong.
- Reconnect: exponential backoff from 1 s to 60 s with full jitter.
- One live connection per device. A new connection replaces the old one, which gets `superseded`.
- Close codes (as built): `4401` unpaired, `4409` superseded, `4426` upgrade required, and `4503`
  when the device's test environment closes (disabled, or waiting for Honeycomb's readiness): the
  pair is kept and the app reconnects with backoff. The handshake and the device's HTTP routes
  answer `503 testing_environment_not_ready` for such an environment.

| Direction | `type` | Meaning |
|---|---|---|
| device → service | `hello` | App version, OS and version, agent-device version, capabilities, setup state |
| device → service | `setup_progress` | One setup step changed |
| device → service | `result` | Answer to a `command`, same `id` |
| device → service | `stop` | The Carbon tapped Stop |
| device → service | `takeover_done` | The Carbon tapped Done on a takeover |
| service → device | `command` | Run this, with `deadline` and `upload_ids` |
| service → device | `cancel` | Stop the command with this `id`; the device answers its `result` with `ok:false`, code `cancelled` |
| service → device | `session_started` / `session_ended` | Show or clear the indicator (Silicon id, since) |
| service → device | `takeover` | Show the takeover reason and Done button |
| service → device | `display` | TV: show or clear a link, image, video or text |
| service → device | `unpaired` | Forget the credential, return to the pairing screen |
| service → device | `environment` | Test environment name for the banner, or `null` for production |
| service → device | `superseded` | Another connection took over; close without reconnecting |

### Per device

As built on 2026-09-27 (the first draft's rows for Android, Mac, Windows and Linux described plans
that changed; §13 says why):

| Device | How commands are carried out | Indicator |
|---|---|---|
| **Android phone and tablet** | The app's AccessibilityService reads every window as an element tree (same `@eN` refs and snapshot shape as agent-device), taps and gestures with `dispatchGesture`, presses back/home/recents, takes screenshots, and reads notifications through a notification listener. **Android debugging** (the Carbon pairs Wireless debugging from the app once; a TV can use its TCP port) connects the app's own ADB client on the device, which adds `adb`, `install`/`reinstall`, `logs` and `record`. Recording runs supervised `screenrecord` segments of up to 180 s and joins them into one MP4, bounded to 30 minutes or 1 GiB; there is no per-recording consent prompt. Without debugging connected those capabilities are reported missing with "connect Android debugging". | Ongoing notification with Stop |
| **Android TV, Google TV, Fire OS** | Same, plus the remote's arrows and select through the accessibility D-pad actions (Android 13+). With Android debugging connected, every remote button but Power (Menu too, and older TVs' D-pad) is a real key press through `input keyevent`; Power is always refused. The display screen is an activity inside the app. With Android debugging: `adb`, `install`/`reinstall` and `logs`; no recording on TVs. The app is named **Silicon Extend TV** on a TV. | Corner badge drawn as an accessibility overlay (no extra permission); Stop in the app |
| **Mac** | agent-device's macOS driver through its signed native helper, with no XCTest runner and no UI Automation setup: Accessibility for the element tree, pointer input and text entry (text passed over stdin, focus checked before each key event), ScreenCaptureKit for screenshots and H.264 recording of one app or the display. The only setup steps are Accessibility and Screen Recording for Silicon Extend. A session starts on the frontmost app; `open <app>` binds the named app; links open with the system and the session follows the frontmost app. Terminal commands run as the logged-in user. | Menu bar icon changes; banner; Stop in the menu |
| **Windows** (built by Extend) | UI Automation for the element tree, `SendInput` for mouse and keyboard, GDI for screenshots, Win32 for the clipboard, the Start menu and shell for apps, `cmd.exe` for the terminal. Mapped onto the same agent-device command set and snapshot shape. `record`, `logs`, `alert` and `replay`/`test`/`batch` are reported missing. Compile-checked and unit-tested only; it has never run on Windows. | Tray icon changes; banner with Stop |
| **Linux** | agent-device's Linux driver: AT-SPI2 for the element tree, xdotool (X11) or ydotool (Wayland) for input, a screenshot tool (gnome-screenshot, scrot or ImageMagick; grim on Wayland), xclip/xsel or wl-clipboard. On X11, recording with ffmpeg (libx264 from `x11grab`): the whole screen, or one app's window through XComposite so windows over it are not recorded (an app that can't redraw its whole window within 5 s is refused, with "record the whole screen instead"). `--quality` picks the bit rate: `normal` 8 Mbit/s, `high` 20 Mbit/s. Wayland recording (the ScreenCast portal) is not built and is reported missing, as are `logs`. PTY for the terminal. | Banner with Stop |
| **iPhone, iPad** (via Mac) | The Mac's app runs agent-device's physical-iOS driver: its XCTest runner is installed on the iPhone once over USB, then reached over Wi-Fi. | On the Mac's app and the website |
| **Apple TV** (via Mac) | The Companion protocol for apps and remote buttons, and AirPlay for pictures and videos, from the Mac on the same network. The Apple TV shows a code the Carbon enters once. | On the Mac's app and the website |
| **Samsung TV** (via computer) | Tizen's local remote-control WebSocket (ports 8001/8002). The TV asks the Carbon to allow the connection once and issues a token. | On the host's app and the website |
| **LG TV** (via computer) | webOS's local SSAP WebSocket (ports 3000/3001). The TV asks the Carbon to accept once and issues a client key. | On the host's app and the website |

A device paired through a host is offline whenever its host is offline or can't reach it.

As built (2026-09-27): the Mac, Windows and Linux app turns start at login on by itself once paired,
unless the Carbon turned it off (the window's switch, the menu, `run --no-autostart`); a copy run
from App Translocation or a disk image is never registered, and `--headless` only with
`--autostart`. On a Mac `record start --quality normal|high` is agent-device's `medium|high` (the
same for a carried iPhone or iPad). A host shows Stop (and Done) for each device it carries in its
window, tray menu and banner, but can't revoke a carried device's pair: the device API has no
route for that yet, so the window sends the Carbon to the website. After a restart turns Wireless
debugging off, the Android app asks the Carbon to turn it back on (a notification, and the setup
step becomes `needs_carbon`) while the device stays usable through accessibility.

---

## 8. Test environments

- **Selecting one:** the caller sends the test application's `app_secret` in
  `X-Testing-Application-Secret`. Extend asks IAM which environment that secret belongs to (IAM
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
services use. Actions: `prepare`, `rotate-key`, `clean`, `disable`, `restore`, `purge`. As built,
Extend also accepts `activate` (below), and acknowledges `import`, `refresh-import` and
`retire-applications` (which, when it names Extend, clears Extend's data and disables the
environment).

| Action | Extend does |
|---|---|
| `prepare` | Creates the schema, records the environment as `preparing`. Test access opens only after Honeycomb confirms every service is ready and IAM reports the environment ready. |
| `rotate-key` | Records the new `key_version`. |
| `clean` | Blocks access, ends sessions, sends `unpaired` to every device in the world, truncates the schema, bumps `generation`. Completes only when all of that is done. Requests, jobs and webhooks carrying an older revision or generation are dropped. As built, this is the clean fence below: requests and scheduler passes admitted before the clean finish first, nothing new gets in until it completes, and webhooks for an environment that isn't open are dropped. |
| `disable` | Blocks access immediately and ends every running session. Data is kept. |
| `restore` | Makes retained data available again; needs a free slot of the 10. Can't undo a clean. |
| `purge` | Drops the schema and forgets the environment. |

Each receipt is `pending`, `completed` or `failed`, repeating the identical instruction returns the
stored receipt, and a changed instruction under the same `operation_id` is `409`. These work even
while the environment is disabled. Extend reports the environment's last activity to Honeycomb;
Honeycomb, not Extend, decides when it expires.

As built (2026-09-27):

- **Selecting a world, on every route.** Every `/api/v{n}/` route checks the secret whenever the
  header is present, the routes that sign nobody in included (enrollments, `/api/v1/iam`, reports,
  telemetry, the device routes). An empty, repeated, unprintable, unknown or revoked secret, or one
  of a disabled or removed environment, is `401 testing_secret_invalid`; an environment being
  prepared or cleaned, or one IAM knows but Extend was never prepared for, is
  `503 testing_environment_not_ready`; each says "Nothing ran in production". `GET /api/version`,
  `/live`, `/ready`, `/webhook/` and `/internal/…` take no secret (the version handshake is the same
  in every world and carries no data). IAM's answer for an open environment is reused for at most
  10 s and never across a lifecycle change; any other state is asked live.
- **Readiness.** `prepare`, `restore` and an `import` that arrives first leave the environment
  `preparing`. It opens when IAM accepts its app secret (Honeycomb confirms readiness to IAM in its
  activate phase, and IAM refuses the secret until then) or when Honeycomb sends the participant
  action `activate`. Honeycomb doesn't send `activate` to participants today (Open question C4).
- **Order and retries.** Operations on one environment run one at a time (an advisory lock). A
  pending receipt is stored, with the new revision, before any effect. `environment_revision` must
  be newer than or equal to the stored one (only an older one is stale). Only the latest operation
  can be retried; a superseded one is refused. Only `clean` advances `generation` (a new clean must)
  and only `rotate-key` advances `key_version` (a new rotate-key must).
- **States.** A disabled environment stays disabled through `clean` and `rotate-key`, and `prepare`
  or `activate` of it is refused (send `restore`). A removed environment accepts only `purge`, not
  even the retry of an operation accepted before the purge, so nothing brings it back. Every move
  into an active state (`preparing`, `ready`, `cleaning`) takes one of the 10 slots under a global
  lock; with none free, `409 test_environment_limit` leaves a `failed` receipt, and retrying the
  identical instruction later succeeds.
- **Disable** ends every running session (`environment_disabled`) and closes every device socket
  with `4503`; no pair ends, and `restore` brings the same credentials back.
- **The clean fence.** Every request in a test world holds a read guard on that world's fence while
  it runs; `clean`, `disable` and `purge` take the write guard before they wipe or close the world,
  so work admitted before them can't write after them. The scheduler holds the same guard per world
  and skips worlds that aren't open. A clean also deletes the environment's waiting pairing codes;
  webhooks for an environment that isn't open are dropped (§9).

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
prompt, not a decision: Extend confirms with IAM introspection before acting, and re-checks on
every command, so a lost webhook delays nothing.

As built (2026-09-27):

- **Webhooks.** Production refuses to start without `EXTEND_IAM_WEBHOOK_SECRET`. An event is
  recorded as applied only after its effects succeed, in the same transaction; a failure answers
  5xx and IAM's retry applies it again. Events about one aggregate apply in version order; an older
  or equal one is acknowledged and dropped. Extend asks IAM first (the Silicon's live
  authorization, then membership); a removal the event reports is used only when IAM can't answer.
  Test deliveries go to their environment only while it is ready or being prepared.
- **IAM sends applications no logout or session-revocation events** (seen against real IAM), so
  the two rows above marked "IAM webhook" for logging out don't arrive by webhook. Instead:
  - `POST /api/v1/auth/logout` identifies the member from the token being revoked (a refresh token
    works alone) and ends a Silicon's running sessions; if IAM can't say whose login it is,
    nothing is revoked and the error says so.
  - **A refused login ends access on the session routes.** When a Silicon's call to a session route
    is refused and Extend can tell which Silicon the token belonged to (a login it saw that Silicon
    use in this world since it started): `not_a_team_member` for a team where it has running
    sessions ends them at once as `left_team`, unless IAM confirms it is still an active member
    there; `token_expired` ends all its running sessions as `silicon_logged_out` 15 s later, unless
    IAM accepts another login of that Silicon that Extend has seen (the CLI refreshes an expired
    token at once, so ordinary expiry never ends a session). If IAM can't answer, nothing ends.
    A logout made elsewhere is therefore noticed only at the Silicon's next session call, and
    only by an instance that saw its login (Open question C1).
- **Starting a session registers the Silicon as a Ting recipient** (in the background), so requests
  reach it; a real Ting refuses requests to unregistered recipients.

---

## 10. Versioning

- The API is versioned in the path (`/api/v1`). Breaking changes get a new major; additive
  changes don't.
- **Negotiation:** the client sends `Silicon-Extend-Supported-API-Versions: 1, 2` to
  `GET /api/version`. The service answers with the highest major both support, in the body and in
  `Silicon-Extend-API-Version`. The client pins that version for its lifetime. A request whose pin
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

As built (2026-09-27, `crates/extend-service/src/versions.rs`):

- **Several majors side by side.** Each major is mounted under its own `/api/v{n}/` prefix, and one
  version layer in front of every route reads the major from the path: a major the build doesn't
  serve is `400 api_version_unsupported`, a pin naming another major is `400 api_version_mismatch`,
  a sunset major is `410 api_version_sunset`. Only v1 exists; the module docs say how a v2 route
  table joins it (reuse v1 handlers where the shape holds, add 2 to `SERVED`, record
  `contracts/v2/client`, then deprecate 1).
- **Lifecycle state** is kept in `extend_global.api_versions`, shared by every instance. Configuration
  deprecates a major (`EXTEND_DEPRECATED_API_VERSIONS=1`, applied when an instance starts; the newest
  served major can't be deprecated, and removing a major from the list makes it current again unless
  it was sunset). Requests are counted per major and UTC day in `extend_global.api_version_usage`.
  Every 5 minutes each instance sunsets a deprecated major that has gone 7 consecutive days without a
  request, counted from the later of its deprecation and the end of its last day with a request, so
  a deprecated major always gets a full week. Sunset is final.
- **Headers.** Every response on a deprecated major carries `Deprecation: @<unix seconds>`
  (RFC 9745) and `Sunset: <HTTP-date>` (RFC 8594, the soonest it can be sunset). Negotiation offers
  only majors that aren't sunset; a client that speaks only retired majors gets `410`.
- **The matrix** (`GET /api/v1/contracts`) is built from that state: `current`, `supported`,
  `deprecated`, the rule, and per major its state, `deprecated_at`, `sunset_at`,
  `sunset_earliest_at`, `last_request_on` and the compatible client crate and CLI ranges
  (`EXTEND_API_V{n}_CLIENT_CRATE`, `EXTEND_API_V{n}_CLI`, default `>=n.0.0, <n+1.0.0`) and
  `device_app_min` (`EXTEND_DEVICE_APP_MIN_VERSION`). `extend version` reads it and says whether the
  CLI is current, deprecated, sunset or unsupported.
- **Contract fixtures** live in `contracts/` (`contracts/README.md`): `v1/client` is recorded by the
  client crate's own test (`contract_fixtures`, which fails when the files no longer match what the
  crate sends), `v1/device` is derived by hand from this protocol and the apps' code until the
  apps dump their own frames, and `internal/honeycomb` from Honeycomb's participant client. The
  service test `contracts` replays every fixture of every major still served against a real
  service and PostgreSQL, and fails if a consumer's request is no longer accepted or an answer lost
  a field that consumer reads. CI runs both.

---

## 11. CLI and client internals

- **Home:** `$SILICON_HOME` if set, else `~`. State lives in `{home}/.extend/`. `extend config home <dir>`
  moves it; it refuses a path that isn't an existing directory (`not a directory: <path>`). The
  chosen location is recorded in `{default home}/.extend/home` so later runs find it. As built, the
  login, settings, test environments and sessions move with it, and a directory that already holds
  Extend state is refused unless `--use-existing` switches to it.
- **Files** (all `0600`, directory `0700`):
  `auth.json` (tokens, team), `config.toml`, `sessions/current`, `sessions/{session_id}.json`
  (device, capabilities), `test/{environment_id}.json` (test app secret and that world's tokens).
- **Token refresh** is serialised with an advisory file lock on `auth.json`, and the replacement
  pair is written atomically (write temporary, `fsync`, rename), as IAM's client docs require.
- **No daemon** is needed: each command is one HTTPS request. See Open question 7.
- **Session selection**, first match wins: `--session <id>`, then `EXTEND_SESSION`, then the
  connected session.
- **Output:** text by default; `--json` gives exactly one JSON document on stdout. Progress,
  warnings and the test-environment line go to stderr so stdout stays safe for scripts and binary
  output. As built (2026-09-27), following the house convention of the sibling CLIs: on success the
  data itself, with no wrapper (`extend iam --json` has `app_id` at the top, `extend login status
  --json` has `authenticated`); on failure `{"error": {code, message, hint, request_id, docs_url,
  details, exit_code}}` on stderr and nothing on stdout. `login status` exits 0 whether or not a
  login works, as `dm login status` does (Open question C7).
- **Test secrets for scripts:** `EXTEND_TEST_SECRET` may stand in for `extend config test add`; it is
  checked to belong to `--test`'s environment and never written to disk, and without `--test` any
  command that would call Extend is refused, so a test script never reaches production.
- **The package has what the CLI has:** attachment building and the 8 MiB limits live in the client
  crate (`silicon_extend_client::attachments`), and file downloads use the client's
  `file_content`/`file_download`.
- **Exit codes:** listed in `cli.yaml`. The same code always means the same kind of failure.

---

## 12. Telemetry, reports, logs

- Telemetry goes to Space Station, on by default, off with `extend config set telemetry off` or the
  website's settings. The CLI posts events to `POST /api/v1/telemetry` so no ingest key ships in
  the CLI. Each event carries source (`cli`, `client`, `app`, `service`, `web`), step, outcome,
  duration, command name, device OS, and the session and command ids. Never typed text, clipboard
  contents, screen contents, tokens, codes or secrets.
- `extend report "<message>" [--pr <url>]` stores the report and emails it through Postmark to the
  three addresses in `UNDERSTANDING.md`. In a test environment the email is simulated. As built
  (2026-09-27) the default list (`EXTEND_REPORT_RECIPIENTS`) sends to `shubhastro2@gmail.com`, where
  `UNDERSTANDING.md` writes `shubhastro2@gmails.com` (Open question C6).
- **Redaction in the activity log:** text typed with `fill` and `type`, and text written with
  `clipboard write`, is replaced with `[redacted N chars]`. Everything else is logged as sent.
- Secrets never appear in URLs, logs, audit rows, telemetry or stored webhook bodies. Credentials and
  enrollment secrets are stored as SHA-256 digests. Pairing codes are stored in plain text: the
  unpaired app has to be able to fetch its current code by polling, a code lives 5 minutes, works
  once, and guessing is rate limited.

---

## 13. As built (2026-09-26, updated twice on 2026-09-27)

Differences from the first draft, each deliberate:

- **Commands are CLI tokens, not structured input.** A command is `{command, args}` where `args` are
  agent-device's command-line tokens. agent-device's structured inputs differ per command and change
  between versions; forwarding tokens keeps its own parser the authority. Flags that pick a device or
  session inside agent-device are refused. Files a caller sends (a replay script, an image for a TV)
  travel as `attachments` and are referenced in args as `attachment:<name>`.
- **Self-destruct is enforced by Extend.** Briefcase's delegated upload takes no self-destruct time,
  so Extend records the time, deletes the file through Briefcase's delegated trash when it passes,
  and `extend file keep` cancels that. Open questions 1–2 still stand for Briefcase itself.
- **One service instance.** Device sockets and waiting commands live in the process
  (`docs/operations.md`).
- **Android reads the screen with an AccessibilityService**, not agent-device's helper over
  on-device ADB: the accessibility tree gives the same element list and needs no debugging setup.
  ADB is used only for what accessibility can't do (`adb`, `install`, `logs`, `record`), through an
  ADB client inside the app (a patched libadb-android) that the Carbon pairs with Wireless debugging
  once. Every connection must start TLS once paired and prove it reaches Android's shell before it
  is used. See `apps/android/README.md` for the limits (256 KiB inline per output stream, 256 MiB per
  command or pull, results capped at 15 MiB, recordings kept through a dropped socket for 240 s).
- **`adb` arguments are verbatim.** Everything after the first `adb` argument reaches the device as
  typed; Extend's own flags go before it (`cli.yaml`).
- **Local inputs.** A command carries at most 8 attachments and 8 MiB in total. `install`,
  `reinstall`, `adb install` and `adb push` take a local file only; Briefcase file ids and links are
  refused by the CLI before sending (Open question 14).
- **Device answers for commands it stops.** Stop on the device and a session ending answer the
  session's running and queued commands with `session_ended`; a `cancel` frame is answered with
  `cancelled`; on Android, disconnecting debugging answers debugging commands with
  `device_not_ready`. On computers, a command that arrives for a session the app already ended is
  answered `session_ended` without running. On Android, commands running when the socket drops get
  no answer (the service reports `device_offline`).
- **Mac uses agent-device's native helper for everything**: text entry through Accessibility and
  recording through ScreenCaptureKit, so neither Xcode nor UI Automation is needed (the first draft
  expected the XCTest runner for typing and recording).
- **Computer cleanup.** When agent-device can't release a Mac or Linux computer after a session
  (close fails, then a forced release fails), the app keeps working but reports every capability
  that needs agent-device as missing, with one reason, and retries in the background; `terminal`
  and `takeover` stay available.
- **Packaged runtime identity.** Mac and Linux packages stamp agent-device's version with a
  content digest and install an entry that replaces a daemon started from another install path,
  so an update or a moved app never keeps running old code.
- **Local stand-ins** for Silicon IAM, Briefcase and Ting (`EXTEND_IAM_MODE=local` etc.) exist for
  development and tests and are refused in production.
- **Extra endpoints:** `GET /api/v1/team/silicons` (access picker), `iam_login_url` in
  `GET /api/v1/iam`, and `input` on setup steps (`"code"` for an Apple TV).
- **ISI:** when the CLI runs with `ISI` set, it's sent as `X-Silicon-ISI` and recorded with session
  starts and commands in the activity log. Nothing depends on it.
- **Telemetry** goes to each world's outbox table and is exported to Space Station when
  `EXTEND_SPACE_STATION_KEY` (and, per test environment, `EXTEND_TEST_TELEMETRY_KEYS`) is set.

Added by the second round (2026-09-27), each described in its section above:

- **Production is never a default.** `EXTEND_ENVIRONMENT` is required (the Docker image sets
  `production`); an unset value refuses to start and says which to choose. Production refuses the
  local stand-ins and member-id logins, and requires the IAM webhook secret.
- **Session routes act on refused logins** (§9), and a session ending mid-command answers at once
  (§5); the idle timer holds while a command runs (§5).
- **Requests:** every new reason is delivered, raw; only a byte-identical repeat within 60 s is
  folded; a raw reason is capped at 1,000 characters; pending requests are retried with the sender's
  latest login and fail with a reason after 6 attempts (`last_error`, `request_failed` in the log).
- **Files:** a download route, `warnings` on command results, and self-destruct that keeps the
  record until Briefcase confirms (§6).
- **Devices:** codes bound to their world, an atomic test device limit, removed devices readable to
  their Carbon, the online filter applied before paging (§4, `api.yaml`).
- **Test environments:** readiness, the lifecycle rules, the clean fence, disable without unpairing
  and close code `4503` (§7, §8).
- **Versioning:** the lifecycle, headers, matrix and contract fixtures (§10).

## Open questions

Proposals in this file that `UNDERSTANDING.md` doesn't settle, or where it conflicts with another
service. Each needs a Carbon's decision.

### Carbon decisions after round 2 (2026-09-27)

What the build now does and needs a yes or a change; the numbered questions below still stand
unless marked settled. Naming, signing and publishing decisions are in `docs/completion-work.md`.

- **C1. Logging out elsewhere ends access by a heuristic.** IAM sends applications no logout or
  revocation events, so Extend ends a Silicon's sessions 15 s after IAM first refuses its login on a
  session route, and only notices at that call (§9). Accept 15 s and "at the next session call", or
  ask IAM for logout and revocation webhooks for the `extend` app.
- **C2. Self-destruct depends on logins Extend saw.** After a service restart, or once the
  Silicon's token has expired, a due file waits (hidden) until the Silicon uses Extend again (§6).
  A durable fix needs one of: Extend's own Briefcase credential, a durable OBO delegation, or
  Briefcase taking the self-destruct time itself through OBO (questions 1–2). Pending Ting requests
  have the same limit, but fail instead of waiting: with no login held, each 30-second retry still
  counts, so the request is marked failed after about 2.5 minutes, saying why.
- **C3. A raw request reason is capped at 1,000 characters**, whitespace included (the reason
  itself is 1–300 without it). Confirm the cap or change it.
- **C4. The `activate` participant action.** Extend opens a test environment when IAM accepts its
  secret, or on a participant `activate`, which Honeycomb doesn't send today. Keep it as an Extend
  extension, ask Honeycomb to send it, or drop it and rely on IAM alone.
- **C5. IAM event ids and aggregate versions survive a clean.** They hold no test data, and keeping
  them stops a replayed event from applying twice. Confirm.
- **C6. The bug-report address.** `UNDERSTANDING.md` lists `shubhastro2@gmails.com`; the build sends
  to `shubhastro2@gmail.com`. Confirm the address and correct `UNDERSTANDING.md` (Carbon-only).
- **C7. The CLI's JSON and `login status`.** `--json` now prints the data itself (errors as
  `{"error": …}` on stderr), and `login status` exits 0 when not signed in, following `dm`. This
  changed `cli.yaml` (it said `{ok, data}` and exit 3). Confirm.
- **C8. Sign-up.** The website's "Create an account" goes to IAM's `/signup` beside its `/login`
  (or `iam_signup_url` if `GET /api/v1/iam` ever returns one). Confirm IAM's sign-up address, or
  have Extend return it.
- **C9. Carbon logout (question 4)** is still open: a Carbon signing out of the website ends
  nothing.

Settled by round 2: question 13 (downloads go through Extend), question 15 (what `--quality`
means on computers), the audit's "bind or document" for pairing codes across worlds (bound, as
`UNDERSTANDING.md` says), and the removed-device promise (a read path, not a copy change).

### Numbered questions

1. **Briefcase self-destruct through OBO.** Briefcase's OBO upload (`/obo/files`) takes only `path`,
   `name` and `content_type`. Self-destruct (`self_destruct_minutes`) exists only on its direct
   upload. Extend needs it on the OBO path.
2. **Making a file permanent through OBO.** Briefcase has no OBO endpoint for it. Its own rule, that
   only the creator, admins and owners can make a file permanent, fits Extend acting for the
   creating Silicon, but the endpoint is missing.
3. **Where files live.** `UNDERSTANDING.md` says "the private folder of the Silicon". Briefcase OBO
   can only write inside the app's own folder, so files land in `apps/extend/private/{silicon id}/`.
   Is that acceptable?
4. **"Logging out … ends it immediately."** Read here as the *Silicon* logging out. Ending every
   Silicon's access when the owning *Carbon* logs out of the website would break sessions each
   time a browser signs out. Confirm. (How a Silicon's logout elsewhere is noticed: C1.)
5. **Team-visible devices.** Proposed: other Carbons in the team see the device's name, OS, owner
   and whether it is online, read-only. They can't grant access or see the activity log.
6. **Can Carbons start sessions?** Proposed: no; sessions are for Silicons, as `UNDERSTANDING.md`
   describes them. Carbons manage devices.
7. **Daemon.** The house style suggests a CLI daemon. Extend doesn't need one for correctness;
   proposed to ship without it and add one only if per-command connection setup proves slow.
8. **Values not in `UNDERSTANDING.md`:** `device_id` as 8 hexadecimal characters; pairing-code rate
   limits (5 per Carbon per 10 min, 30 per IP per hour); offline session end after 120 s; takeover
   pause up to 30 min; command timeout 30 s by default and 300 s at most; 1 GiB file and
   30-minute recording caps; activity-log redaction of typed text. As built, the per-IP claim limit
   does not exist; added since: 60 enrollments per hour per address, 20 cross-world code answers per
   Carbon per 15 min, the 15 s logout grace (C1), the 1,000-character raw reason (C3), 6 Ting
   attempts 30 s apart, self-destruct retries backing off from 1 minute to 1 hour, and a 10 s / 5 s
   wait for a test environment's device-add turn.
9. **Activity-log retention.** Not stated. Proposed: keep for the life of the device plus 90 days
   after it is removed.
10. **Physical Fire TV.** `UNDERSTANDING.md` says Extend adds it. agent-device's Fire TV support is
    limited to Amazon's virtual device, so screen reading on a physical Fire TV depends on Extend's
    fork of the Android helper working on Fire OS. Needs a hardware check early.
11. **iPhone signing.** agent-device's physical-iOS runner is an XCTest app that must be signed with
    an Apple development team and installed from a Mac with Xcode tools. The setup guide needs to
    say which Apple account signs it: the Carbon's own, or a team one.
12. **What the owner Carbon may do with a Silicon's file.** §6 first said create, read and update.
    Briefcase refuses `write` on a file and grants create only on folders, so the build shares read
    and update. Confirm read and update, or ask Briefcase for another grant.
13. **Downloading files through Extend.** *Settled as built on 2026-09-27:*
    `GET /api/v1/files/{file_id}/content` reads the file from Briefcase on the member's behalf after
    Extend's own visibility check (§6, `api.yaml`); `extend file get` and every `--out` use it. It
    was the proposal here because the CLI's use of the Briefcase URL with an Extend token was
    refused by Briefcase.
14. **Briefcase file ids as inputs.** `cli.yaml` offered a Briefcase file id for `install`, and
    still does for `replay`, `display` and `diff screenshot --baseline`; nothing in the CLI, the
    service or the devices resolves one. The CLI now refuses ids for `install` and asks for a local
    file. Decide whether the service (or the device) should fetch Briefcase inputs, which would also
    lift the 8 MiB attachment limit for APKs, or whether `cli.yaml` drops `file_id` there.
15. **`record start --quality` on computers.** *Settled as built on 2026-09-27:* `normal` and
    `high` work everywhere `cli.yaml` offers them. The agent passes them to agent-device as
    `medium` and `high`; on a Mac (and a carried iPhone or iPad) that is the export quality, and the
    fork's Linux recorder now encodes at 8 Mbit/s (`medium`) or 20 Mbit/s (`high`), as Android's
    screenrecord does, instead of refusing the option.
