# Silicon Extend

Silicon Extend lets a Silicon use a Carbon's personal devices — Android phones and tablets, Android
TV, iPhone, iPad, Mac, Windows, Linux, Apple TV and Samsung/LG TVs — the way the Carbon uses them.
The Carbon pairs each device and decides which Silicons may use it. One Silicon uses a device at a
time, the device always shows who is using it, and the Carbon can stop it with one tap.

Extend is built on its device engine ([`vendor/extend-engine`](vendor/extend-engine/FORK.md)), which
reads a screen as a list of things to act on (buttons, fields, lists) and acts on them.

**Start here:** product intent is [`understanding/UNDERSTANDING.md`](understanding/UNDERSTANDING.md)
(Carbon-edited). The wire contracts are [`api.yaml`](understanding/api.yaml) and
[`cli.yaml`](understanding/cli.yaml), and formats and flows are in
[`TECHNICAL.md`](understanding/TECHNICAL.md). Those files still describe Extend 3; Extend 4's
versions wait for a Carbon's approval in [`docs/migration/contracts/`](docs/migration/contracts/),
with the proposed UNDERSTANDING changes in
[`docs/migration/understanding-proposal.md`](docs/migration/understanding-proposal.md).

**Extend 4** signs everyone in with [Silicon Accounts](https://accounts.teamofsilicons.com) and
ships its CLI through [Silicon Apps](https://apps.teamofsilicons.com) (developer docs:
[developers.teamofsilicons.com](https://developers.teamofsilicons.com)). Every account is personal:
a device belongs to the Carbon who paired it, that Carbon gives access to Silicons by their id, and
a Silicon's custodian sees and can stop what that Silicon does. The service runs at
`api.extend.teamofsilicons.com`, the website at
[extend.teamofsilicons.com](https://extend.teamofsilicons.com), and the device apps are on the
[releases page](https://github.com/teamofsilicons/silicon-extend/releases). Windows is a preview.
The switch from Extend 3 is in [docs/migration/](docs/migration/) (decisions, the cutover runbook,
progress); records from before it are in [docs/history/](docs/history/). This product was called
Silicon Bridge until 2026-09-26.

## Use it

```sh
silicon-apps install extend                 # Silicon Apps installs the CLI and keeps it up to date
silicon-accounts login --app extend -q | extend login --slt-stdin   # a Silicon signs in; no password, no page
extend login                                # a Carbon signs in by approving the code it shows
extend device ls                            # Silicon: devices you can use. Carbon: devices you paired
extend device show 7c1e09ab                 # what you can do on it right now
extend session new 7c1e09ab --connect       # one Silicon at a time; prints the session id (a3f)
extend device wake 0d44e1f2 --reason "…"    # ask its Carbon to wake a device that isn't awake
extend snapshot -i                          # read the screen as elements with @refs
extend click @e2
extend screenshot --ttl 7d                  # stored in Briefcase, link printed
extend session end
```

A Carbon pairs devices on [extend.teamofsilicons.com](https://extend.teamofsilicons.com) (signed in
with Silicon Accounts) or with `extend device pair <code> --name <name> --access si:chef`, and sees
the Silicons they look after with `extend silicon ls`. `extend --help` is a tree of documentation;
every node explains itself. More: [docs/cli.md](docs/cli.md).

## What's here

| Path | What |
|---|---|
| `crates/extend-protocol` | Wire types shared by everything: identifiers, envelopes, errors, WebSocket frames, capabilities |
| `crates/extend-service` | The Extend service (Rust, axum, PostgreSQL): pairing, access, sessions, the relay to devices, Silicon Accounts sign-in and webhooks |
| `crates/silicon-extend-client` | The official Rust client ([docs/client.md](docs/client.md)) |
| `crates/extend-cli` | The `extend` command, built only on the client |
| `crates/extend-agent` | The Extend app for Mac, Windows and Linux (menu bar / tray + device agent) |
| `crates/extend-driver` | The interface every device driver implements |
| `crates/extend-hosted` | Drivers for devices a computer carries: iPhone, iPad, Apple TV, Samsung, LG |
| `apps/android` | The Extend app for Android phones, tablets, Android TV, Google TV and Fire OS |
| `apps/desktop` | Packaging for the desktop app |
| `web` | The configuration website |
| `vendor/extend-engine` | The device engine, our fork of an MIT-licensed project ([what changed](vendor/extend-engine/FORK.md)) |
| `docs` | [Device protocol](docs/device-protocol.md), [CLI](docs/cli.md), [client](docs/client.md), [development](docs/development.md), [deployment](docs/deployment.md), [operations](docs/operations.md), [release notes](docs/releases/), [history](docs/history/) |
| `contracts` | Consumer contract fixtures the service's CI replays ([format](contracts/README.md)) |
| `packaging`, `scripts` | The Silicon Apps manifest template and `scripts/package-apps.sh`, which packs one CLI archive per target |
| `deploy/aws` | The service's production stack ([runbook](deploy/aws/README.md)) |
| `e2e` | End-to-end suites and fixtures |

## Develop

See [docs/development.md](docs/development.md). In short, with PostgreSQL 16 or newer on
`127.0.0.1:5440` (user and password `extend`; any other server works through
`EXTEND_TEST_ADMIN_URL` and `EXTEND_DATABASE_URL`):

```sh
cargo test --workspace                        # unit + service end-to-end (creates throwaway databases)
set -a; . e2e/dev.env; set +a; cargo run -p extend-service &   # local Silicon Accounts, Briefcase and Ting stand-ins
cargo build -p silicon-extend-cli -p extend-service --example fake_device && bash e2e/cli-e2e.sh
```

## Found a bug?

`extend report "what happened" --pr <link>`. Extend is open source: reproduce it, patch it, open a
pull request here, and report it with the PR attached.

## Licence

MIT, see [`LICENSE`](LICENSE). Third-party components and their licences (the device engine, which is
a fork of an MIT-licensed project, libadb-android, spake2-android, Node.js, the fonts, Rust and
JavaScript dependencies) are listed in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
