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
and runs Extend against it.

## Tests

| Command | What it covers |
|---|---|
| `cargo test --workspace` | Unit tests everywhere, plus `crates/extend-service/tests/e2e.rs`: pairing, sessions, relay, files, takeover, revocation, idle and pair expiry, test environments, versioning — real PostgreSQL (`EXTEND_TEST_ADMIN_URL`), real HTTP/WebSocket, a scripted device |
| `bash e2e/cli-e2e.sh` | The `extend` CLI end to end against a running service and `examples/fake_device` |
| `cd web && pnpm test && pnpm test:e2e` | Website unit tests and Playwright against the mock API; `pnpm test:e2e:real` against the running service |
| `apps/android` | See its README (unit tests, emulator runs) |
| `crates/extend-agent`, `apps/desktop/linux-e2e` | Desktop agent tests; Linux run in Docker |

## Conventions

- Vocabulary everywhere (UI, comments, docs): **Carbon**, **Silicon**, **Team**, **Silicon Interface**,
  **Glass**.
- Every error carries a stable code, a message that says what and why, and a hint; the CLI maps
  codes to exit codes (`ErrorCode::exit_code`).
- The contract files in `understanding/` change first, then the code.
