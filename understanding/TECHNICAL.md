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

**1.1.0 draft, 2026-09-27, for the Carbon's approval.** It follows `UNDERSTANDING.md` as the Carbon
edited it that day (devices belong to the Carbons who paired them, several Carbons per device, waking
a device, the device engine) and the Carbon's four decisions of the same day:

1. A Carbon logging out of Extend (website or CLI) ends the running sessions of the Silicons that
   Carbon gave access to, and only that Carbon's side. A Silicon's logout ends its own (§9).
2. The Carbon a request is routed to sees the requesting Silicon's id and reason. Its Ting goes at
   once from the asking Silicon when the two share a Team, else from the recipient's own login (§5).
3. On a computer several Carbons paired, only Silicons given access by the Carbon who installed
   Extend on it (its first pair) get the terminal; the others get the screen, keyboard and apps
   ([Shared computers](#9a-shared-computers-11)).
4. Ting types stay per Team: Extend registers them itself where the Carbon is that Team's Ting
   manager (and Ting accepts it on their behalf), and otherwise shows the exact command (C2);
   `docs/requests/ting-app-level-types.md` asks Ting for app-level types.

It also adds setup retry and plain-language setup errors (§4), and calls the device engine Silicon
Extend's everywhere a Carbon or Silicon can see it: names, paths, settings and helper apps (§2, §7). Sections marked **1.1** describe the
design being built; §13 lists where the build differs once it lands. API v1 stays additive
(`api.yaml`), and 1.0 apps, CLI, client and website keep working.

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
| `device_id` | `7c1e09ab` | `^[0-9a-f]{8}$`, 8 lowercase hexadecimal characters, random | Unique within its world. Never reused, even after removal. 1.1: one per **pair** (one Carbon's pairing of a device), so a device two Carbons paired has two. |
| `instance_id` (1.1) | UUID | UUID | One per physical device, made by the service at its first pairing. Every pair of the device shares it. The app learns it from `GET /api/v1/device`; it never asserts one. |
| `device_credential` | `edc_x9Lk…` (47 chars) | `^edc_[A-Za-z0-9_-]{43}$`, 32 random bytes base64url | Issued once when pairing completes, one per pair. Valid until the pair ends; 1.1: on a computer several Carbons paired, replaced at the end of every session (the old one works until the app confirms). Stored hashed (SHA-256). On the device it lives in the OS secret store (Keychain, Android Keystore, DPAPI, libsecret). |
| `session_id` | `a3f` | `^[0-9a-f]{3,}$`, lowercase hexadecimal, 3 characters to start | Unique within its world and **never reused**. Allocation picks at random among the unused ids of the shortest length that still has any left. When all 4,096 three-character ids are used, new ids have 4 characters (65,536), then 5, and so on. |
| `command_id` | UUIDv7 | UUIDv7 | One per command sent into a session. Also the activity-log entry id for that command. |
| `request_id` | UUIDv7 | UUIDv7 | One per Silicon-to-Silicon request. |
| `activity_id` | UUIDv7 | UUIDv7 | One per activity-log entry that isn't a command (session started, access granted, pair revoked…). |
| `upload_id` | UUIDv7 | UUIDv7 | One per file a device uploads to Extend on its way to Briefcase. Single use. |
| `takeover_id` | UUIDv7 | UUIDv7 | One per takeover hold. |
| `wake_id` (1.1) | UUID | UUID | One per wake request: at most one open per physical device, Team and Silicon. |
| Side tag (1.1) | `9f2c4b1a0d3e5f67` | `^[0-9a-f]{16}$`: the first 16 hex characters of HMAC-SHA256(salt, owner id + "\n" + Team) | Names a side (a Team plus the Carbon who gave access) on one device without naming either. The salt is kept per physical device by the service and never leaves it; a computer and the devices it carries share one. |
| `hardware_key` (1.1) | 64 hex | `^[0-9a-f]{64}$`: HMAC-SHA256(world hardware salt, driver + ":" + stable hardware id) | Sent by a computer for a device it carries, so Extend recognises one TV added by two Carbons. A pseudonym, not a secret: anyone with the salt can test a guessed id. Never returned by the API. |
| Device `version` | `7` | Integer ≥ 1 | Increments on every change to a device's settings. Sent as `ETag` and required as `If-Match` on changes. |

### Values Extend bounds

| Name | Type and range | Default |
|---|---|---|
| Device `name` | 1–64 Unicode scalar values after trimming. No control characters. Not unique. | Set by the Carbon at pairing |
| Device `visibility` | `team` or `personal`. 1.1: always `personal` (a device is visible only to the Carbons who paired it); a value sent is ignored | `personal` |
| `pair_ttl_days` | Integer 1–30. Days without activity before the pair ends on its own. | `14` |
| Session idle timeout | Fixed 300 s after the last command finished, or after the session started if no command was sent. A command in flight holds it (until 300 s after its deadline). | 300 s |
| Request `reason` | 1–300 Unicode scalar values after trimming, and at most 1,000 with the whitespace around it (as built; Open question C3). Stored and delivered exactly as sent, whitespace included. | required |
| File self-destruct | 1 minute to 30 days (43,200 minutes), in whole minutes | 1 day (1,440 minutes) |
| `timeout_ms` on a command | Integer 1,000–300,000 | 30,000 |
| Test environment paired devices | At most 5 per environment | — |
| Active test environments | At most 10 across the whole deployment | — |
| Page `limit` | Integer 1–100 | 50 |
| Wake request `reason` (1.1) | As a request reason: 1–300 after trimming, at most 1,000 raw | required |
| Wake request lifetime (1.1) | Expires 1,800 s after the last ask; asking again is allowed after 300 s and refreshes it | — |
| Wake alerts and Tings (1.1) | The device sounds at most every 900 s; one Carbon Ting per pair and Team every 900 s; at most 6 wake Tings per Carbon per hour (later ones deferred, never refused) | — |
| Pairs per device (1.1) | `EXTEND_MAX_PAIRS_PER_DEVICE`, a guard on connections, not a product rule | 8 |
| Waiting "Pair with another Carbon" codes (1.1) | At most 3 per device | — |
| Setup retries (1.1) | At most one per device every 5 s | — |

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
| Element ref | `@e12` | `^@e[0-9]+$` | The device engine. Valid until the next `snapshot` in the same session. |
| Selector | `role="button" label="Continue"` | The device engine's selector expression | The device engine |

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
 Carbon ──website──────┤  HTTPS / JSON                   │     └── the device engine operates the device
 other apps ─client────┴──►  Extend service  ◄── WSS ────┤
                              │   │    │    │            └── Extend app on a Mac or computer, acting as host
                              │   │    │    │                  └── iPhone, iPad, Apple TV, Samsung/LG TV
                              │   │    │    └── Space Station (telemetry)
                              │   │    └── Ting (requests for a device, wake requests)
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
| Extend apps | Per OS, see §7. The Mac and Linux apps run the device engine. | Downloads on the website |
| The device engine | Silicon Extend's fork of an MIT-licensed device automation project, in `vendor/extend-engine` (package `silicon-extend-engine`; `FORK.md` names the project and lists every change). Its settings are `EXTEND_ENGINE_*` (the fork's own `AGENT_DEVICE_*` names still work). | Ships inside the Mac and Linux apps |

The device and the Silicon never talk directly. Both connect outward to the Extend service, so
neither needs to be reachable from the internet and they never need to share a network.

### Why a relay and not the device engine's own remote mode

The device engine can already run against a daemon on another machine. That mode expects the client to
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
| `devices` | `device_id`, team handle, owner Carbon id, name, OS, kind, visibility, `pair_ttl_days`, `last_activity_at`, `paired_at`, `version`, `host_device_id` (for devices paired through a computer), setup state, app version, credential hash. 1.1: one row per pair; see below |
| `device_access` | (`device_id`, Silicon id), granted by, granted at. 1.1: (`device_id`, Team, Silicon id) |
| `device_locks` | `device_id` primary key → `session_id`. The unique key is what enforces one Silicon at a time. 1.1: unique per physical device |
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

### 1.1: pairs, physical devices and Teams

One new world migration (schema version 4) and one global migration. Earlier migrations are never
edited.

- **A `devices` row is one pair**: one Carbon's device, with its own `device_id`, credential (and a
  rotated one not yet confirmed, `next_credential_digest`), owner, name, lifetime, `last_used_at`,
  removal, `wake_muted`, and for a carried device `hardware_key`, `duplicate` and
  `provisional_until`. The new `instance_id` names the physical device; pairs of one device share
  it, and a unique index allows one live pair per Carbon per device. Capabilities, setup and OS
  facts stay per row: the app writes the same hello on every pair's connection, and a new pair
  copies them from a live sibling so it is ready at once. `last_activity_at` is written on every
  live pair of the device at once (a pair's lifetime counts the device's activity, §4).
- **`devices.team`** is now only the Team selected when the pair was made. It authorizes nothing;
  it stays because 1.0 apps need `DeviceSelf.team` non-null, and for a rollback to 1.0.0.
  **`devices.visibility`** is always `personal`; a trigger enforces it for any writer, 1.0.0 too.
- **`device_instances`** (new): one row per physical device, with `side_salt` (keys side tags;
  never leaves the service), the awake state (`awake`, `sleep_state`, `awake_changed_at`, and the
  `awake_run`/`awake_seq` of the last frame applied), and `last_wake_alert_at`.
- **`world_settings`** (new): values of the world itself, kept by a clean: `hardware_salt`.
- **`device_access`**: one grant per (pair, the Silicon's Team, Silicon). `granted_by` is always
  the pair's owner. The Team is the key's second column.
- **`device_locks`**: one row per physical device (`instance_id` unique). It is the one-Silicon
  lock across every pair and Team; `device_id` is the pair the session runs through.
- **`sessions`**: unchanged. `device_id` is the pair; `team` is the Silicon's Team. A session's
  **side** is (its Team, the owner of its pair).
- **`activity`**: rows stay per pair, so each Carbon's log is their own side; `team` is the acting
  Silicon's Team. Details never carry another side's Silicon, Carbon, Team or session.
- **`requests`**: `team` is the requester's Team and `device_id` its pair. New columns say where it
  went: `routed_to` (`holder` or `carbon`), `routed_to_id` (the Carbon), `holder_device_id`,
  `holder_team`, `holder_session_id`, and the Ting's Team, frozen body and next attempt. A request
  routed to a Carbon keeps a fixed text in `to_id` and no `session_id`, so no column 1.0.0 reads
  ever holds another side's identity.
- **`wake_requests`** (new): one row per request (open, woken, expired, withdrawn, declined), with
  its asks, expiry, what the device did with it, and the Carbon's and the answer's Ting deliveries.
- **`ting_recipients`**, **`ting_type_status`** (new): whether Extend's Tings reach a member in a
  Team, and which of Extend's Ting types Ting reported missing in a Team.
- **`membership_checks`** (new): IAM's last definite answers about a member of a Team (§9).
- `extend_global.enrollments` gains `instance_id` and `from_device_id`: an enrollment started by
  "Pair with another Carbon" adds a pair to that device.
- A clean truncates the new per-world tables too, and keeps `world_settings`.

**Lock order.** Every transaction that changes who may use a device takes its locks in this order:
the physical devices' rows (`device_instances`, `FOR NO KEY UPDATE`, in `instance_id` order; for a
session start, the computer and every device it carries), then `devices`, then `device_access`, then
`sessions` and `device_locks`, then `wake_requests`. Session start, grant and revoke, unpair, the
Team-leaving revocations, wake create and answer, the awake handler, carried-device linking and
credential rotation all follow it. Every change that ends access takes the device row first, so it
serialises with a session start's re-check, and nothing can deadlock.

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
| Owner leaves the team | IAM webhook, then confirmed by introspection. **1.1: no longer ends the pair.** A device belongs to the Carbon, not to a Team; the grants that Carbon gave in that Team end instead (§9). |

Activity means a command in a session, or the owner changing the device's settings. A device
that's merely online doesn't count. 1.1: a session or command through any Carbon's pair keeps every
pair of that device alive, because `UNDERSTANDING.md` ends a pair when "the device goes without
activity" (a family TV B's Silicons never use stays paired to B while A's Silicons use it). "Last
used" stays per pair, and the owner's own settings changes bump only their own pair.

In 1.1 each of these ends one pair only: its sessions, its grants and its open wake requests end,
its credential stops working, and `unpaired` goes to that pair's connection. The physical device and
other Carbons' pairs of it are untouched; when the last pair ends, the app shows the pairing screen.

### Pairing another Carbon (1.1)

A device can be paired to several Carbons (`UNDERSTANDING.md`, Pairing), each with their own pair.

```
 Extend app (paired to A)            Extend service                         Website (Carbon B)
 POST /device/enrollments ─────────► enrollment tied to this physical device
   (A's device credential)           and to its world
          ◄──── enrollment_id, secret, code (as a first pairing)
 shows a warning, then the code                     ◄──── POST /pairings {code, name}
                                     creates B's pair: same instance_id,
                                     capabilities copied, ready at once;
                                     "another_carbon_paired" on A's pair
          ◄──── "paired" {B's device_id, B's credential}
 keeps both credentials, one socket each
```

- The app proves it belongs to the device by holding a live credential of it; it never names the
  device itself. The code pairs only into the device's world.
- B entering a code for a device B already paired gets `409 conflict`, naming only B's own pair; two
  of B's claims racing get one 201 and one 409, never a 500.
- A carried device has no credential: B pairs the computer, then adds the device through B's own
  pair of it, and the computer's report links the two (§7).
- Each Carbon sees only their own side. Owners see "Also paired by another Carbon" with no names,
  and their log records "another Carbon paired this device". The device's own screen lists every
  Carbon it is paired to, as `UNDERSTANDING.md` asks.

### The test device limit in 1.1

The limit of 5 counts physical devices: another Carbon's pair of a device already paired in the
environment never counts. A carried device added by a second Carbon while the environment is full,
through a computer that already carries the same device for another Carbon, is accepted
provisionally (setup step `recognising`) and counts nothing once the computer links it; if it isn't
linked within `EXTEND_TEST_LINK_WINDOW_S` (120 s), it is removed with the limit's message.

### Setup steps and retry (1.1)

A setup step's `error` is one or two sentences for the Carbon: what is wrong and what to do ("The
iPhone is locked or not connected by cable. Unlock it and keep it plugged in."). Environment
variables, file paths, build commands, exit codes and stack traces go to the app's or agent's log.

A Carbon who paired the device retries a failed step from the website, with
`extend device setup <id> --retry [--step <key>]`, or (Android) with the Retry button on the app's
own setup screen. `POST /api/v1/devices/{device_id}/setup/retry` checks the step exists and has
failed, that the device (or, for a carried device, its computer) is online and its app advertises
`setup_retry` in its hello, and that no retry of the device went out in the last 5 s; then it sends
the device `setup_retry` and answers 202 with the steps it asked for. The service changes no step
itself: the device reruns the step at once and reports progress as usual (docs/device-protocol.md).

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

**1.1: across pairs, Teams and lock groups.** The lock is per physical device, so one Silicon uses a
device at a time whichever Carbon's pair or Team it comes through. A computer and the devices it
carries form one **lock group**: a Silicon can't start on any member while a Silicon on another side
(another Team, or another Carbon's pair) uses any member, so a terminal on the computer can't watch
another side's iPhone or TV session. Same side, it is as in 1.0. A session start locks the group's
device rows first, re-checks the grant and the owner's membership under that lock, then inserts the
session and the lock (§3, lock order).

`device_in_use` names the holder only to the same side (the same Team and the same Carbon's pair)
and to the holder itself; for anyone else it says "Device 7c1e09ab (Living room TV) is in use. Only
one Silicon can use a device at a time." with `details.in_use.hidden` and the request-send hint.

**Stopping (1.1).** Every Carbon who paired a device can stop the Silicon using it (`UNDERSTANDING.md`,
Always visible), from the website, the CLI, or the device's own Stop. A remote Stop of a computer
also ends the sessions on devices it carries that the stopper paired, but never one on a carried
device the stopper didn't pair: that device's own Carbons, or the computer's own Stop, end it. The
stopped session's pair logs it as stopped by `extend` with `{"stopped_by": "another_carbon"}`,
naming no one, and the Silicon's hint says "a Carbon who paired the device stopped it".

**Requests for a device in use (1.1).** When the asking Silicon is in the same Team as the Silicon
using the device and was given access by the same Carbon (the same pair), the request goes to that
Silicon through Ting, as in 1.0, and the asker sees which Silicon it is. Otherwise it goes to the
Carbon who gave the Silicon using the device access, who can stop the session:

- The asker sees only "in use" and "Sent to the Carbon who gave access to the Silicon using it",
  even when that Carbon is its own; never the Carbon, the Silicon using it, or its session.
- That Carbon sees the asking Silicon's id and its reason (the Carbon's decision of 2026-09-27;
  `UNDERSTANDING.md`: "with the requesting Silicon's name and reason"), on the website, in
  `extend device requests` and in the Ting. Nothing names the Silicon using the device, its Team or
  its session.
- The Ting (`extend.device.requested`) goes at once from the asking Silicon, with the login it just
  used, when the asking Silicon and that Carbon share a Team (it goes in that Team). Otherwise it goes
  from the Carbon's own login, as a notification to themselves, in the Team where they gave the
  holder access: at once when Extend holds that login, otherwise at their next use of Extend. The
  website and CLI show the request at once in every case. The Silicon using the device is never the
  sender.
- The 60 s repeat fold and the idempotency route keys include the Team, so nothing crosses Teams.

### Relaying a command

1. The CLI posts `{command, args, timeout_ms}` with the Silicon's access token. `args` are the
   command-line tokens after the name, exactly as the device engine's command line takes them.
2. The service introspects the token with IAM (cached no longer than 30 s, and dropped immediately
   on a relevant IAM webhook), then checks: the session is the caller's, it is `active`, the caller
   still has access (1.1: its grant in the session's Team, and the owner-active check of §9), the
   device is online, and the command is in the device's capabilities.
3. The service sends `{"type":"command","id":<command_id>,"timeout_ms":…,"command":…,"args":[…],"upload_ids":[…]}`
   down the device's WebSocket and waits for the matching `result`.
4. The app runs it (on Mac and Linux, through the device engine) and replies. Files it produced (screenshots, recordings,
   logs, replay scripts) are uploaded to `PUT /api/v1/device/artifacts/{upload_id}` first, using
   upload ids the service included with the command.
5. The service stores each file in Briefcase on the Silicon's behalf (§6), writes the command to
   the activity log, resets the idle timer, and returns the result with Briefcase links.

Commands in one session run one at a time, in order. A second command sent while one is in flight
waits for it; this matches the device engine, whose own daemon serialises a session.

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
stays. 1.1: on a computer several Carbons paired, `terminal` is only on its first pair (the Carbon
who installed Extend there); every other pair lists it as missing (§9a).

### Device engine commands Extend does not expose

The parts made for app developers: `boot`, `shutdown`, `web …`, `viewport`, `react-native …`,
`react-devtools …`, `metro …`, `cdp …`, `perf …`, `trace …`, `network dump`, `debug symbols`,
`audio probe`, `push`, `trigger-app-event`, `install-from-source`, `settings …` (simulator helpers),
`fold`, `orientation`, `action-button`, `prepare ios-runner`, `mcp`, `doctor`. Discovery and connection are replaced by Extend's own commands:
`devices` → `extend device ls`, `connect`/`disconnect` → `extend session connect`/`disconnect`,
`session list` → `extend session ls`. Any of them sent through Extend returns
`404 unknown_command` naming the Extend replacement where there is one.

### Waking a device (1.1)

A device can be paired and online but not awake: a phone with its screen off or locked, a computer
asleep or locked or showing another account, a TV in standby (`UNDERSTANDING.md`, Waking a device).

**Awake is information, never a gate.** The terminal, Android debugging and every other command keep
working while a device isn't awake, because they run as the Carbon's own account. A command that
needs the screen fails with the device's own error. When the device is known not to be awake (or
was not awake when last seen), `device_offline`, `unsupported_on_device` and a failed command's
answer add the **wake hint**: "<name> isn't awake (<state> since <time>). Its Carbon can wake it:
extend --team <T> device wake <id> --reason \"...\"". A session starts on a device that isn't awake
(the CLI prints the hint).

**Reporting.** Apps 1.1 and later report `awake` on every pair's connection, after each hello and
on every change, with `input_seen` and an ordering (`run`, `seq`); computers report the devices
they carry in `attached` (docs/device-protocol.md, "Awake", has what each OS reports). A device reads
awake (yes), not awake (screen off, locked, asleep, standby, another account), or unknown: offline
(with the last state seen), an app older than 1.1, or a device Extend can't read yet (iPhone, iPad:
`wake_detectable: false`).

**Asking.** `extend device wake <id> --reason "..."` (up to 300 characters), by a Silicon with a grant
on that pair in its Team T:

- Where it can, the device shows a notification with the Silicon's name and reason: an Android phone's
  lock screen (the name always, the reason only where the phone shows notification contents there),
  or a Mac, Windows or Linux system notification. A TV, and a device paired through a computer,
  shows nothing; the Carbon's Ting and the website carry it (for a carried device, "its computer must
  be awake too").
- The Carbon who gave the Silicon access gets `extend.device.wake_requested` through Ting, in T, sent
  as the Silicon.
- When the device is turned on or unlocked (an awake report that isn't `input_seen: false`), every
  open request on the physical device ends as woken, through any pair and in any Team, and each
  Silicon that asked gets `extend.device.woken` in its own Team, saying whether the device is free,
  already its own, or in use (then with the request-send command), naming no one.
- "It's awake" from a Carbon who paired it (website or CLI) is a fact about the device: it ends every
  open request on it the same way. This is how requests end on devices Extend can't read. "Decline"
  ends only the answering Carbon's own pair's requests and sends `extend.device.wake_declined`.
- Asking again after 5 minutes refreshes the request (new reason, ask count up, expiry reset); sooner
  is `429`. A request expires 30 minutes after the last ask. `--cancel` withdraws it. It is also
  withdrawn when access ends, the Carbon leaves the Team, the pair ends, the Carbon turns wake requests
  off, or the Silicon starts a session while the device's state is unknown.

**Limits (the defaults the Carbon accepted).** The device sounds at most once every 15 minutes. At
most one wake Ting per pair and Team every 15 minutes: later asks are covered by it. At most 6 wake
Tings per Carbon per hour: an ask over that is accepted and its Ting waits for the hour's window
(deferred; the Silicon sees pending), so an ask is never refused because of what other Teams did. A
Carbon can turn wake requests off for a device, or for one Silicon. Ting delivery is best effort,
with retries (§9, C2).

**Extend never wakes a device.** Its own code never turns a screen on or sends a power-on: the Apple
TV's power button sends sleep only while awake and is refused while asleep, a Samsung TV in standby
gets no power key, Android TV's power key stays refused, LG power only turns the TV off, and no app
uses a full-screen intent, a screen-on flag or a wake lock that turns the screen on.

**Keeping the screen on.** While a Silicon uses a device that is awake, the app keeps the screen
from turning off on its own (an accessibility overlay on Android, a display assertion on Mac and
Windows, an idle inhibit on Linux; docs/device-protocol.md). It never turns a screen on, and the
Carbon can still lock it; it is released at session end, Stop, a takeover, or when the device stops
being awake.

**Sides on the device.** A wake request's frame carries a side tag, and so does `session_started`.
While a Silicon of another side uses the device (or its lock group), the service leaves the asking
Silicon and reason out of the frame, and the app redacts its screen and OS notification from
`session_started` before the session's first command, so neither a lock screen nor a notification
history shows the holder another side's request.

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

**1.1: Teams and several Carbons.** A file is stored in the Team the Silicon acted in (the session's
Team) and shared with the Carbon who gave that Silicon access: the owner of the pair the session ran
through, whichever Carbons paired the device. A Carbon lists the files made through their own pairs
in every Team, each tagged with its Team, and reads one only as themselves, in the file's Team: a
Carbon whose Extend login doesn't reach that Team gets "Sign in to Extend for <team> to open files
made there". Another Carbon who paired the same device never sees them.

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

Every app does four things: keep the WebSocket to the service open, run commands (on Mac and
Linux through the device engine), show the in-use indicator with a Stop button, and offer "Revoke
pair". It starts at boot and reconnects on its own. 1.1 adds: one socket per pair when several
Carbons paired the device, "Pair with another Carbon", revoke per Carbon, the awake report, the wake
notification, keeping an awake screen on during a session, and setup retry (docs/device-protocol.md).

### Connection protocol (`WSS /api/v1/device/connect`)

- Auth: `Authorization: Extend-Device <device_credential>` on the upgrade request.
- Frames are flat JSON text: `{"type": ..., <fields>}` (docs/device-protocol.md). Files never travel on the socket.
- Heartbeat: the service pings every 15 s; the device is **offline** after 45 s without a pong.
- Reconnect: exponential backoff from 1 s to 60 s with full jitter.
- One live connection per pair (1.1: an app with several pairs keeps one per pair, each with its
  own credential, and each is a 1.0 connection plus the 1.1 frames). A new connection replaces the
  old one, which gets `superseded`; when the old one had answered a ping within 30 s, the pair's log
  records `connection_replaced`, so a takeover of a pair's connection is visible to its Carbon.
- Close codes (as built): `4401` unpaired, `4409` superseded, `4426` upgrade required, and `4503`
  when the device's test environment closes (disabled, or waiting for Honeycomb's readiness): the
  pair is kept and the app reconnects with backoff. The handshake and the device's HTTP routes
  answer `503 testing_environment_not_ready` for such an environment.

| Direction | `type` | Meaning |
|---|---|---|
| device → service | `hello` | App version, OS and version, device engine version (`engine_version`; 1.0: `agent_device_version`), capabilities, setup state, 1.1 `features` (`setup_retry`) |
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
| device → service | `awake` (1.1) | Awake or not, why not, whether input was seen; ordered by `run` and `seq` |
| device → service | `wake_request_shown` (1.1) | Whether the device could show a wake request, and why not |
| device → service | `credential_saved` (1.1) | The app stored the credential the last `credential` frame gave |
| device → service | `attached` (1.1 fields) | A carried device's `awake`, `sleep_state` and `hardware_key` |
| service → device | `session_started.side` (1.1) | The session's side tag, for redacting other sides' wake requests |
| service → device | `wake_request` / `wake_request_ended` (1.1) | Show or remove a wake request; redacted while another side uses the device |
| service → device | `credential` (1.1) | Computers several Carbons paired: this pair's new credential |
| service → device | `setup_retry` (1.1) | Run a failed setup step (or every failed step) again now; only to apps whose hello lists `setup_retry` |

### Per device

As built on 2026-09-27 (the first draft's rows for Android, Mac, Windows and Linux described plans
that changed; §13 says why):

| Device | How commands are carried out | Indicator |
|---|---|---|
| **Android phone and tablet** | The app's AccessibilityService reads every window as an element tree (same `@eN` refs and snapshot shape as the device engine), taps and gestures with `dispatchGesture`, presses back/home/recents, takes screenshots, and reads notifications through a notification listener. **Android debugging** (the Carbon pairs Wireless debugging from the app once; a TV can use its TCP port) connects the app's own ADB client on the device, which adds `adb`, `install`/`reinstall`, `logs` and `record`. Recording runs supervised `screenrecord` segments of up to 180 s and joins them into one MP4, bounded to 30 minutes or 1 GiB; there is no per-recording consent prompt. Without debugging connected those capabilities are reported missing with "connect Android debugging". | Ongoing notification with Stop |
| **Android TV, Google TV, Fire OS** | Same, plus the remote's arrows and select through the accessibility D-pad actions (Android 13+). With Android debugging connected, every remote button but Power (Menu too, and older TVs' D-pad) is a real key press through `input keyevent`; Power is always refused. The display screen is an activity inside the app. With Android debugging: `adb`, `install`/`reinstall` and `logs`; no recording on TVs. The app is named **Silicon Extend TV** on a TV. | Corner badge drawn as an accessibility overlay (no extra permission); Stop in the app |
| **Mac** | The device engine's macOS driver through its signed native helper, with no XCTest runner and no UI Automation setup: Accessibility for the element tree, pointer input and text entry (text passed over stdin, focus checked before each key event), ScreenCaptureKit for screenshots and H.264 recording of one app or the display. The only setup steps are Accessibility and Screen Recording for Silicon Extend. A session starts on the frontmost app; `open <app>` binds the named app; links open with the system and the session follows the frontmost app. Terminal commands run as the logged-in user. | Menu bar icon changes; banner; Stop in the menu |
| **Windows** (built by Extend) | UI Automation for the element tree, `SendInput` for mouse and keyboard, GDI for screenshots, Win32 for the clipboard, the Start menu and shell for apps, `cmd.exe` for the terminal. Mapped onto the same device engine command set and snapshot shape. `record`, `logs`, `alert` and `replay`/`test`/`batch` are reported missing. Compile-checked and unit-tested only; it has never run on Windows. | Tray icon changes; banner with Stop |
| **Linux** | The device engine's Linux driver: AT-SPI2 for the element tree, xdotool (X11) or ydotool (Wayland) for input, a screenshot tool (gnome-screenshot, scrot or ImageMagick; grim on Wayland), xclip/xsel or wl-clipboard. On X11, recording with ffmpeg (libx264 from `x11grab`): the whole screen, or one app's window through XComposite so windows over it are not recorded (an app that can't redraw its whole window within 5 s is refused, with "record the whole screen instead"). `--quality` picks the bit rate: `normal` 8 Mbit/s, `high` 20 Mbit/s. Wayland recording (the ScreenCast portal) is not built and is reported missing, as are `logs`. PTY for the terminal. | Banner with Stop |
| **iPhone, iPad** (via Mac) | The Mac's app runs the device engine's iOS driver: its helper (1.1: Silicon Extend Helper, bundle id `com.teamofsilicons.extend.helper`, replacing the fork's older helper, which is removed after the new one installs) is installed on the iPhone once over USB, then reached over Wi-Fi. | On the Mac's app and the website |
| **Apple TV** (via Mac) | The Companion protocol for apps and remote buttons, and AirPlay for pictures and videos, from the Mac on the same network. The Apple TV shows a code the Carbon enters once. | On the Mac's app and the website |
| **Samsung TV** (via computer) | Tizen's local remote-control WebSocket (ports 8001/8002). The TV asks the Carbon to allow the connection once and issues a token. | On the host's app and the website |
| **LG TV** (via computer) | webOS's local SSAP WebSocket (ports 3000/3001). The TV asks the Carbon to accept once and issues a client key. | On the host's app and the website |

A device paired through a host is offline whenever its host is offline or can't reach it.

**Names a Carbon or Silicon sees (1.1).** Everything says Silicon Extend: the apps, CLI output, help
and errors, the website, logs a Carbon reads, the helper apps a device shows (the iPhone and iPad
helper "Silicon Extend Helper"; the device engine's Android helpers "Silicon Extend Keyboard" and
"Silicon Extend Snapshot Helper", packages `com.teamofsilicons.extend.imehelper` and
`…snapshothelper`, which replace the fork's older helpers), process and file names, and settings: the
engine reads `EXTEND_ENGINE_<X>` (its fork's `AGENT_DEVICE_<X>` names still work, and the new name
wins), the desktop agent finds the engine through `EXTEND_ENGINE` or `config.json` `engine` (1.0's
`EXTEND_AGENT_DEVICE` and `agent_device` are still read) and keeps its state in `engine/` (1.0's
`agent-device/` is renamed once). The protocol calls its version `engine_version`. Internal code names
nobody sees may keep the fork's spelling, and the fork's MIT licence and attribution stay where the
licence requires them (`vendor/extend-engine/LICENSE`, `FORK.md`, `THIRD_PARTY_NOTICES.md`).

As built (2026-09-27): the Mac, Windows and Linux app turns start at login on by itself once paired,
unless the Carbon turned it off (the window's switch, the menu, `run --no-autostart`); a copy run
from App Translocation or a disk image is never registered, and `--headless` only with
`--autostart`. On a Mac `record start --quality normal|high` is the device engine's `medium|high` (the
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
- **1.1.** Every pair of a device is in one world ("Pair with another Carbon" inherits its world),
  and the device limit counts physical devices (§4). A clean, restore or purge also forgets which
  members Extend registered with Ting and its cached membership answers for that environment, and a
  clean removes every pair, so each app returns to its pairing screen. Ting's own clean removes its
  types and grants: Extend's four Ting types must be registered again in each Team of the environment
  after every clean (`docs/operations.md`), and Silicons are registered again at their next session.
  The world's hardware salt survives a clean. Every new handler and background task checks that its
  world is open first, so a clean during an awake report, a rotation, a pair enrollment or a pending
  Ting leaves nothing behind and sends nothing.

---

## 9. Revocation

Anything that ends access takes effect on the next command at the latest, and usually before it:

| Event | Source | Effect |
|---|---|---|
| Owner removes a Silicon's access | API | That Silicon's session on the device ends at once (`access_removed`). 1.1: per Team (`?team=`), or every Team's grant |
| Device removed or pair revoked | API | Session ends; pair ends (§4). 1.1: that Carbon's pair only |
| Carbon taps Stop | Device socket or API | Session ends (`stopped_by_carbon`). 1.1: any Carbon who paired the device (§5) |
| Silicon logs out, or its IAM session is revoked | IAM webhook | Its sessions end (`silicon_logged_out`) |
| Carbon logs out of Extend (1.1) | `POST /api/v1/auth/logout` | The running sessions of the Silicons that Carbon gave access to end (`stopped_by_carbon`), through that Carbon's pairs, in every Team; another Carbon's Silicons on the same device keep theirs. Devices and grants stay |
| Silicon removed from the team or deleted | IAM webhook | Its sessions end (`left_team`); its access grants are removed. 1.1: in that Team only |
| Owner Carbon leaves the team | IAM webhook | All the Carbon's devices in that team are unpaired. **1.1:** the grants that Carbon gave in that Team end, on all their pairs, and those sessions end (`left_team`); open wake requests there are withdrawn. The devices stay paired, and grants in their other Teams stay |

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

### Membership in 1.1: acting only on definite answers

A grant needs the Carbon's Extend login to reach the Silicon's Team (checked live) and the Silicon to
be an active member there. Access then ends when the Silicon or the Carbon leaves the Silicon's Team
(`UNDERSTANDING.md`, Access). Extend learns it four ways: the IAM webhook (re-checked with IAM), a
Silicon's refused login on the session routes (above), the owner-active check at every use, and a
sweep.

- **A membership answer is one of three.** `active`: IAM answered 200 for the member's directory
  entry. `gone`: IAM answered 404 for it, and the same reader's read of its own entry in the Team
  succeeded just before, so the reader can read the Team's directory. `unknown`: a 403, any other
  error, or nobody signed in to ask. Nothing is ever deleted on `unknown`.
- **The owner-active check.** At session start, every command, a wake request and a request send, a
  Silicon acting in Team T through a Carbon's pair asks whether that Carbon is still active in T,
  reading as the Silicon itself. `active` answers are cached for `EXTEND_OWNER_CHECK_CACHE_S` (30 s)
  per world, Team and Carbon; an IAM event about the Carbon and a test clean clear the cache. `gone`
  refuses the call at once (`403 no_access`, "c:alice, who gave you access to Living room TV, is no
  longer an active member of labs, so her access grants in labs ended.") and ends that Carbon's
  running sessions in T through any of their pairs; the grants are deleted only when a second reader
  confirms `gone`. If the second reader says `active`, the grants stay, and for the next 10 minutes
  a Silicon reader's `gone` for that Carbon and Team counts as `unknown` (and is logged as an error,
  naming both readers). `EXTEND_OWNER_CHECK_AT_USE=false` turns the check off, in case IAM hides a
  Team's Carbons from its Silicons.
- **The sweep.** Every `EXTEND_MEMBERSHIP_SWEEP_HOURS` (6), the scheduler checks every (Silicon, Team)
  and (granting Carbon, Team) with grants, and deletes grants only after two `gone` answers at least
  10 minutes apart, from different readers when there are two.
- So one Silicon's view of IAM can never wipe a Carbon's grants, and a missed webhook can't keep a
  session running past the next use plus 30 seconds.
- **Revocation per Team.** A Silicon that left Team T loses its grants in T only. A Carbon who left T
  loses the grants they gave in T on all their pairs; the devices stay theirs.
- **Logout (the Carbon's decision, 2026-09-27).** A Silicon's logout ends its own running sessions.
  A Carbon's logout of Extend, on the website or with `extend logout`, ends the running sessions of the
  Silicons that Carbon gave access to: only that Carbon's side, never another Carbon's. Nothing is
  unpaired or revoked; the Silicons can start again once the Carbon is back, or right away (a Carbon's
  login isn't needed for a Silicon to use a device it has access to).

---

## 9a. Shared computers (1.1)

A Mac, Windows or Linux computer can be paired by several Carbons. A Silicon's terminal there runs as
the computer's own account (the Carbon's decision), which can reach what the Extend app keeps for
every pair. `UNDERSTANDING.md` and the Carbon's decision of 2026-09-27 settle who gets the terminal,
and Extend limits what outlives a session:

- **The terminal belongs to the installer's Silicons.** On a computer whose live pairs belong to two or
  more Carbons, only Silicons given access through its first pair (the one made by the app's first
  enrollment: the Carbon who installed Extend on it) get `terminal`. Every other pair reports it
  missing, with "Several Carbons paired this computer. Only Silicons given access by the Carbon who
  installed Silicon Extend on it can use its terminal. The screen, keyboard and apps work as usual.",
  and the service refuses `terminal` through those pairs. When only one Carbon's pair is left, the
  terminal is theirs again; when the first pair has ended and several Carbons remain, no pair has it
  (open question 16). This limits Extend's `terminal` command. A Silicon with the screen and keyboard
  can still open a terminal app, so the warnings below stay.
- **Session processes end with the session.** Every process a session's terminal started carries a
  per-session mark (a Job Object on Windows) and is killed at session end. Jobs handed to the OS's
  own schedulers (`launchd`, `schtasks`, `systemd-run`, `cron`, `at`) can't be contained.
- **Credentials rotate.** At the end of every session on a computer whose pairs belong to more than
  one Carbon, each pair gets a new credential over its own connection, and the old one stops working
  once the app confirms. A copy made during the session is useless afterwards. If the copy holds the
  pair's connection at that moment, or connects first while the app is offline, it receives the
  rotation: that shows as `connection_replaced` in the pair's log, and as the real app's Reconnect.
- **Take-overs are visible.** A connection that replaces a pair's live connection is logged as
  `connection_replaced` on that pair, and the app shows Reconnect for it.
- **Carbons are warned** before a second Carbon pairs a computer, in the app and on the website: the
  installer's Silicons use its terminal as the OS user and can reach what that user can, including
  the app's other pairs; other Carbons' Silicons use its screen, keyboard and apps; share a computer
  only with Carbons you trust. Other devices get the general note: "Silicons any Carbon gives access
  to can use this whole device, including what others leave on it."
- **What Extend keeps off the device.** Status and log files hold no wake requests, reasons or other
  sides' Silicons; each command's work directory and a session's recordings are wiped at its end;
  the service never receives raw hardware ids.
- Android pairs are never rotated: their credentials are sealed with the app's Android Keystore key
  in app-private storage, which Android debugging can't read.

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
- **1.1 keeps 1.0 consumers under test.** Before the client's fixtures are regenerated for 1.1, the
  released 1.0.0 ones are frozen in `contracts/v1/client-1.0.0/` and replayed too; the 1.0.0 device
  fixtures stay as they were, and the 1.1 apps' fixtures are new files beside them. The compatibility
  matrix is unchanged: client crate and CLI `>=1.0.0, <2.0.0`, device apps from 1.0.0. The new
  frames, fields and routes are additive, and no error code, end reason, capability, visibility or
  OS value was added, because 1.0 readers refuse values they don't know.

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
- **Token refresh** is serialised with a lock file, `refresh.lock` (created exclusively; one older
  than 30 s is taken as left by a dead process). A process that can't get it within 10 s refreshes
  anyway: the refresh sends an idempotency key derived from the refresh token, so two refreshes of
  one token get the same answer. A process only removes a lock it created. The replacement pair is
  written atomically (write temporary, `fsync`, rename), as IAM's client docs require.
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
  website's settings. Off sends `X-Extend-Telemetry: off` on every request: the service then drops
  the caller's own events and records nothing about that caller's commands. The CLI posts events to
  `POST /api/v1/telemetry` so no ingest key ships in the CLI. Every event carries its source (`cli`,
  `client`, `app`, `service`, `web`), step, outcome and duration. The service's command events also
  carry the command name, device OS and the session and command ids; the CLI's carry the command
  name. Never typed text, clipboard contents, screen contents, tokens, codes or secrets.
- `extend report "<message>" [--pr <url>]` stores the report and emails it through Postmark to the
  three addresses in `UNDERSTANDING.md`. In a test environment the email is simulated. As built
  (2026-09-27) the default list (`EXTEND_REPORT_RECIPIENTS`) sends to `shubhastro2@gmail.com`, where
  `UNDERSTANDING.md` writes `shubhastro2@gmails.com` (Open question C6).
- **Redaction in the activity log:** text typed with `fill` and `type`, and text written with
  `clipboard write`, is replaced with `[redacted N chars]`. Everything else is logged as sent.
- Secrets never appear in URLs, logs, audit rows, telemetry or stored webhook bodies. Credentials and
  enrollment secrets are stored as SHA-256 digests. 1.1: the `credential` frame is never logged (its
  Rust type prints only the `edc_` prefix), apps keep wake requests and their reasons in memory only,
  and the service never stores or logs a carried device's raw hardware id. Pairing codes are stored in plain text: the
  unpaired app has to be able to fetch its current code by polling, a code lives 5 minutes, works
  once, and guessing is rate limited.

---

## 13. As built (2026-09-26, updated twice on 2026-09-27)

Differences from the first draft, each deliberate:

- **Commands are CLI tokens, not structured input.** A command is `{command, args}` where `args` are
  the device engine's command-line tokens. The engine's structured inputs differ per command and
  change between versions; forwarding tokens keeps its own parser the authority. Flags that pick a
  device or session inside the engine are refused. Files a caller sends (a replay script, an image for a TV)
  travel as `attachments` and are referenced in args as `attachment:<name>`.
- **Self-destruct is enforced by Extend.** Briefcase's delegated upload takes no self-destruct time,
  so Extend records the time, deletes the file through Briefcase's delegated trash when it passes,
  and `extend file keep` cancels that. Open questions 1–2 still stand for Briefcase itself.
- **One service instance.** Device sockets and waiting commands live in the process
  (`docs/operations.md`).
- **Android reads the screen with an AccessibilityService**, not the device engine's helper over
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
- **Mac uses the device engine's native helper for everything**: text entry through Accessibility and
  recording through ScreenCaptureKit, so neither Xcode nor UI Automation is needed (the first draft
  expected the XCTest runner for typing and recording).
- **Computer cleanup.** When the device engine can't release a Mac or Linux computer after a session
  (close fails, then a forced release fails), the app keeps working but reports every capability
  that needs the engine as missing, with one reason, and retries in the background; `terminal`
  and `takeover` stay available.
- **Packaged runtime identity.** Mac and Linux packages stamp the device engine's version with a
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

1.1.0 (draft, 2026-09-27). Deliberate choices in the 1.1 design, each described in its section; this
list is checked against the build before release:

- **One pair per Carbon, one row per pair** (§3). A physical device is an instance that pairs point
  to, so every 1.0 route, frame and credential keeps working, and a 1.0 app is the one-pair case.
- **One connection per pair**, not one connection carrying several pairs: no 1.0 frame or handshake
  changes, uploads stay bound to one credential, and a reconnect never half-replaces a socket other
  pairs use. The cost is one socket per Carbon (at most 8).
- **The app proves the device by holding a credential**; "Pair with another Carbon" is an enrollment
  started with one, so no Carbon can attach to another's device without it showing a code.
- **Carried devices are matched by a keyed hardware id** (`hardware_key`) with a salt every computer in
  the world gets, and a device can be carried by one computer only (a second computer's pair is
  refused, naming no one), so one lock group holds it and one Silicon at a time holds.
- **Side tags on frames** let an app redact other sides' wake requests before a session's first
  command, without the service naming a Carbon or Team to the device.
- **No awake gate, no Extend wake.** Awake is information and a request flow; commands, the terminal
  and Android debugging never wait for it.
- **Requests routed to a Carbon** keep a fixed text in the 1.0 `to` column and no session, so a
  rollback to 1.0.0 can't leak another side; the recipient Carbon sees the asking Silicon (the
  Carbon's decision).
- **Membership acts on definite answers only** (§9), with a second reader before deleting grants.
- **Ting retries freeze the first body** (a retry after a rename still matches Ting's idempotency
  key), back off, and don't count attempts with no login held; a missing Ting type is shown with its
  register command instead of being retried silently.
- **No new error code, end reason, capability, visibility or OS value**: 1.0 readers decode those
  strictly. A Carbon's logout ends sessions as `stopped_by_carbon`.
- **Rollback to 1.0.0 needs a down step** (`deploy/rollback/1.1-to-1.0.sql`, listed in
  `deploy/aws/README.md` and `docs/deployment.md`): it removes grants 1.0.0 would misread, fails routed
  requests still pending, withdraws open wake requests, and deletes waiting "Pair with another Carbon"
  codes. Triggers keep 1.0.0 safe while rolled back (devices stay personal, one Silicon per physical
  device).

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
  *1.1 changes the Ting side:* every Ting goes from a member who took part (the asking Silicon, the
  answering Carbon, or the recipient Carbon to themselves), never from a bystander such as the
  Silicon using the device; each has a chain of such actors to try. An attempt with no login held
  doesn't count, and a pending Ting goes out at the next signed-in call of any member in its chain.
  Retries back off (30 s, then 1, 2, 4 and 8 minutes, then every 8 minutes) and resend the exact
  first body; requests give up after 6 counted attempts or 24 hours, woken and declined Tings 30
  minutes after their request ended. A member who turned Extend's Tings off stops the retries until
  "Turn on". Ting's types are per Team: Extend's four types (`extend.device.requested`,
  `.wake_requested`, `.woken`, `.wake_declined`) must be registered in every Team it sends in, and
  in each test environment after every clean. Extend registers them itself with the login of a Carbon
  who is that Team's Ting manager, where Ting accepts that on the Carbon's behalf, and otherwise shows
  the exact command in Settings, on the device
  page, in `extend ting status` and in the delivery line of `request send` and `device wake` (the
  Carbon's decision of 2026-09-27). `docs/requests/ting-app-level-types.md` asks Ting for types
  registered once per app.
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
- **C9. Carbon logout (question 4).** *Settled for 1.1 on 2026-09-27:* a Carbon signing out of the
  website or the CLI ends the running sessions of the Silicons that Carbon gave access to, only on
  that Carbon's side (§9). The build ends them as `stopped_by_carbon`, since no end reason can be
  added without breaking 1.0 readers; confirm, or accept a new reason in API v2.

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
4. **"Logging out … ends it immediately."** *Settled on 2026-09-27* (C9): a Silicon's logout ends
   its own sessions; a Carbon's logout ends the sessions of the Silicons that Carbon gave access to.
   (How a Silicon's logout elsewhere is noticed: C1.)
5. **Team-visible devices.** *Settled by `UNDERSTANDING.md` on 2026-09-27:* a device belongs to the
   Carbons who paired it and nobody else sees it; 1.1 removes visibility (every device reads
   `personal`, and 1.0's "Team devices" list is empty).
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
10. **Physical Fire TV.** `UNDERSTANDING.md` says Extend adds it. The device engine's Fire TV support is
    limited to Amazon's virtual device, so screen reading on a physical Fire TV depends on Extend's
    fork of the Android helper working on Fire OS. Needs a hardware check early.
11. **iPhone signing.** The device engine's iPhone helper (Silicon Extend Helper) is an XCTest app that must be signed with
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
    `high` work everywhere `cli.yaml` offers them. The agent passes them to the device engine as
    `medium` and `high`; on a Mac (and a carried iPhone or iPad) that is the export quality, and the
    fork's Linux recorder now encodes at 8 Mbit/s (`medium`) or 20 Mbit/s (`high`), as Android's
    screenrecord does, instead of refusing the option.

### 1.1.0 (2026-09-27)

The spec's own open questions were settled by the Carbon's decisions of 2026-09-27 (see the top of
this file): logout (C9 above), requests routed to another Carbon (they see the asking Silicon), the
terminal on shared computers (only the installer's Silicons), and Ting types (per Team, registered by
Extend where the Carbon is a Ting manager, plus a request to Ting). What remains:

16. **The terminal when the installer's pair ends.** On a computer several Carbons paired, only the
    first pair's Silicons get the terminal. If that Carbon revokes their pair and two or more Carbons
    remain, no pair has the first pair, so no Silicon gets the terminal until only one Carbon is
    left. Keep that, or let the oldest remaining pair take it over?
17. **The terminal rule and the screen.** The rule removes Extend's `terminal` command from the
    other Carbons' Silicons, but a Silicon that can use the screen and keyboard can still open a
    terminal app on the computer. Is the warning before pairing enough, or should Extend also refuse
    `open` for terminal apps for those Silicons (a list that can't be complete)?
18. **iPhone and iPad awake state.** Extend reports "can't tell" for them until a lock-state reading
    is checked on a real device; their Carbon answers "It's awake" instead. Which device should that
    check use?
19. **App-level Ting types.** `docs/requests/ting-app-level-types.md` asks Ting and Honeycomb for
    notification types registered once per app. Until then, every new Team needs its Ting manager
    (or Extend, with a Ting manager's login) to register four types, and every test clean undoes it.
20. **A request routed to a Carbon who shares no Team with the asking Silicon** goes from the
    Carbon's own login, so it waits for that Carbon's next use of Extend when Extend holds no login for
    them. The website and CLI show it at once. Accept, or ask Ting for a way to notify a member without
    their login?
