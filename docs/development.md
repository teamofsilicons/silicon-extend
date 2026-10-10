# Development

## Prerequisites

Rust 1.98 (pinned in `rust-toolchain.toml`), PostgreSQL 16 or newer, Node 22+ with pnpm (website, the
device engine), Python 3.9+ (packaging and the end-to-end lanes), and for device apps: the Android SDK
(API 36) with a JDK 17+, and Xcode on a Mac. Docker is one way to run PostgreSQL; a few manual lanes
(Linux desktop recordings) need it.

## The service

```sh
# PostgreSQL on 127.0.0.1:5440 with user and password `extend` and a database `extend`, for example:
docker run -d --name silicon-extend-postgres -e POSTGRES_USER=extend -e POSTGRES_PASSWORD=extend \
  -e POSTGRES_DB=extend -p 127.0.0.1:5440:5432 postgres:16.9-bookworm
set -a; . e2e/dev.env; set +a
cargo run -p extend-service          # http://127.0.0.1:8480, migrations run at start
```

Any other PostgreSQL works: point `EXTEND_DATABASE_URL` (the running service) and
`EXTEND_TEST_ADMIN_URL` (the suites, an admin URL ending in `/postgres`) at it.
`EXTEND_ENVIRONMENT` must be set (`e2e/dev.env` sets `development`): the service refuses to start
without it rather than guess. CI and production run PostgreSQL 17; the suites also pass on 16.

`e2e/dev.env` runs with local stand-ins for Silicon Accounts, Briefcase and Ting (all refused in
production):

- **Local Silicon Accounts** (`EXTEND_ACCOUNTS_MODE=local`): signs real EdDSA access tokens with its
  own key, so the service verifies them exactly as it verifies Silicon Accounts'. Get one with
  `POST /dev/accounts/token {"type":"dev_token","data":{"id":"si:scout","custodian":"c:ada"}}`
  (the account, and a Silicon's custodian, are created on first use); its keys are at
  `/dev/accounts/.well-known/jwks.json`. Deliver webhook events signed with
  `EXTEND_ACCOUNTS_WEBHOOK_SECRET` to `POST /webhooks/accounts` to simulate sign-outs, custodian
  changes and deletions.

  The CLI signs in to it exactly as it signs in to Silicon Accounts. `POST /dev/accounts/slt` (same
  body) mints a short-lived token for Extend, as `silicon-accounts login --app extend -q` does, and
  the stand-in's `/dev/accounts/v1/oauth/token` exchanges and refreshes it:

  ```sh
  export EXTEND_API_URL=http://127.0.0.1:8480 ACCOUNTS_URL=http://127.0.0.1:8480/dev/accounts
  curl -s -XPOST "$ACCOUNTS_URL/slt" -H 'content-type: application/json' \
    -d '{"type":"slt","data":{"id":"si:scout","custodian":"c:ada"}}' |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["slt"])' | extend login --slt-stdin
  ```
- **Local files**: stored under `EXTEND_DATA_DIR/files`, served at `/dev/files/{id}`.
- **Local Ting** (`EXTEND_TING_MODE=local`): Tings are recorded; `GET /dev/ting` lists them. Without
  `EXTEND_TING_URL` or this, notifications are off and requests stay visible on the website, in the
  CLI and on the device.

Against the local Silicon Accounts stack (accounts on `http://localhost:9590`, its API on
`http://127.0.0.1:9589`): set `ACCOUNTS_URL`, `ACCOUNTS_API_URL`, `EXTEND_APP_SECRET` and
`EXTEND_ACCOUNTS_WEBHOOK_SECRET` instead of `EXTEND_ACCOUNTS_MODE` (`crates/extend-service/.env.example`
lists every variable). `tests/real_accounts.rs` runs the service against that stack when given a
fresh Carbon and Silicon signed in to `extend` (see the file).

The shared development service limits new enrollments to 60 per hour per address, in memory. When
several people or agents run end-to-end lanes, start your own instance with the same `e2e/dev.env`
but another `EXTEND_BIND`/`EXTEND_PUBLIC_URL` port, its own `EXTEND_DATABASE_URL` database and its
own `EXTEND_DATA_DIR`, and point the lane at it (`bash e2e/cli-e2e.sh http://127.0.0.1:<port>`).

## Tests

| Command | What it covers |
|---|---|
| `cargo test --workspace` | Unit tests everywhere, plus the service suites against real PostgreSQL (`EXTEND_TEST_ADMIN_URL`), real HTTP/WebSocket and scripted devices: `e2e.rs` (pairing, sessions, relay, files, takeover, revocation, idle and pair expiry, versioning), `core_gaps.rs` (the idle hold, sessions ending mid-command, sign-outs, Ting enrolment and retries, self-destruct, downloads, warnings, requests), `devices_gaps.rs` (removed devices, hosted devices, paging), `multi_carbon.rs` (several Carbons per device, sides, the custodian circle), `authz.rs` (every route family), `accounts_auth.rs`, `accounts_webhook.rs`, `accounts_migration.rs` (schema 9 and `identity apply`), `accounts_stub.rs` and `cross_app_stub.rs` (Silicon Accounts, Briefcase and Ting over HTTP stand-ins), and `contracts.rs` (versioning and the contract replay). `tests/real_accounts.rs` runs only with a local Accounts stack's values set. The suites leave throwaway databases (`extend_*`); `e2e/clean-test-dbs.sh` drops them |
| `cargo test -p silicon-extend-client --test contract_fixtures` and `cargo test -p extend-service --test contracts` | Consumer contracts: the client's recorded fixtures still match what it sends, and a real service accepts every fixture in `contracts/` (`contracts/README.md`) |
| `cargo test -p silicon-extend-cli` | Includes `tests/device_args.rs`: the real binary against a fake service, reading the exact arguments a device receives |
| `bash e2e/cli-e2e.sh [api_url]` | The `extend` CLI end to end against a running service with `e2e/dev.env`'s stand-ins and `examples/fake_device`: every account signs in with a short-lived token from the Silicon Accounts stand-in, then pairing, access by id, sessions and device commands, files, requests between Silicons of one custodian and of another, takeover, waking, setup retry, the custodian's views, sign-out ending sessions, and the removed Extend 3 spellings. CI runs it |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | The Silicon Apps packager (`scripts/package-apps.sh`): its refusals, the discovery checks, and a full pack with the real `silicon-apps` when 0.2 is installed |
| `python3 -m unittest discover -s deploy/aws -p 'test_*.py'` | The production host's environment renderer against fixtures, and `refresh-host-helper.py` |
| `python3 apps/desktop/macos/banner-native-e2e.py` | macOS production banner in an owned fake-agent app: native frame, controls, focus and timeout checks. `--interactive` leaves only that fixture open for a human drag check. No real driver, service, TCC change or installed-app mutation. |
| `python3 e2e/linux-release-rehearsal.py --package /path/to/linux-arm64.deb --out /new/output` | Owned Docker X11 app, banner movement/collapse/Stop, recording through host metadata changes and detached terminal cleanup. Uses the existing Linux test image and an owned local service/database. `--agent-bin` records an explicit native agent override for a newly fixed binary. Extend 3 rehearsal: runs against a 1.1–3.1 service only. |
| `python3 e2e/released-agent-compat.py --archive /path/to/released-1.0-mac.zip --out /new/output` | Native macOS arm64 headless agent upgrade with an owned file credential store, disabled engine, terminal/session continuity and two-Carbon terminal rules. Requires built current binaries and local PostgreSQL; verifies the published old archive checksum and leaves installed apps/autostart untouched. Extend 3 rehearsal: runs against a 1.1–3.1 service only. |
| `cd web && pnpm test && pnpm build && pnpm test:e2e` | Website unit tests, the type-checked build, and Playwright against the mock API; `pnpm test:e2e:real` against a running service (`EXTEND_REAL_URL`) |
| `apps/android` | See its README (unit tests, emulator runs, the notices generator and dependency verification) |
| `crates/extend-agent`, `apps/desktop/linux-e2e` | Desktop agent tests; Linux run and recording lanes in Docker (`apps/desktop/README.md`) |
| `node --test apps/desktop/*.test.mjs` | Packaged runtime stamp and entry, and the packaging checks (stamp errors, dist freshness) |
| `node --test apps/desktop/banner-ui.e2e.mjs` | Desktop WebView banner controls in Chromium; requires `web` dependencies and Playwright Chromium, and runs in CI's browser job. |
| `cd vendor/extend-engine && pnpm typecheck && pnpm lint && pnpm test:unit` | The device engine (Silicon Extend's fork): its whole unit suite (unit-core and fuzz-worker), as CI runs it; `pnpm test:macos-helper` for the Swift helper |
| `cd apps/android && ./gradlew :app:testDebugUnitTest :libadb:testDebugUnitTest` | The Android app's and libadb's JVM tests (with `JAVA_HOME` at a JDK 17) |

## Conventions

- Vocabulary everywhere (UI, comments, docs): **Carbon**, **Silicon**, **custodian**, **Silicon
  Interface**, **Glass**. Never "organization", "org", "Team" (for a grouping of accounts), "human"
  or "AI agent"; when Silicons of one custodian matter, say "you and the Silicons you look after".
- Every error carries a stable code, a message that says what and why, and a hint; the CLI maps
  codes to exit codes (`ErrorCode::exit_code`).
- The contract files in `understanding/` change first, then the code. `UNDERSTANDING.md` is the
  Carbon's alone; changes to the other contract files need a Carbon's approval.
- Record every change to `vendor/extend-engine` in its `FORK.md` (newest first), and every new
  shipped dependency in `THIRD_PARTY_NOTICES.md`. The engine's settings are `EXTEND_ENGINE_<X>`: use
  only those names in Extend's code, scripts, CI and docs (the engine maps each onto its fork's
  `AGENT_DEVICE_<X>`, so the old names still work for anyone who set them). The desktop agent finds
  the engine through `EXTEND_ENGINE` (a path to `bin/extend-engine.mjs` or an executable).
- Setup errors (`SetupStep.error`) are one or two sentences for the Carbon: what is wrong and what to
  do. Environment variables, file paths, build commands, exit codes and stack traces go to the log.
- End-to-end lanes that drive a desktop run only on a test machine or in a container, never on a
  Mac someone is using.
