# Silicon Bridge

Silicon Bridge lets a Silicon use a Carbon's personal devices — Android phones and tablets, Android
TV, iPhone, iPad, Mac, Windows, Linux, Apple TV and Samsung/LG TVs — the way the Carbon uses them.
The Carbon pairs each device and decides which Silicons may use it. One Silicon uses a device at a
time, the device always shows who is using it, and the Carbon can stop it with one tap.

Bridge is built on [our fork of agent-device](vendor/agent-device/FORK.md), which reads a screen as
a list of things to act on (buttons, fields, lists) and acts on them.

**Start here:** product intent is [`understanding/UNDERSTANDING.md`](understanding/UNDERSTANDING.md)
(Carbon-edited). The wire contracts are [`api.yaml`](understanding/api.yaml) and
[`cli.yaml`](understanding/cli.yaml); formats and flows are in
[`TECHNICAL.md`](understanding/TECHNICAL.md).

## Use it

```sh
honeycomb install 'bridge'
bridge login <slt>                      # a short-lived token from Silicon IAM; Bridge never asks for a password
bridge device ls                        # Silicon: devices you can use. Carbon: devices you paired
bridge device show 7c1e09ab             # what you can do on it right now
bridge session new 7c1e09ab --connect   # one Silicon at a time; prints the session id (a3f)
bridge snapshot -i                      # read the screen as elements with @refs
bridge click @e2
bridge screenshot --ttl 7d              # stored in Briefcase, link printed
bridge session end
```

A Carbon pairs devices on [bridge.teamofsilicons.com](https://bridge.teamofsilicons.com) or with
`bridge device pair <code> --name <name> --access si:chef`. `bridge --help` is a tree of
documentation; every node explains itself. More: [docs/cli.md](docs/cli.md).

## What's here

| Path | What |
|---|---|
| `crates/bridge-protocol` | Wire types shared by everything: identifiers, envelopes, errors, WebSocket frames, capabilities |
| `crates/bridge-service` | The Bridge service (Rust, axum, PostgreSQL): pairing, access, sessions, the relay to devices, test environments |
| `crates/silicon-bridge-client` | The official Rust client ([docs/client.md](docs/client.md)) |
| `crates/bridge-cli` | The `bridge` command, built only on the client |
| `crates/bridge-agent` | The Bridge app for Mac, Windows and Linux (menu bar / tray + device agent) |
| `crates/bridge-driver` | The interface every device driver implements |
| `crates/bridge-hosted` | Drivers for devices a computer carries: iPhone, iPad, Apple TV, Samsung, LG |
| `apps/android` | The Bridge app for Android phones, tablets, Android TV, Google TV and Fire OS |
| `apps/desktop` | Packaging for the desktop app |
| `web` | The configuration website (SolidJS) |
| `vendor/agent-device` | Our fork of agent-device |
| `docs` | [Device protocol](docs/device-protocol.md), [development](docs/development.md), [deployment](docs/deployment.md), [operations](docs/operations.md) |
| `e2e` | End-to-end suites and fixtures |

## Develop

See [docs/development.md](docs/development.md). In short:

```sh
docker run -d --name silicon-bridge-postgres -e POSTGRES_USER=bridge -e POSTGRES_PASSWORD=bridge -e POSTGRES_DB=bridge -p 127.0.0.1:5440:5432 postgres:16.9-bookworm
cargo test --workspace                        # unit + service end-to-end (creates throwaway databases)
set -a; . e2e/dev.env; set +a; cargo run -p bridge-service &
cargo build -p bridge-cli -p bridge-service --example fake_device && bash e2e/cli-e2e.sh
cd web && pnpm install && pnpm dev            # website against the local service
```

## Found a bug?

`bridge report "what happened" --pr <link>`. Bridge is open source: reproduce it, patch it, open a
pull request here, and report it with the PR attached.
