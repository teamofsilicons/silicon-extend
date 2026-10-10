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
4. Ting setup stays visible and actionable. The original design assumed per-Team types and automatic
   OBO registration where authorized. Verification of Ting 0.1.9 corrected that dependency premise:
   types resolve per app and context across delivery Teams, and the published OBO catalog has no
   `types.register`. The supported path is the app-owning Team's manager CLI, with explicit guidance
   and persistent retry/error state (C2). This is a contract correction, not a new registration API.

It also adds setup retry and plain-language setup errors (§4), and calls the device engine Silicon
Extend's everywhere a Carbon or Silicon can see it: names, paths, settings and helper apps (§2, §7). Sections marked **1.1** describe the
design being built; §13 lists where the build differs once it lands. API v1 stays additive
(`api.yaml`), and 1.0 apps, CLI, client and website keep working.

**Reconciled review copy, 2026-09-28.** This copy proposes the completed 1.1 implementation
updates without changing the protected original. The API/CLI copies and patch beside it cover the
banner preference, stored display media, Android TV element clicks, first iOS screenshot attachment,
Ting's actual catalog scope and schema version 5. Historical 1.0 open questions below are retained
for context; they are not a fresh signing, authentication or publication approval request.

**Extend 4 review copy, 2026-10-10, for the Carbon's approval.** `understanding/TECHNICAL.md` is
unchanged; this copy proposes the move to Silicon Accounts and Silicon Apps. Extend 4 signs every
Carbon and Silicon in with Silicon Accounts (personal accounts keyed by a permanent uuid), has no
Teams and no Honeycomb test environments, and reaches Briefcase and Ting with Silicon Accounts
proofs. The sections rewritten for it are: identifiers owned by other services (§1), the 4.0 data
model (§3), files (§6), test environments (§8, removed), sign-in, sign-out and account changes (§9)
and API v2 (§10); the ship stage (2026-10-10) brought the architecture (§2), the start of the data
model (§3), command relay (§5) and the device close codes (§7) up to date. Where an untouched
section still speaks of Teams, IAM logins or test environments, it describes 1.x–3.x and these
sections win; such passages remain in §4, §5, §7 and §11–§13, so this copy needs an editorial pass
before it is published as a docs page. Decisions behind each change:
`docs/migration/decisions.md`.

---

## 1. Identifiers and values

Every identifier, token and bounded value that crosses a wire. Regexes are anchored. "World"
means one isolated data plane; since 4.0 there is only production (until 3.1 each test environment
was a world of its own).

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
| Account uuid | `zQo`, `8YnY5M4E` | Short, case-sensitive letters and digits; never contains `:`. Permanent: Extend keys every person on it. | Silicon Accounts |
| Carbon id | `c:alice` | `c:` + handle. The current public id: it can change, so Extend shows it and never keys on it. | Silicon Accounts |
| Silicon id | `si:chef` | `si:` + handle, as above | Silicon Accounts |
| Membership id | `extend:zQo` | `{app_id}:{uuid}` | Silicon Accounts |
| `app_id` | `extend` | Extend's app id in Silicon Accounts and Silicon Apps | Silicon Apps |
| App secret | `sa_app_extend_…` | Opaque; Extend's server-side credential for introspection, lookups and proofs | Silicon Apps |
| Access token | `eyJ…` | EdDSA-signed JWT: `aud` = `extend`, `iss` = `ACCOUNTS_URL`, `sub` = account uuid, `kind`, `id`, `fid` (sign-in family), `iat`, `exp` (30 minutes) | Silicon Accounts |
| Refresh token | `sar_…` | Opaque; held by the CLI or the website's server, never by Extend's service | Silicon Accounts |
| Short-lived token (SLT) | `slt_…` | Opaque; a Silicon gets one with `silicon-accounts login --app extend -q` and the CLI exchanges it | Silicon Accounts |
| Proof token | `sap_…` | Opaque; sent to the receiving app as `Authorization: Proof sap_…` | Silicon Accounts |
| Proof refresh token | `sapr_…` | Opaque; rotates on every use. Extend seals it at rest (`EXTEND_DELEGATION_ENCRYPTION_KEY`). | Silicon Accounts |
| Webhook signature | `v1=9f86…` | `X-Accounts-Signature: v1=<hex HMAC-SHA256>` over `{X-Accounts-Timestamp}.{exact body}` | Silicon Accounts |
| `Idempotency-Key` | `b1f0c9d2-…` | `^[!-~]{8,255}$` | Caller. The CLI sends a UUIDv4. |
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
                              │   │    └── Ting (requests for a device, wake requests; off until Ting accepts proofs)
                              │   └── Briefcase (files, with Silicon Accounts proofs)
                              └── Silicon Accounts (sign-in, introspection, lookups, proofs, webhooks)
```

Parts, matching `UNDERSTANDING.md`:

| Part | Tech | Where |
|---|---|---|
| Extend service | Rust, `axum` + `tokio`, PostgreSQL through `sqlx`, the official `silicon-accounts-client` crate | `backend.extend.teamofsilicons.com` |
| Configuration website | For Carbons: signs in with Silicon Accounts and calls the same public API from its own server (the browser never holds a token). A subset of the CLI. | `extend.teamofsilicons.com` |
| Extend client | Rust crate `silicon-extend-client`, stateless | crates.io |
| `extend` CLI | Rust, built only on `silicon-extend-client`, keeps its state on disk | `silicon-apps install extend` (Silicon Apps keeps it up to date) |
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

Extend's data is in the PostgreSQL schema `extend` (production, the only world since 4.0). Until 3.1
each test environment had a schema of its own (`extend_test_<environment_id without dashes>`) with
the same migrations; 4.0 stops reading them and leaves them for a manual cleanup. The rest of this
section, down to "4.0", describes the schema as 1.0–3.1 built it; the 4.0 subsection says what
changed.

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

Two additive world migrations bring every world to schema version 5: version 4 introduces
physical devices/Teams/waking, and version 5 adds the shared `in_use_indicator` column (default
`shown`, constrained to `shown|hidden`). One global migration adds enrollment fields. Earlier
migrations are not rewritten. A rollback retains the banner column/default; 1.0 ignores it.

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
  `awake_run`/`awake_seq` of the last frame applied), `last_wake_alert_at`, and the shared
  `in_use_indicator` preference (schema 5).
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
  Team, and missing app/context types observed during delivery in a Team. Those observations do
  not imply separate type catalogs per Team.
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

### 4.0: accounts, identity and no Teams

Schema version 9 (`crates/extend-service/src/identity.rs`) is additive. It deletes nothing:

- **`accounts`**: every account Extend has seen, keyed by uuid: kind, current id, display name, photo,
  status (`active`, `unclaimed`, `pending_custodian`, `deleted`), a Silicon's custodian, the version
  of the last `account.updated` applied, `revoked_before` (tokens issued earlier are refused) and
  `id_set_at` (an older token's `id` claim never overwrites an id an event or lookup set). Filled
  from token claims at every sign-in, from lookups (at most 500 a minute, below the 600 Silicon
  Accounts allows an app; a profile is looked up again after 12 hours) and from webhooks.
- **Identity columns** (`devices.owner_id`, `device_access.silicon_id` / `granted_by`,
  `sessions.silicon_id`, `activity.actor_id`, `requests.from_id` / `to_id` / `routed_to_id`,
  `wake_requests.from_id` / `to_id`, `files.created_by` / `shared_with`) hold account uuids for
  everything written since 4.0. Rows from before still hold IAM public ids (`c:…`, `si:…`): no
  access token's `sub` contains `:`, so they are inert (nobody can use or see them) until an
  operator re-keys them.
- **Re-keying.** `extend-service identity apply --file mapping.csv [--dry-run]` (also spelled
  `link-identities`) rewrites those columns in one transaction from a reviewed mapping of old public
  ids to uuids (`identity suggest` writes a candidate from Silicon Accounts lookups). Each column's
  original value is kept first in a shadow column (`owner_iam_id`, `silicon_iam_id`, …) and every
  run re-derives from the originals, so a corrected mapping replaces the previous result and the
  re-key can be undone until cutover. `identity_links` holds the mapping in force,
  `identity_link_runs` every run's report (rows changed and ids left unmapped per column). A mapping
  that would merge two pairs of one device, or two grants on one pair, is refused whole. Pending
  requests and wake Tings addressed through Silicon IAM are marked failed with why.
- **No Teams.** Team columns keep their values as history and new rows leave them NULL (except
  `devices.team`, which installed apps read: `''` for new pairs). Grants were one per pair, Team and
  Silicon: the earliest stays (with the latest use, and muted if any copy was) and the others move
  to `device_access_archive`. Open wake requests were one per device, Team and Silicon: the latest
  ask stays open and the others are withdrawn (`left_team`). A pair's "wake requests off" moves from
  its organization bindings to the pair. `device_organizations`, the OBO tables and the IAM tables
  stay, inert.
- New tables: `accounts_events` (webhook dedupe on `event_id`), `proof_grants` (proofs Extend holds,
  sealed), `ting_enrolments` and `ting_types` (per account, not per Team).
- **The custodian circle.** Two accounts are in one circle when they are the same account, a
  Silicon and its custodian, or two Silicons with the same custodian. A custodian sees its
  Silicons' grants, sessions, files and requests and can stop them; it never acts as them. A
  request for a device in use reaches the holder only through the same pair and within the asker's
  circle; otherwise it goes to the Carbon who gave the holder access.
- `extend_global.test_environments` and the `extend_test_*` schemas of former test environments stay
  untouched; a later manual cleanup drops them. Rolling back from schema 9 means restoring the
  snapshot taken before migrating (a 3.x service can't run on it: grants lost their Team key).

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
2. The service verifies the Silicon Accounts access token and introspects it (cached no longer than
   30 s, and dropped at once by any webhook about the account), then checks: the session is the
   caller's, it is `active`, the caller still has its grant on the session's pair, the device is
   online, and the command is in the device's capabilities.
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

### Android TV element clicks (1.1)

A primary `click @ref` or selector click on an Android TV app >=1.1 uses `screen.read` and
accessibility element activation. It does not advertise pointer/touch capabilities. The service
keeps the old pointer/touch eligibility for an app older than 1.1 or with an unknown version, so
it does not offer an old app a command its local gate refuses. Coordinates, held/repeated and
secondary clicks retain their native injection requirements. Help and command eligibility share
the same version-aware rule.

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

Every file a command produces is stored in the Silicon's own Briefcase (Extend's app folder for
it), through Briefcase's delegated routes (`/api/v1/obo/…`), with a Silicon Accounts **User
verification proof** for the Silicon (`Authorization: Proof sap_…`):

1. Extend gets the proof while the Silicon is using Extend: its current access token is the subject
   token, the receiving app is `briefcase`, and the scopes are exactly what Extend does there
   (`briefcase.uploads.reserve`, `.commit`, `.status`, `.cancel`, `briefcase.files.read`,
   `briefcase.invitations.create`, `briefcase.entries.trash`). The proof token stays in memory; its
   rotating refresh token is sealed in `proof_grants` and refreshed single-flight with an
   `Idempotency-Key` derived from it.
2. Extend reserves the upload, sends the bytes, and commits it. Every file gets its own Briefcase
   name (`screenshot-<operation id>.png`): Briefcase stores a repeated name as a new version of the
   same entry. Extend's own record and `name` keep the name the device gave.
   The byte transfer (`PUT /api/v1/obo/uploads/{upload_id}/content`) carries no proof: only the
   upload capability, and `X-Org-ID` set to the Silicon's uuid (the drive the reservation belongs
   to), as Briefcase's transfer contract asks.
3. Extend shares the file with the Carbon who paired the device (`read` and `update`, never delete;
   Briefcase grants create only on folders), by the Carbon's uuid.
4. Extend records the `entry_id` and returns Briefcase's permanent link,
   `https://briefcase.teamofsilicons.com/org/{silicon uuid}/apps/extend/{name}`.

Self-destruct defaults to 1 day. `--ttl` on the command that makes the file sets 1 minute to 30
days. `extend file keep <file_id>` makes a file permanent before it goes (the Silicon, or its
custodian). Both are Extend's own records: when a file is due, Extend trashes it through
`entries/trash` as the creating Silicon, with the proof it holds for it (no session or sign-in
needed), with an `operation_id` derived from the file id, so a retry is the same deletion; a 404
counts as gone. The record stays until Briefcase confirms; a failure is retried with backoff from
1 minute to 1 hour, and due files are hidden from lists and reads at once. Without a proof held
for the Silicon (it never used Extend since the proof ended), the deletion waits for its next use.

Who sees a file: the Silicon that made it (while it still has access to the device), the Carbon
who paired the device it was made on, and the Silicon's custodian. Reads (`GET
/api/v2/files/{file_id}/content`, display media) go to Briefcase as the reader, with a User
verification proof for them scoped `briefcase.files.read`, so Briefcase's own sharing applies too.

**Stored display media.** `display show --image` and `--video` accept `file:<file_id>`, a bare
UUID, an Extend content link, or the exact Briefcase URL saved in Extend's file row. A Silicon may
resolve only its own unexpired files. Extend validates the image/video type, bounds the read by the
remaining attachment budget, re-checks expiry after reading and the active session before sending,
and replaces the argument with an ordinary attachment, so the device receives neither private URLs
nor credentials. Local and stored inputs together stay within 8 files/8 MiB.

Sizes: a single file up to 1 GiB. Recordings stop at 1 GiB or 30 minutes, whichever comes first.

**Nothing is lost quietly.** A file the device listed but never uploaded, one under an upload id not
issued for the command, one Briefcase refused to store, one stored but not shared with the device's
Carbon, and one stored but not recorded each become a line in the result's `warnings`.

---

## 7. The Extend apps

Every app does four things: keep the WebSocket to the service open, run commands (on Mac and
Linux through the device engine), honor the in-use indicator preference while retaining Stop, and offer "Revoke
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
- Close codes (as built): `4401` unpaired, `4409` superseded, `4426` upgrade required. Until 3.1 a
  device in a closed test environment also got `4503` (and `503 testing_environment_not_ready` on its
  HTTP routes), keeping its pair; 4.0 never sends them, and apps still keep the credential and
  reconnect with backoff if they see one.

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

### In-use banner controls (1.1)

`in_use_indicator: shown|hidden` is shared by all pairs of a physical device, including carried
aliases. `shown` is the default for a new device and an omitted wire field; an unknown future value
also shows it. A Carbon can change it through the website or `extend device banner <id> on|off`
(the existing owner device PATCH). A Silicon cannot. Device apps can PATCH `/api/v1/device` with
`device_self` data and any live pair credential. A host app can PATCH
`/api/v1/device/attachments/{device_id}` only for a live device carried by that exact host pair in
the same world. The response is the target's `device_self`. Authorization is re-checked under the
instance locks. The service sends `refresh` to direct pairs or `attach` carrying the preference to
all carried aliases/hosts, preserving live drivers, commands, sessions and recording state.

Shown banners/notifications hide after 10 seconds without ending the session or screen hold.
Desktop banners are movable and collapsible and retain position; the desktop icon stays changed
for the session. Hidden suppresses in-use announcements and the icon change. Takeover requests
still show until answered; app and website Stop remain available. Android TV's badge is at bottom
centre. Android's quiet running notification, Apple's Automation Running banner and test-environment
disclosure are separate and are not hidden by this setting.

Desktop changes are saved locally before success, scoped to the service URL, retried after reconnect
and protected against stale responses overwriting a newer choice. Carried aliases update together.
Android also saves offline choices. An older service's omitted preference does not erase a saved
choice. On an unsupported PATCH, Android keeps the local choice with a local-only notice; desktop
keeps it with synchronization pending/error visible. The write cannot block Stop.

### First iPhone/iPad screenshot (1.1)

A first screenshot with no engine session, or a capture refused with `SESSION_NOT_FOUND`, uses a
bare engine `open` to attach the session to the current device, then captures the existing screen.
It does not launch an app. Plain screenshots and bare attachment require no runner; overlays and
selector cropping can need one. Attach, stale-session recovery and capture share the caller's one
deadline. Repeated screenshots reuse the session, and session end closes it. This was verified
through the real engine on an isolated iPad simulator; physical iPad reconnect/new-session checks
remain hardware verification. It does not establish an iPhone/iPad lock-state reading: awake stays
unknown and the Carbon can answer "It's awake".

### Per device

As built on 2026-09-27 (the first draft's rows for Android, Mac, Windows and Linux described plans
that changed; §13 says why):

| Device | How commands are carried out | Indicator |
|---|---|---|
| **Android phone and tablet** | The app's AccessibilityService reads every window as an element tree (same `@eN` refs and snapshot shape as the device engine), taps and gestures with `dispatchGesture`, presses back/home/recents, takes screenshots, and reads notifications through a notification listener. **Android debugging** (the Carbon pairs Wireless debugging from the app once; a TV can use its TCP port) connects the app's own ADB client on the device, which adds `adb`, `install`/`reinstall`, `logs` and `record`. Recording runs supervised `screenrecord` segments of up to 180 s and joins them into one MP4, bounded to 30 minutes or 1 GiB; there is no per-recording consent prompt. Without debugging connected those capabilities are reported missing with "connect Android debugging". | In-use notification for 10 seconds when enabled; quiet running notification and Stop remain |
| **Android TV, Google TV, Fire OS** | Same, plus the remote's arrows and select through the accessibility D-pad actions (Android 13+). With Android debugging connected, every remote button but Power (Menu too, and older TVs' D-pad) is a real key press through `input keyevent`; Power is always refused. The display screen is an activity inside the app. With Android debugging: `adb`, `install`/`reinstall` and `logs`; no recording on TVs. The app is named **Silicon Extend TV** on a TV. | Bottom-centre badge for 10 seconds when enabled, as an accessibility overlay (no extra permission); Stop in the app |
| **Mac** | The device engine's macOS driver through its signed native helper, with no XCTest runner and no UI Automation setup: Accessibility for the element tree, pointer input and text entry (text passed over stdin, focus checked before each key event), ScreenCaptureKit for screenshots and H.264 recording of one app or the display. The only setup steps are Accessibility and Screen Recording for Silicon Extend. A session starts on the frontmost app; `open <app>` binds the named app; links open with the system and the session follows the frontmost app. Terminal commands run as the logged-in user. | Enabled: menu bar icon changes, 10-second movable/collapsible banner; Stop remains in the menu |
| **Windows** (built by Extend) | UI Automation for the element tree, `SendInput` for mouse and keyboard, GDI for screenshots, Win32 for the clipboard, the Start menu and shell for apps, `cmd.exe` for the terminal. Mapped onto the same device engine command set and snapshot shape. `record`, `logs`, `alert` and `replay`/`test`/`batch` are reported missing. Compile-checked and unit-tested only; it has never run on Windows. | Enabled: tray icon changes and 10-second movable/collapsible banner; Stop remains in the app |
| **Linux** | The device engine's Linux driver: AT-SPI2 for the element tree, xdotool (X11) or ydotool (Wayland) for input, a screenshot tool (gnome-screenshot, scrot or ImageMagick; grim on Wayland), xclip/xsel or wl-clipboard. On X11, recording with ffmpeg (libx264 from `x11grab`): the whole screen, or one app's window through XComposite so windows over it are not recorded (an app that can't redraw its whole window within 5 s is refused, with "record the whole screen instead"). `--quality` picks the bit rate: `normal` 8 Mbit/s, `high` 20 Mbit/s. Wayland recording (the ScreenCast portal) is not built and is reported missing, as are `logs`. PTY for the terminal. | Enabled: 10-second movable/collapsible banner; Stop remains in the app |
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

## 8. Test environments (removed in 4.0)

Extend 4 has no Honeycomb test environments: Silicon Apps has no test environments to serve. The
`/internal/honeycomb/…` lifecycle routes are gone (404), and a request that still sends
`X-Testing-Application-Secret` is refused (`401 testing_secret_invalid`) rather than run in
production. Every request is about the `extend` schema. To try Extend without touching anything
real, run a local service (`EXTEND_ENVIRONMENT=development`) with the local Silicon Accounts
stand-in, or against the local Silicon Accounts stack. Existing `extend_test_*` schemas stay until a
manual cleanup.

---

## 9. Sign-in, sign-out and account changes (4.0)

**Signing in.** Every API v2 request carries a Silicon Accounts access token. Extend verifies it
locally: the EdDSA signature against Silicon Accounts' published keys (fetched at start, cached for
an hour, fetched again at most every 10 s when a token names a key Extend doesn't have), `aud` =
`extend`, `iss` = `ACCOUNTS_URL`, `exp` and `nbf`. A token issued before the account's
`revoked_before` is refused (`401 token_expired`). The routes where a sign-out must take effect at
once also ask Silicon Accounts (introspection with Extend's app credentials, cached for at most
30 s and dropped by any event about the account): pairing, granting or removing access, removing a
device, starting a session, commands, takeovers, requests, wake requests and file content.

**What ends access.**

| Event | Source | Effect |
|---|---|---|
| Owner removes a Silicon's access | API | Its session on the device ends at once (`access_removed`); its open wake requests there are withdrawn |
| Device removed or pair revoked | API | That Carbon's pair ends (§4): its sessions end (`device_removed`, `pair_revoked`) and the app is told (`unpaired`) |
| Carbon taps Stop | Device socket or API | Session ends (`stopped_by_carbon`); any Carbon who paired the device |
| A Silicon's custodian ends its session | API | Session ends (`stopped_by_carbon`) |
| `POST /api/v2/auth/logout` | API | Silicon Accounts revokes that sign-in. A Silicon's sessions end (`silicon_logged_out`); a Carbon's sign-out ends the sessions of the Silicons they gave access to, through their own pairs only (`access_removed`). Proofs Extend held for the account end. Devices and grants stay, and the account's other sign-ins stay valid |
| `membership.signed_out` (`app_revoked`) | Silicon Accounts webhook | Extend revoked one sign-in (a logout). The same as `POST /api/v2/auth/logout`, for sessions started before the logout (nothing more when the logout route already ended them; this covers a client that could only revoke at Silicon Accounts). Other sign-ins stay valid |
| `membership.signed_out` (not `app_revoked`), `membership.access_removed` | Silicon Accounts webhook | Tokens issued before the event are refused. A Silicon's sessions end (`silicon_logged_out`) and its wake requests are withdrawn; a Carbon's Silicons' sessions on their pairs end (`access_removed`). Proofs Extend held for the account are revoked. Devices and grants stay |
| `silicon.custodian_changed` | Silicon Accounts webhook | The grants the previous custodian gave the Silicon end (their sessions end `access_removed`); other Carbons' grants stay, and their devices' logs say the custodian changed |
| `account.deleted` | Silicon Accounts webhook | As above, and: a Carbon's pairs end (unpaired, `device_removed`); a Silicon's grants end and its wake requests are withdrawn; the account's id, name and photo go, so others' history reads "deleted account" |
| `account.id_changed`, `account.updated` | Silicon Accounts webhook | The cached id, name and photo change (`account.updated` in version order); everything stays keyed on the uuid |

**Webhooks** (`POST /webhooks/accounts`) are verified over the exact raw body with
`EXTEND_ACCOUNTS_WEBHOOK_SECRET` (and the previous secret during a rotation), with 5 minutes of
tolerance: a bad signature is 401, a body that isn't an event 400. An event is recorded in
`accounts_events` only after its effects succeed (applied one at a time per account, every effect
idempotent), so a failure answers 5xx and Silicon Accounts' retry applies it again; a delivery seen
before is acknowledged.
Types Extend doesn't act on are acknowledged and logged. Production refuses to start without the
secret.

**Without a webhook.** A Silicon signed out elsewhere can't run a command, start a session or ask
for a device once Extend's cached introspection answer is gone (at most 30 s); its running session
ends when the event arrives or at its idle timeout.

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
  (decision 16 below). This limits Extend's `terminal` command. A Silicon with the screen and keyboard
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

### Extend 4: API v2 for accounts

- **API v2** (`/api/v2/…`) carries every account route: devices, access, sessions, commands,
  requests, wake requests, Ting, files, reports and telemetry, plus `/api/v2/accounts`,
  `/api/v2/accounts/lookup`, `/api/v2/me`, `/api/v2/auth/logout` and `/api/v2/silicons…` (custodian
  views). It takes a Silicon Accounts access token; it has no `X-Org-ID` and no `team` parameters.
  Shapes keep their v1 field names and add the permanent uuid next to each person (`*_uuid`); `team`
  is never sent.
- **API v1** stays for the device wire installed apps speak (`/api/v1/device…`,
  `/api/v1/enrollments…`) and `/api/v1/contracts`, byte-for-byte as before. Every other `/api/v1`
  route answers `410 api_version_sunset`, hint `silicon-apps update extend`, details `{retired,
  use_api_version: 2}`. The CLI and client 1–3 negotiated API v1; 4 negotiates v2.
- `SERVED` is `[1, 2]`; the matrix states client and CLI `>=1.0.0, <4.0.0` for v1 and `>=4.0.0,
  <5.0.0` for v2. v1 is never deprecated while installed apps speak it.
- Contract fixtures: `contracts/v1/device` replays unchanged; published client fixtures
  (`contracts/v1/client*`, including the frozen `client-3.1.1`) replay their device-wire requests and
  must get the 410 above for account routes; Honeycomb's lifecycle fixtures moved to
  `contracts/retired/honeycomb` and must get 404.

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
- **Older servers and new consumers.** Deploy the 1.1 service first. A 1.0 service does not support
  new pair enrollment, wake/setup routes, banner PATCH or stored-media resolution. Its absent banner
  field defaults to shown for a fresh client but cannot overwrite a native app's saved local
  choice. Older device apps ignore the optional preference and attach metadata. Stored media uses
  existing command attachments, so no new media wire frame or capability is needed. TV click
  eligibility stays old for apps below 1.1 or of unknown version. The Rust `DevicePatch` struct
  remains source compatible; the additive `DeviceSettingsPatch` and `set_in_use_indicator` client
  method carry the new setting.

---

## 11. CLI and client internals

- **Home:** `$SILICON_HOME` if set, else `~`. State lives in `{home}/.extend/`. `extend config home <dir>`
  moves it; it refuses a path that isn't an existing directory (`not a directory: <path>`). The
  chosen location is recorded in `{default home}/.extend/home` so later runs find it. As built, the
  sign-in, settings and sessions move with it, and a directory that already holds Extend state is
  refused unless `--use-existing` switches to it.
- **Files** (all `0600`, directory `0700`): `auth.json` (format 4: Extend's Silicon Accounts tokens,
  the account's uuid, id, kind, name and custodian, how it signed in, and the Silicon Accounts and
  Extend URLs it is for; it is never sent anywhere else), `config.toml`,
  `sessions/acct-<hex of the uuid>/current` and `…/{session_id}.json` (device, capabilities; keyed by
  the account, in hex because uuids are case-sensitive and some file systems aren't). Extend 3's
  files are never used: its `auth.json` reads as "sign in again", and the next `login` or `logout`
  deletes its `contexts/` and `test/`.
- **Token refresh** happens when less than 60 s are left, under an operating-system lock on
  `refresh.lock` (released if the process dies). After taking it the process reads `auth.json`
  again and uses another process's fresh tokens if there are any; otherwise it refreshes with
  `client_id` alone and saves the new pair (atomically) before using it. It waits up to 60 s for the
  lock and then fails rather than refreshing without it, because presenting a used refresh token
  ends the sign-in. A refused refresh means the sign-in is over: the file is deleted and the command
  exits 3 (`token_expired`, reason `sign_in_ended`).
- **No daemon** is needed: each command is one HTTPS request. See Open question 7.
- **Session selection**, first match wins: `--session <id>`, then `EXTEND_SESSION`, then the
  connected session.
- **Output:** text by default; `--json` gives exactly one JSON document on stdout. Progress,
  warnings and the test-environment line go to stderr so stdout stays safe for scripts and binary
  output. As built (2026-09-27), following the house convention of the sibling CLIs: on success the
  data itself, with no wrapper (`extend accounts --json` has `app_id` at the top, `extend login
  status --json` has `authenticated`); on failure `{"error": {code, message, hint, request_id,
  docs_url, details, exit_code}}` on stderr and nothing on stdout. `login status --json` always exits
  0 and is exactly `{"authenticated":false}` when no one is signed in; without `--json` it exits 1
  when signed out.
- **No test environments:** `--test`, `extend config test` and `extend env` exit 2 with what replaced
  them, and while `EXTEND_TEST_SECRET` is set every command that would call Extend is refused before
  anything is sent, so a script written for a test environment never reaches the real service.
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
- **Local stand-ins** for Silicon Accounts, Briefcase and Ting (`EXTEND_ACCOUNTS_MODE=local`,
  `EXTEND_FILES_MODE=local`, `EXTEND_TING_MODE=local`) exist for development and tests and are
  refused in production. Since 4.0 the Silicon Accounts stand-in also mints short-lived tokens and
  answers the token and revoke endpoints, so the CLI signs in to a local service as it does to
  Silicon Accounts.
- **Extra endpoints:** `GET /api/v2/accounts/lookup` and `GET /api/v2/silicons…` (the website's
  access picker and the custodian's views; Extend 3 had `GET /api/v1/team/silicons` and
  `iam_login_url` in `GET /api/v1/iam`), and `input` on setup steps (`"code"` for an Apple TV).
- **ISI:** when the CLI runs with `ISI` set, it's sent as `X-Silicon-ISI` and recorded with session
  starts and commands in the activity log. Nothing depends on it.
- **Telemetry** goes to the outbox table and is exported to Space Station when
  `EXTEND_SPACE_STATION_KEY` is set (until 3.1, test environments had their own keys).

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
  strictly. A Carbon's logout ends sessions as `access_removed`, whose hint to the Silicon says the
  Carbon took its access away or signed out.
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
  ask IAM for logout and revocation webhooks for the `extend` app. *Resolved by 4.0:* Silicon
  Accounts sends `membership.signed_out`, `membership.access_removed` and `account.deleted` to
  Extend's webhook, and the routes that must see a sign-out at once introspect (cached at most
  30 s); the heuristic is gone (§9).
- **C2. Self-destruct depends on logins Extend saw.** After a service restart, or once the
  Silicon's token has expired, a due file waits (hidden) until the Silicon uses Extend again (§6).
  A durable fix needs one of: Extend's own Briefcase credential, a durable OBO delegation, or
  Briefcase taking the self-destruct time itself through OBO (questions 1–2). Pending Ting requests
  have the same limit, but fail instead of waiting: with no login held, each 30-second retry still
  counts, so the request is marked failed after about 2.5 minutes, saying why. *Resolved by 4.0:*
  Extend keeps the Briefcase proof it got for each Silicon, its refresh token sealed in the
  database, so self-destruct survives restarts and needs no live sign-in (a file whose Silicon has
  no proof, such as one stored before 4.0 or after the Silicon signed out, waits for its next use of
  Extend), and every Ting goes from Extend itself with an App verification proof, so no one's
  sign-in is needed for a retry (§6, §9).
  *1.1 changes the Ting side:* every Ting goes from a member who took part (the asking Silicon, the
  answering Carbon, or the recipient Carbon to themselves), never from a bystander such as the
  Silicon using the device; each has a chain of such actors to try. An attempt with no login held
  doesn't count, and a pending Ting goes out at the next signed-in call of any member in its chain.
  Retries back off (30 s, then 1, 2, 4 and 8 minutes, then every 8 minutes) and resend the exact
  first body; requests give up after 6 counted attempts or 24 hours, woken and declined Tings 30
  minutes after their request ended. A member who turned Extend's Tings off stops the retries until
  "Turn on". Recipient registration remains per Team. Ting 0.1.9 resolves types by context and
  app, across delivery Teams. Extend's four types (`extend.device.requested`, `.wake_requested`,
  `.woken`, `.wake_declined`) are registered once through the app-owning Team's authorized manager
  in each context, and registered again after that context is cleaned. Its current OBO catalog has
  no `types.register`; Extend cannot register them automatically on a Carbon's behalf. Settings,
  the device page, `extend ting status` and delivery lines show missing types and the supported
  manager command. When the owning Team is unknown, guidance uses the quoted `'<owning-team>'`
  placeholder and never substitutes the delivery Team. Turning a recipient on or reading Settings
  does not erase a missing-type error; successful delivery clears that Team's observation of the
  type. The previous app-level-types request's per-Team premise is obsolete and needs a separate
  documentation correction; no maintainer message is implied by this contract review.
- **C3. A raw request reason is capped at 1,000 characters**, whitespace included (the reason
  itself is 1–300 without it). Confirm the cap or change it.
- **C4. The `activate` participant action.** Extend opens a test environment when IAM accepts its
  secret, or on a participant `activate`, which Honeycomb doesn't send today. Keep it as an Extend
  extension, ask Honeycomb to send it, or drop it and rely on IAM alone. *Obsolete in 4.0:* there
  are no test environments (§8).
- **C5. IAM event ids and aggregate versions survive a clean.** They hold no test data, and keeping
  them stops a replayed event from applying twice. Confirm. *Obsolete in 4.0:* there are no cleans;
  Silicon Accounts' events are de-duplicated by `event_id` in `accounts_events`.
- **C6. The bug-report address.** `UNDERSTANDING.md` lists `shubhastro2@gmails.com`; the build sends
  to `shubhastro2@gmail.com`. Confirm the address and correct `UNDERSTANDING.md` (Carbon-only).
- **C7. The CLI's JSON and `login status`.** `--json` now prints the data itself (errors as
  `{"error": …}` on stderr), and `login status` exits 0 when not signed in, following `dm`. This
  changed `cli.yaml` (it said `{ok, data}` and exit 3). Confirm. *4.0:* `login status --json` still
  always exits 0, as Silicon Apps requires; without `--json` it exits 1 when signed out (§11).
- **C8. Sign-up.** The website's "Create an account" goes to IAM's `/signup` beside its `/login`
  (or `iam_signup_url` if `GET /api/v1/iam` ever returns one). Confirm IAM's sign-up address, or
  have Extend return it. *Resolved by 4.0:* Silicon Accounts' hosted pages sign Carbons up and in;
  the website only sends them there.
- **C9. Carbon logout (question 4).** *Settled for 1.1 on 2026-09-27:* a Carbon signing out of the
  website or the CLI ends the running sessions of the Silicons that Carbon gave access to, only on
  that Carbon's side (§9). The build ends them as `access_removed` (the Silicon's hint says the Carbon
  took its access away or signed out), since no end reason can be added without breaking 1.0
  readers. This accepted 1.1 decision does not need another end-reason choice.

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
3. **Where files live.** `UNDERSTANDING.md` says "the private folder of the Silicon". An app acting
   for an account in Briefcase writes only inside its own folder of that account's drive, so files
   land in `apps/extend/` of the Silicon's drive. Is that acceptable?
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
14. **Briefcase file ids as inputs.** *Settled for display media in 1.1:* Extend resolves only its
    own stored image/video references with the caller's authority (§6), keeping the 8-file/8-MiB
    total. `install`, `replay` and `diff screenshot --baseline` require local inputs. Arbitrary
    Briefcase entries and larger APK streaming are not implied by this change.
15. **`record start --quality` on computers.** *Settled as built on 2026-09-27:* `normal` and
    `high` work everywhere `cli.yaml` offers them. The agent passes them to the device engine as
    `medium` and `high`; on a Mac (and a carried iPhone or iPad) that is the export quality, and the
    fork's Linux recorder now encodes at 8 Mbit/s (`medium`) or 20 Mbit/s (`high`), as Android's
    screenrecord does, instead of refusing the option.

### 1.1.0 decisions and verification (reconciled 2026-09-28)

The accepted design already settles logout (C9), the requester identity shown to a routed Carbon,
the first-pair terminal restriction and the cross-Team self-send fallback. Ting's actual type lookup
and OBO catalog correct the old dependency assumptions. These do not need fresh product approval.

16. **After the installer's pair ends.** The implemented literal first-pair rule remains: while
    several Carbons remain paired and none holds the original first pair, no pair gets Extend's
    terminal; when only one remains, it gets terminal capability again. Automatically transferring
    the installer role would be a separate product change, not required by the accepted design.
17. **Terminal apps on the screen.** Settled by the accepted scope: other pairs retain screen,
    keyboard and apps. The restriction removes Extend's terminal capability; it is not an OS-user
    security boundary and does not add a terminal-app denylist. Pairing warns about the shared OS
    account. Process cleanup and credential rotation remain required safeguards.
18. **iPhone/iPad awake state.** A hardware verification item, not an unresolved behavior: report
    unknown until a real lock-state reading is verified, with manual "It's awake" available.
19. **Ting types.** Verified app/context lookup works across delivery Teams. Use the owning-Team
    manager path once per context; there is no current `types.register` OBO endpoint. Keep missing
    type and retry state visible. No per-delivery-Team registration or new Ting API is required.
20. **Routed request with no shared Team.** Accepted: it sends from the recipient Carbon to
    themselves with their own login. If Extend holds no usable login, delivery waits for that
    Carbon's next use; the website/CLI request is visible at once. Never substitute a bystander's
    authority. Self-send was accepted by the isolated real Ting fixture.

No new undecided product choice was found in these 1.1 deltas. Physical-device coverage and release
execution remain verification/operations work, separate from approving edits to the protected files.
