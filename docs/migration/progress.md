# Migration progress: Silicon Accounts + Silicon Apps

Branch `migrate/accounts-apps-20261010` (based on `origin/main` e1ed793). Each stage appends a dated section:
what it did, commits, test commands and results, what is left, gotchas. Decisions are in
[decisions.md](decisions.md).

## 2026-10-10 — Stage 1: service

### Baseline (before any change)

Command (Postgres 16 on 127.0.0.1:5460, no Docker; extend-agent excluded, it is the heavy desktop app and has no
identity code):

```
export CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
EXTEND_TEST_ADMIN_URL=postgres://postgres@127.0.0.1:5460/postgres \
  cargo test --workspace --locked --exclude extend-agent --no-fail-fast
```

Result: **399 passed, 0 failed, 5 ignored** (build 3.5 min, tests 4 min). Per binary:

| crate / binary | passed | ignored |
|---|---|---|
| extend-hosted (lib) | 82 | 4 (windows/hardware) |
| extend-service (lib unit) | 46 | 0 |
| extend-service tests: carried_online_freshness 1, carried_reconnect_removal 2, contracts 15, core_gaps 21, devices_gaps 9, e2e 4, idempotency 4, idempotency_concurrency 8, in_use_indicator 5, membership 7, migration 4, multi_carbon 14, obo_grants 1, obo_requests 12, org_devices 4, real_services 1 (returns early without EXTEND_REALIAM_STATE), removed_ref_selection 1, request_concurrency 3, test_env 2, testenv_gaps 17, tv_click 2, waking 8 | 145 | 1 (migration capture) |
| silicon-extend-cli (unit 29, build_scripts 1, cli_behaviour 36, device_args 8, json_consumers 2) | 76 | 0 |
| silicon-extend-client (unit 6, contract_fixtures 3, privacy_defaults 1, request_bodies_1_0 1) | 11 | 0 |
| silicon-extend-protocol (unit 32, compat_1_0 5) | 37 | 0 |
| doc tests | 2 | 0 |

The service tests create `extend_<prefix>_<uuid>` databases and never drop them (113 after one run); they were
dropped with psql afterwards.

### How this stage ran

Twice. The first pass (03:48–05:43) did the work below except the items under "Second pass", ran the
full suite green and clippy clean, but ended before committing or writing past the baseline. The second
pass (07:30–08:20) reviewed that work against the stage checklist, `apps/extend.md` and Briefcase's
migrated contract, restored two dropped tests, fixed what it found, proved the service against the real
local Silicon Accounts stack and against origin/main's own binary, and committed in logical steps. The
first pass's code is commit `cd94147` exactly as it was tested; the second pass's changes are `ecbca18`.

### What the service does now

(Details and the reasons: [decisions.md](decisions.md); the API: `contracts/api.yaml` review copy.)

- **Sign-in**: every account route is on `/api/v2` with `Authorization: Bearer <Silicon Accounts access
  token>` (EdDSA, `aud=extend`, `iss=ACCOUNTS_URL`), verified locally against the cached JWKS (refetched on
  an unknown `kid`, at most every 10 s). The actor is `{uuid, kind, id, scope, family, issued_at}`.
  Introspection (≤ 30 s cache, dropped by any webhook about the account) on pairing, grants, device
  removal, session start, commands, takeovers, requests, wake requests, file content and Ting "Turn on".
  `/api/v1` keeps only the device wire (byte-identical for installed apps); other `/api/v1` routes answer
  `410 api_version_sunset`, hint `silicon-apps update extend`. No inbound proofs (no app calls Extend).
- **Webhook** `POST /webhooks/accounts`: raw-body signature (current + previous secret, 5 min), dedupe on
  `event_id` (`accounts_events`), per-account serialization; all six events handled (see lifecycle.rs);
  unknown types 204, bad signature 401, bad body 400. `/webhook/` answers 410.
- **Accounts and the circle**: `accounts` table (uuid, kind, current id, name, pfp, status, custodian,
  version, `revoked_before`), filled from tokens, lookups (≤ 500/min) and webhooks; `same_circle`,
  `is_custodian`, `silicons_of`; `c:`/`si:` ids resolved through Accounts with exact errors.
- **No Teams**: personal pairs, one grant per (pair, Silicon uuid), custodian views (`/api/v2/silicons…`,
  `?silicon=` on sessions/files/requests), request routing by pair + circle, per-account Ting.
- **Data**: schema 9 additive; `extend-service identity suggest|apply` (alias `link-identities --file
  mapping.csv [--dry-run]`).
- **Cross-app**: Briefcase with User verification proofs (scopes = Briefcase's ids), sealed refresh
  tokens, single-flight refresh; Ting adapter (App verification to send, User verification to enrol), off
  unless `EXTEND_TING_URL` is set.
- **Config**: `ACCOUNTS_URL`, `ACCOUNTS_API_URL`, `EXTEND_APP_ID`, `EXTEND_APP_SECRET`,
  `EXTEND_ACCOUNTS_WEBHOOK_SECRET` (+ `_PREVIOUS_`), validated at boot with exact messages, http only for
  loopback; `crates/extend-service/.env.example`.

### Second pass

- **Briefcase transfer** now matches Briefcase's migrated `openapi.yaml` (and its cutover.md, which names
  Extend): `X-Org-ID: <Silicon uuid>` on `PUT /api/v1/obo/uploads/{id}/content` (no credential), the Carbon
  invited by uuid, links `https://briefcase.teamofsilicons.com/org/{uuid}/apps/extend/{name}`.
- **`membership.signed_out` / `app_revoked`** ends what the account runs, like `POST /api/v2/auth/logout`,
  for sessions started before the logout (the first pass ignored it, leaving a gap when a client can only
  revoke at Silicon Accounts). Shared helper `lifecycle::logged_out`.
- **Host removal race**: removing a computer also unpairs a device attached while it waited for the lock;
  an attachment that gets the lock afterwards is refused. 3.x had this for organization removal only; the
  test came back on API v2 (`carried_reconnect_removal.rs`).
- **Deletion** also drops the Ting enrolment record; **actor** carries `scope`; `Debug` redacts the token;
  the Docker-production check is back (config unit test).
- Docs: operations.md's webhook wording corrected (applied one at a time per account, not "one
  transaction"); review copies updated; `docs/migration/cutover.md` written; `e2e/clean-test-dbs.sh` uses
  psql instead of `docker exec`.

### Commits

| commit | subject |
|---|---|
| `cd94147` | Sign in with Silicon Accounts and drop Teams and test environments from the service |
| `ecbca18` | Match Briefcase's proof-era transfer and keep sign-out and removal guarantees |
| `8848b2a` | Regenerate third-party notices for silicon-accounts-client |
| `619f9be` | Document configuring and running the service with Silicon Accounts |
| `33fe762` | Record the migration decisions, cutover notes and contract review copies |
| (this) | Log the service stage |

### Tests and proofs (all run in the second pass, at HEAD unless said)

1. Full suite, same command as the baseline (plus `cargo check --workspace --all-targets --locked` first):
   **370 passed, 0 failed, 5 ignored**. Per binary: extend-hosted 82 (4 ignored); extend-service lib 37;
   extend-service integration 125 (1 ignored: the migration capture) — accounts_auth 5, accounts_migration 5,
   accounts_stub 3, accounts_webhook 8, authz 5, carried_online_freshness 1, carried_reconnect_removal 3,
   contracts 16, core_gaps 21, cross_app_stub 2, devices_gaps 3, e2e 4, idempotency 4,
   idempotency_concurrency 8, in_use_indicator 4, migration 4, multi_carbon 14, real_accounts 1 (skips
   without the stack's values), removed_ref_selection 1, request_concurrency 3, tv_click 2, waking 8;
   silicon-extend-cli 76; silicon-extend-client 11; silicon-extend-protocol 37; doc tests 2. Against the
   baseline's 399: the suites of removed features are gone (membership, obo_grants, obo_requests,
   org_devices, real_services, test_env, testenv_gaps = 44; six test-environment cases in devices_gaps, one
   in in_use_indicator, IAM/OBO/test-world unit tests), 29 new tests in seven new suites plus the restored
   two. Every baseline test of behaviour that still exists passes (decisions.md #33 lists the mapping).
2. `cargo clippy --workspace --all-targets --locked -- -D warnings` (agent included): clean.
   `cargo fmt --all -- --check`: clean.
3. `npx @redocly/cli@2.49.0 lint docs/migration/contracts/api.yaml --skip-rule no-path-trailing-slash`:
   valid, 12 warnings (the same 12 style warnings `understanding/api.yaml` has).
4. **Real Silicon Accounts** (shared local stack, `tests/real_accounts.rs`): a Carbon signed in to `extend`
   through the hosted pages (`mint.mts app-signin --exchange`) and a Silicon under it (SLT exchanged with
   Extend's dev secret): real JWKS verification, `/api/v2/me` with the custodian from a real lookup, by-id
   resolution (unknown id → 422), pairing and a session (introspected), a command, the custodian view, the
   Silicon's logout revoking at Accounts and ending its session (`silicon_logged_out`), and its token then
   refused (`401 token_expired` from introspection). Passed with c:extend-svc-c1-1/si:extend-svc-s1-1 on the
   first pass's code and with c:extend-svc-c1-3/si:extend-svc-s1-3 at HEAD.
5. **Real webhook deliveries** (scratch `extend-svc/webhook-smoke.py`): registered Extend's webhook on the
   stack (`PUT /v1/apps/extend/webhook`, url `http://127.0.0.1:4221/webhooks/accounts`), ran
   `extend-service serve` on 4221 (SDK mode, Postgres 5460): Accounts' test ping → delivered, 204; the
   custodian changed `si:extend-svc-s1-2` to `si:extend-svc-s1-2b` (`POST /v1/me/silicons/{uuid}/id`) → a
   real `account.id_changed` reached Extend, the cached id and the custodian's `GET /api/v2/silicons` showed
   the new id; `accounts_events` = [ping, account.id_changed]; a forged delivery → 401. Then the service was
   stopped and the webhook deleted again (`DELETE` → 204): **no webhook is registered for extend on the
   stack now** (the e2e stage registers its own).
6. **Upgrade path with origin/main's own binary** (scratch `extend-svc/upgrade-proof.sh`): `git archive
   e1ed793` built into a scratch target; its `extend-service migrate` → schema 8; a fixture written the 3.x
   way (two pairs of one TV in two Teams, a carried device, per-Team duplicate grants, sessions, a lock,
   activity, pending/delivered requests, two open wake requests, files, Ting rows, OBO rows, IAM events,
   idempotency, reports, telemetry, a test environment and a `extend_test_*` schema); this branch's
   `migrate` → schema 9. Every one of the 47 rows survived except the documented merges: 1 grant moved to
   `device_access_archive` (6 → 5 + 1), the older duplicate open wake request withdrawn (`left_team`), the
   org-level wake mute moved to its pair, and the schema version 8 → 9. Then `link-identities --dry-run`
   (5 links, 4 owner rows, `si:scout` reported unmapped, nothing changed) and `identity apply` (owners and
   grants re-keyed with the originals in `*_iam_id`, 1 pending IAM-era request failed with why, the test
   schema and OBO rows untouched, one `identity_link_runs` row). Historical migrations are byte-identical to
   origin/main's (checked by script).
7. `cargo about generate about.hbs` reproduces `THIRD_PARTY_LICENSES.txt` byte for byte.

### Blocked on

- **Briefcase**: Extend's cutover waits for Briefcase 4.0 to accept User verification proofs from
  `extend` (its decisions S4/S15 already plan it). Its drive layout lands in its part 2: the file link
  form (`/org/{uuid}/apps/extend/{name}`) and reads of a Silicon's file by the pair's Carbon (with the
  Carbon's own proof, from another account's drive) must be re-checked then (integration stage).
- **Ting** stays on IAM: the adapter is tested against an HTTP stand-in only (no `ting` app on the local
  stack); delivery is off at cutover by design.

### Left for later stages

- CLI and client (stage 2): `silicon-extend-client` 3.1.1 and the CLI still call the `/api/v1` account
  routes, which now answer 410; device flow, `--slt`/`--slt-stdin`, the positional alias and hidden
  `iam --json`, `accounts --json`, `login status --json`, `contracts/v2/client` fixtures, `extend silicon`.
- Ship: `apps.yaml` and packaging, `release.yml`, the deploy renderer in `deploy/aws/standalone.yaml`
  (still requires IAM/Honeycomb keys), README (still IAM/Docker-era), THIRD_PARTY_NOTICES' "Honeycomb
  archive" line, moving IAM-era docs (OBO_CUTOVER, FEATURE_PERMISSIONS, ORGANIZATION_DEVICES, RELEASE_IAM5,
  verification…) to `docs/history/`, deleting `e2e/real-iam`, `honeycomb.yaml`, `e2e/cli-e2e.sh` updates.
- e2e: `scripts/dev-accounts.sh` with the webhook registered on the stack.
- Web: the Next.js BFF calls `/api/v2` and must sign out through `POST /api/v2/auth/logout`.
- UNDERSTANDING.md: the Carbon's call ([understanding-proposal.md](understanding-proposal.md)).

### Gotchas

- `psql` isn't on PATH on this Mac: `/opt/homebrew/opt/postgresql@16/bin/psql`
  (`PSQL=… e2e/clean-test-dbs.sh`). The suites leave `extend_<prefix>_<32 hex>` databases; one full run
  makes ~120.
- The local Accounts stand-in derives uuids from ids (`LocalAccounts::uuid_for`), so tests name accounts
  by id; its tokens are real EdDSA JWTs verified by the same code path as production's.
- `Principal` holds the raw access token (the subject token of proofs): never log it.
- crates.io `silicon-accounts-client` 0.4.0 lacks the public-client SLT exchange and revoke helpers the
  local source has: the CLI stage needs raw HTTP for those (brief, "Public-client revoke").
- `mint.mts app-signin --app extend` must use the seeded redirect `http://127.0.0.1:9593/extend/callback`.
- No process from this stage is left running (`.mig/pids` empty, nothing on 4220-4239); every database it
  created on 5460 was dropped.

## 2026-10-10 — Stage 2: client crate and CLI

`silicon-extend-client` 4.0.0, the `extend` CLI 4.0.0 and `silicon-extend-protocol` 2.0.0 speak
Silicon Accounts only, follow the Silicon Apps CLI contract, and work against the migrated service
(API v2). Decisions 35–57 are in [decisions.md](decisions.md#client-and-cli-stage-2-2026-10-10); the
production steps are in [cutover.md](cutover.md) ("From the client and CLI stage"); the
UNDERSTANDING edits for the CLI are §8 of [understanding-proposal.md](understanding-proposal.md);
the CLI contract's review copy is [contracts/cli.yaml](contracts/cli.yaml).

### What changed

- **Client** (`crates/silicon-extend-client`): `auth::SignIn` — device flow (`start_device`,
  `poll_device`, `wait_for_device` honouring `interval`/`slow_down`, 10-minute expiry), short-lived
  token exchange with `client_id=extend` and no secret (every `invalid_grant` reason mapped:
  `slt_already_used`, `slt_expired`, `slt_wrong_app`, `slt_unknown`, `slt_wrong_kind`,
  `slt_sign_in_ended`; non-`slt_` values never sent), rotating `refresh`, `revoke`; typed
  `AuthError`; `Secret` tokens. `Client::authed(token)` on `/api/v2` (no Team header), `accounts()`,
  `me()`, `sign_out()`, `lookup()`, `silicons()`/`silicon()`/`silicon_grants()`/`renounce()`,
  `ListQuery::silicon`, Ting without a Team. The handshake offers 2; each request pins the major its
  path names (device-side calls stay on v1). IAM/test-environment/Team/permission/import calls
  removed. `src/lib.rs` split into `lib.rs` (client, device side), `authed.rs` (account API) and
  `auth.rs` (sign-in).
- **CLI** (`crates/extend-cli`): `login` (device flow, `--open`, `--label`, `--json` event lines),
  `login --slt|--slt-stdin|<slt>`, `login status [--offline] [--json]`, `logout` (service sign-out,
  falling back to a direct public revoke), offline `accounts --json`, hidden `iam --json`, `silicon
  ls|show|renounce`, `--silicon` on `session ls`/`file ls`/`request ls`; `signin.rs` (auth.json
  format 4, 0600/0700, atomic, OS file lock, single-flight refresh, Extend 3 detection);
  `retired.rs` (removed spellings → exit 2 with the replacement); `store.rs` without test
  environments or per-organization contexts, sessions keyed by account uuid; `accounts_url` setting;
  help tree and update hints rewritten (`silicon-apps install|update extend`).
- **Protocol**: the v1 account API's IAM/Team types and `ting::register_command` removed;
  `Visibility::default()` is `Personal`; CHANGELOG 2.0.0 written (it covers the service stage's
  additions too). The device wire is unchanged (protocol compat suite green).
- **Contracts**: 57 fixtures in `contracts/v2/client` (account calls), 8 in `contracts/v1/client`
  (device side); the service's replay treats `/api/v2` as served (`still_served`), and its two
  matrix tests now expect the 4.0 client to agree 2. `fake_device` no longer takes a test secret.
- **Docs**: `docs/cli.md`, `docs/client.md`, both crate READMEs, `contracts/README.md`,
  `docs/migration/contracts/cli.yaml` (review copy of `understanding/cli.yaml`, which the help test
  now reads), THIRD_PARTY_LICENSES.txt regenerated.

### Commits

| commit | subject |
|---|---|
| `33c7784` | Sign the client and CLI in with Silicon Accounts and drop Teams and test environments |
| `f344676` | Document the CLI and client on Silicon Accounts |
| `98513ee` | Regenerate third-party licences and describe the fixtures by major |
| `dcdd0bf` | Say what replaced Extend 3's groupings without naming them as a concept |
| `8597303` | Record the client and CLI stage |
| (this) | Keep Silicon IAM, Honeycomb and Teams out of what the CLI prints and the crates' docs |

### Tests

All with `CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3`.

| Command | Result |
|---|---|
| `EXTEND_TEST_ADMIN_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --workspace --locked --exclude extend-agent --no-fail-fast` | **394 passed, 0 failed, 5 ignored** (service 125 integration + 37 lib, hosted 82, protocol 32 + 5, CLI 34 unit + 15 `cli_accounts` + 28 `cli_behaviour` + 8 `device_args` + 2 `json_consumers` + 1 `build_scripts`, client 9 unit + 3 `contract_fixtures` + 8 `sign_in` + 1 + 1, doc tests 3) |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` (agent included) | clean |
| `cargo fmt --all -- --check` | clean |
| `EXTEND_TEST_ADMIN_URL=… cargo test -p extend-service --test contracts` | 16 passed: the 57 v2 client fixtures replay against a real service; 3.1.1 and older client fixtures still get the 410; device fixtures replay |
| `EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures`, then without it | fixtures written, then 3 passed |
| `cargo package --list --allow-dirty --offline -p <crate>` for `silicon-extend-protocol`, `silicon-extend-client`, `silicon-extend-cli` | each lists its sources and README; protocol and client their CHANGELOG; client and CLI their tests (the CLI's shared harness `tests/common/mod.rs` included) |
| `cargo about generate about.hbs -o THIRD_PARTY_LICENSES.txt` | only the client and CLI versions change |
| `ruby -ryaml -e 'YAML.load_file("docs/migration/contracts/cli.yaml")'` | parses: 45 commands, 38 device commands |
| After the last commit's rewording (messages, changelogs, rustdoc, the review copy): `cargo fmt --all -- --check`; `cargo test -p silicon-extend-cli --locked`; `cargo test -p silicon-extend-protocol -p silicon-extend-client --locked`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; the YAML parse again | clean; 34 + 1 + 15 + 28 + 8 + 2 passed; 32 + 5 and 9 + 3 + 1 + 1 + 8 + 3 doc tests passed; clean; 45 / 38 |

New coverage. `crates/silicon-extend-client/tests/sign_in.rs` (axum stand-in for Silicon Accounts):
device flow pending → slow_down (interval 1 → 6 s) → tokens; denied; expired (server and local
deadline); SLT ok with `client_id` alone and no Authorization header; each refusal reason with the
token never echoed; non-SLT values never sent; refresh rotation and a reused refresh token
(`sign_in_ended`); revoke form; unreachable Accounts (transient); URL rules.
`crates/extend-cli/tests/cli_accounts.rs` (the real binary against a fake that is both Extend and
Silicon Accounts): discovery in an empty `env -i` home (golden `accounts --json`, `iam --json` the
same, `login status --json` = `{"authenticated":false}` exit 0, text exit 1, `--help` without
IAM/Honeycomb/Team words, no file written); the device flow with `--json` lines, the label, the
saved file's 0600/0700 modes; denied/expired codes; SLT on stdin, `--slt` and positional, never in
a file; every refusal's `details.reason`; mismatch and unwritable home refused before the token is
spent; four concurrent commands → one refresh; a refused access token refreshed and the call
repeated; a refused refresh deletes the sign-in; `--offline`; logout through Extend and the fallback
revoke (form: token, hint, client_id); Extend 3 and unreadable files; origin isolation; replacing
another account's sign-in revokes it; a refresh never overwrites a newer file.
`crates/extend-cli/tests/cli_behaviour.rs` was rewritten on the shared harness
(`tests/common/mod.rs`) for API v2: every behaviour of 3.x that still exists, plus removed spellings,
access by id, Ting without Teams and the custodian commands. Unit tests: `signin.rs` (round trip,
legacy/corrupt files, Debug redaction, refresh keeps the account), `retired.rs`, `args.rs` (login
grammar, retired global flags), settings.

### Live: the shared Accounts stack and the migrated service

Scripts: `<scratch>/extend-cli/real_run.py` and `real_run2.py` (they start `extend-service serve` on
127.0.0.1:4221 in SDK mode against `http://localhost:9590` / `http://127.0.0.1:9589` with Extend's dev
secret and a fresh database on 5460, plus `fake_device`, and stop and drop everything at the end).
Every CLI call ran as `env PATH=/usr/bin:/bin HOME=$H SILICON_HOME=$H EXTEND_API_URL=http://127.0.0.1:4221
ACCOUNTS_URL=http://localhost:9590 target/mig/debug/extend …` with a fresh `$H` per identity; tokens
appear only as SHA-256 prefixes. Run 1603606:

1. **Discovery, empty home** (`env -i HOME=$E SILICON_HOME=$E`): `--help` exit 0 (82 lines);
   `accounts --json` exit 0 →
   `{"accounts_url":"https://accounts.teamofsilicons.com","api_url":"https://backend.extend.teamofsilicons.com","app_id":"extend","client_id":"extend","device_flow":true,"docs_url":"https://extend.teamofsilicons.com/docs","install":"silicon-apps install extend","package_url":"https://crates.io/crates/silicon-extend-client","public_client":true,"repository_url":"https://github.com/teamofsilicons/silicon-extend","sign_in":{"carbon":"extend login","silicon":"silicon-accounts login --app extend -q | extend login --slt-stdin"},"status":"extend login status --json","update":"silicon-apps update extend","version":"4.0.0","website_url":"https://extend.teamofsilicons.com"}`;
   `login status --json` exit 0 → `{"authenticated":false}`; `iam --json` the same object; files
   written: 0.
2. **Silicon** (`mint.mts silicon` → `si:extend-cli-s-1603606` = `X9R`, custodian
   `c:extend-cli-c-1603606` = `lP8`; `mint.mts slt --app extend`): `printf %s "$SLT" | extend login
   --slt-stdin` exit 0 → "Signed in to Extend as si:extend-cli-s-1603606 (…), a Silicon looked after
   by c:extend-cli-c-1603606."; `login status --json` →
   `{"authenticated":true,"custodian":{"id":"c:extend-cli-c-1603606","uuid":"lP8"},"id":"si:extend-cli-s-1603606","kind":"silicon","method":"slt","uuid":"X9R","verified":true,…}`;
   auth.json `0o600`, without the SLT in it. The same SLT again → exit 3 `slt_invalid`/`slt_already_used`
   ("…already used; each one works once."); an SLT minted for remind → `slt_wrong_app` ("…issued
   for the app 'remind', not for 'extend'…"); `--slt slt_not-a-real-token-at-all` → exit 3 `slt_invalid`, "The
   short-lived token is not known: it is mistyped or was never issued."; still signed in afterwards; an SLT used after 125 s → `slt_expired` ("…expired at … (they last 120
   seconds)…"). No printed output holds a token (checked by pattern on both run outputs; the
   automated suites assert it).
3. **Carbon** (device flow): `extend login --json` in the background printed
   `{"event":"device_code","user_code":"84CX-7XAY","verification_uri":"http://localhost:9590/device","expires_in":600,"interval":5}`;
   `mint.mts approve --email extend-cli-c-1603606@example.test --code 84CX-7XAY` → 204; the CLI
   ended with `{"event":"signed_in","authenticated":true,"uuid":"lP8","id":"c:extend-cli-c-1603606","kind":"carbon","method":"device","verified":true}`, exit 0.
4. **Real commands**: the Carbon paired `fake_device` with `extend device pair <code> --name "CLI
   stage box" --access si:extend-cli-s-1603606` → `7e5a030a`. The Silicon: `device ls` (the box,
   online), `session new 7e5a030a --connect` → session `531`, `snapshot -i` → "snapshot -i → ok on
   the fake device", `session status` (active, 25 commands work there). The Carbon as custodian:
   `silicon ls` (yes / 1 / 1 / 1), `silicon show` (every device it can use), `session ls --silicon …
   --state active` (531), `device show 7e5a030a` ("Access: si:extend-cli-s-1603606"). The Silicon
   ended its session.
5. **Refresh with the real Silicon Accounts**: with 20 s left on the Carbon's access token, `device
   ls` refreshed (refresh token `f9ffb2d2c363` → `101f4ed11f78`); again with three `device ls` at
   once: all exit 0, one rotation (`101f4ed11f78` → `7a8210a43096`), and `login status` then
   verified — a second refresh of one token would have revoked the sign-in.
6. **Runtime forms**: `extend login "$SLT"` (positional) exit 0; `extend iam --json` → app_id extend.
7. **Sign-out**: `logout --json` for the Silicon, the Carbon and the runtime home →
   `{"signed_out":true,"revoked":true,"via":"extend",…}`; `login status --json` →
   `{"authenticated":false}`; each old refresh token at Silicon Accounts → 400 "The sign-in this
   refresh token belongs to was revoked at … (app_revoked)".
8. **Extend down** (`real_run2.py`, run 1603795): a Silicon signed in, the service was stopped,
   `logout --json` → `{"signed_out":true,"revoked":true,"via":"silicon-accounts"}`, auth.json gone,
   the refresh token revoked at Silicon Accounts.
9. **Revoked elsewhere**: a Silicon signed in, its refresh token revoked directly at Silicon
   Accounts, the access token expired by hand: `extend device ls` → exit 3 "Your sign-in to Extend
   has ended: The sign-in this refresh token belongs to was revoked at … (app_revoked); sign in
   again." with the sign-in hint; auth.json deleted; `login status --json` → `{"authenticated":false}`.

No webhook was registered for `extend` on the stack (the service stage removed its own), so these
runs didn't depend on deliveries. Every process started was stopped (`.mig/pids` empty, nothing on
4220–4239) and both databases were dropped; the service suites' throwaway databases were dropped
with `e2e/clean-test-dbs.sh`.

### Blocked on

- Nothing new. (Briefcase's proof support and Ting's Accounts support, from stage 1, still gate the
  production cutover; the CLI only reports what the service answers.)

### Left for later stages

- **e2e**: `e2e/cli-e2e.sh`, `e2e/run-all.sh` and `e2e/released-*` still drive the 3.x flow (test
  environments, member-id logins, the Honeycomb lifecycle); they need Accounts logins (the device
  flow can be approved with `mint.mts approve`, Silicons with `mint.mts slt`). `e2e/real-iam` is still
  there (the json_consumers test lists it as a known lane; drop it from that list when it goes).
- **Ship**: `apps.yaml` and `scripts/package-apps.sh` (the three discovery commands already answer in
  an empty `env -i` home), `release.yml` (still "Honeycomb release archive"), THIRD_PARTY_NOTICES'
  "Honeycomb archive" row, `honeycomb.yaml`, moving IAM-era docs to `docs/history/`.
- **Web**: the Next.js BFF should read `docs/migration/contracts/cli.yaml` for its CLI reference
  (`web/scripts/gen-docs.mjs` still reads `understanding/cli.yaml`), and sign out through `POST
  /api/v2/auth/logout`.
- **Runtime**: move stemcell to `extend login --slt-stdin` and `extend accounts --json`, then drop
  the hidden `iam` alias (4.1).

### Gotchas

- `crates/silicon-extend-client/tests/contract_fixtures.rs` decides a fixture's directory from the
  request path; a new account call must use `/api/v2/…` or it lands in `v1/client`.
- `store::write_private` chmods the parent to 0700, so a 0500 state directory owned by the user is
  still writable; the unwritable-home test uses a home without `.extend` instead.
- macOS (APFS) is case-insensitive: session caches use the hex of the uuid, never the uuid itself.
- The shared stack's issuer is `http://localhost:9590`: the CLI's `ACCOUNTS_URL` must be exactly
  that, or the service's discovery check refuses the sign-in as `accounts_mismatch` (by design).
- `HOSTNAME` is often not exported; the device-flow label falls back to "extend CLI".
