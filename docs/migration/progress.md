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
