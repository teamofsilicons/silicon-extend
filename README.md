# Silicon Extend

Silicon Extend lets a Silicon use a Carbon's personal devices — Android phones and tablets, Android
TV, iPhone, iPad, Mac, Windows, Linux, Apple TV and Samsung/LG TVs — the way the Carbon uses them.
The Carbon pairs each device and decides which Silicons may use it. One Silicon uses a device at a
time, the device always shows who is using it, and the Carbon can stop it with one tap.

Extend is built on [our fork of agent-device](vendor/agent-device/FORK.md), which reads a screen as
a list of things to act on (buttons, fields, lists) and acts on them.

**Start here:** product intent is [`understanding/UNDERSTANDING.md`](understanding/UNDERSTANDING.md)
(Carbon-edited). The wire contracts are [`api.yaml`](understanding/api.yaml) and
[`cli.yaml`](understanding/cli.yaml); formats and flows are in
[`TECHNICAL.md`](understanding/TECHNICAL.md).

## Use it

```sh
honeycomb install 'extend'
extend login <slt>                      # a short-lived token from Silicon IAM; Extend never asks for a password
extend device ls                        # Silicon: devices you can use. Carbon: devices you paired
extend device show 7c1e09ab             # what you can do on it right now
extend session new 7c1e09ab --connect   # one Silicon at a time; prints the session id (a3f)
extend snapshot -i                      # read the screen as elements with @refs
extend click @e2
extend screenshot --ttl 7d              # stored in Briefcase, link printed
extend session end
```

A Carbon pairs devices on [extend.teamofsilicons.com](https://extend.teamofsilicons.com) or with
`extend device pair <code> --name <name> --access si:chef`. `extend --help` is a tree of
documentation; every node explains itself. More: [docs/cli.md](docs/cli.md).

## What's here

| Path | What |
|---|---|
| `crates/extend-protocol` | Wire types shared by everything: identifiers, envelopes, errors, WebSocket frames, capabilities |
| `crates/extend-service` | The Extend service (Rust, axum, PostgreSQL): pairing, access, sessions, the relay to devices, test environments |
| `crates/silicon-extend-client` | The official Rust client ([docs/client.md](docs/client.md)) |
| `crates/extend-cli` | The `extend` command, built only on the client |
| `crates/extend-agent` | The Extend app for Mac, Windows and Linux (menu bar / tray + device agent) |
| `crates/extend-driver` | The interface every device driver implements |
| `crates/extend-hosted` | Drivers for devices a computer carries: iPhone, iPad, Apple TV, Samsung, LG |
| `apps/android` | The Extend app for Android phones, tablets, Android TV, Google TV and Fire OS |
| `apps/desktop` | Packaging for the desktop app |
| `web` | The configuration website (SolidJS) |
| `vendor/agent-device` | Our fork of agent-device |
| `docs` | [Device protocol](docs/device-protocol.md), [development](docs/development.md), [deployment](docs/deployment.md), [operations](docs/operations.md) |
| `e2e` | End-to-end suites and fixtures |

## Develop

See [docs/development.md](docs/development.md). In short:

```sh
docker run -d --name silicon-extend-postgres -e POSTGRES_USER=extend -e POSTGRES_PASSWORD=extend -e POSTGRES_DB=extend -p 127.0.0.1:5440:5432 postgres:16.9-bookworm
cargo test --workspace                        # unit + service end-to-end (creates throwaway databases)
set -a; . e2e/dev.env; set +a; cargo run -p extend-service &
cargo build -p extend-cli -p extend-service --example fake_device && bash e2e/cli-e2e.sh
cd web && pnpm install && pnpm dev            # website against the local service
```

## Found a bug?

`extend report "what happened" --pr <link>`. Extend is open source: reproduce it, patch it, open a
pull request here, and report it with the PR attached.
