# Development

## Prerequisites

Rust 1.98 (pinned in `rust-toolchain.toml`), Docker, Node 22+ with pnpm (website, agent-device fork),
and for device apps: the Android SDK (API 36) with a JDK 17+, and Xcode on a Mac.

## The service

```sh
docker run -d --name silicon-extend-postgres -e POSTGRES_USER=extend -e POSTGRES_PASSWORD=extend \
  -e POSTGRES_DB=extend -p 127.0.0.1:5440:5432 postgres:16.9-bookworm
set -a; . e2e/dev.env; set +a
cargo run -p extend-service          # http://127.0.0.1:8480, migrations run at start
```

`e2e/dev.env` runs with local stand-ins for Silicon IAM, Briefcase and Ting (all refused in
production):

- **Local IAM**: the SLT is a member id (`c:alice`, `si:chef@acme`). Members come from
  `EXTEND_LOCAL_MEMBERS`. `POST /dev/iam/members` simulates membership changes and logouts (and the
  webhook that follows); `GET /dev/iam/login?app_id=&redirect_uri=` is a stand-in consent screen.
- **Local files**: stored under `EXTEND_DATA_DIR/files`, served at `/dev/files/{id}`.
- **Local Ting**: requests are recorded; `GET /dev/ting` lists them.
- **Test environments**: prepare one with Honeycomb's lifecycle endpoint (token
  `hck_local_dev_token`), then register a test app secret with `POST /dev/iam/test-apps`.

Against a real Silicon IAM: `EXTEND_IAM_MODE=sdk`, `EXTEND_IAM_BASE_URL`, `EXTEND_IAM_APP_ID`,
`EXTEND_IAM_APP_SECRET`, `EXTEND_IAM_WEBHOOK_SECRET[_VERSION]`. `e2e/real-iam/` brings up a local IAM
and runs Extend against it; with `--briefcase` and `--ting` it also runs a real Briefcase (with
MinIO) and a real Ting, reached through IAM OBO (`EXTEND_FILES_MODE=briefcase`,
`EXTEND_TING_MODE=ting`). See its README.

The shared development service limits new enrollments to 60 per hour per address, in memory. When
several people or agents run end-to-end lanes, start your own instance with the same `e2e/dev.env`
but another `EXTEND_BIND`/`EXTEND_PUBLIC_URL` port, its own `EXTEND_DATABASE_URL` database and its
own `EXTEND_DATA_DIR`, and point the lane at it (`bash e2e/cli-e2e.sh http://127.0.0.1:<port>`).

## Tests

| Command | What it covers |
|---|---|
| `cargo test --workspace` | Unit tests everywhere, plus `crates/extend-service/tests/e2e.rs`: pairing, sessions, relay, files, takeover, revocation, idle and pair expiry, test environments, versioning — real PostgreSQL (`EXTEND_TEST_ADMIN_URL`), real HTTP/WebSocket, a scripted device. `tests/obo_requests.rs` checks Briefcase and Ting requests against mock servers; `tests/real_services.rs` runs only with `EXTEND_REALIAM_STATE` set. `e2e/clean-test-dbs.sh` drops the throwaway `extend_e2e_*` databases |
| `cargo test -p extend-cli` | Includes `tests/device_args.rs`: the real binary against a fake service, reading the exact arguments a device receives |
| `bash e2e/cli-e2e.sh` | The `extend` CLI end to end against a running service and `examples/fake_device` |
| `cd web && pnpm test && pnpm test:e2e` | Website unit tests and Playwright against the mock API; `pnpm test:e2e:real` against the running service |
| `apps/android` | See its README (unit tests, emulator runs, the notices generator and dependency verification) |
| `crates/extend-agent`, `apps/desktop/linux-e2e` | Desktop agent tests; Linux run and recording lanes in Docker (`apps/desktop/README.md`) |
| `node --test apps/desktop/stamp-runtime.test.mjs apps/desktop/runtime-entry.test.mjs` | Packaged runtime stamp and entry |
| `cd vendor/agent-device && pnpm typecheck && pnpm exec vitest run …` | The agent-device fork; `pnpm test:macos-helper` for the Swift helper |

## Conventions

- Vocabulary everywhere (UI, comments, docs): **Carbon**, **Silicon**, **Team**, **Silicon Interface**,
  **Glass**.
- Every error carries a stable code, a message that says what and why, and a hint; the CLI maps
  codes to exit codes (`ErrorCode::exit_code`).
- The contract files in `understanding/` change first, then the code. `UNDERSTANDING.md` is the
  Carbon's alone; changes to the other contract files need a Carbon's approval.
- Record every change to `vendor/agent-device` in its `FORK.md` (newest first), and every new
  shipped dependency in `THIRD_PARTY_NOTICES.md`.
- End-to-end lanes that drive a desktop run only on a test machine or in a container, never on a
  Mac someone is using.
