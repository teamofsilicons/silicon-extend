# Deployment

How the pieces ship:

- the service, on the AWS stack in [`deploy/aws/README.md`](../deploy/aws/README.md)
  (`deploy/aws/standalone.yaml`: one ARM64 EC2 host with Caddy in front and a private RDS
  PostgreSQL 17, with its first-deploy, release and rollback steps);
- the website, on Vercel;
- the `extend` CLI, through Silicon Apps, which installs it and keeps it up to date;
- the device apps, on the [releases page](https://github.com/teamofsilicons/silicon-extend/releases).

The production switch from Extend 3 (Silicon IAM, Honeycomb) to Extend 4 has its own runbook:
[docs/migration/cutover.md](migration/cutover.md). How releases were made before Extend 4 is in
[docs/history/deployment-1.x-3.x.md](history/deployment-1.x-3.x.md).

## Service (`backend.extend.teamofsilicons.com`)

- Image: `docker build -t silicon-extend .` (see `Dockerfile`; `extend-service serve`, port 8080,
  healthcheck `/ready`). The image sets `EXTEND_ENVIRONMENT=production` and carries `LICENSE`,
  `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` in `/usr/share/doc/silicon-extend/`. Run `extend-service migrate` once per release, or let `serve` migrate at
  start (migrations take an advisory lock, so concurrent starts are safe).
- **Run one instance.** Device sockets and in-flight commands live in the process (see
  [operations.md](operations.md)). Whatever terminates HTTPS in front of it (a load balancer, or
  Caddy on the host as in `deploy/aws/`) needs a WebSocket idle timeout ≥ 60 s (the service pings
  every 15 s), and its address in `EXTEND_TRUSTED_PROXY_CIDRS`.
- PostgreSQL 17 (CI runs 17.9). Extend's data is in the `extend` schema (and `extend_global`). Since
  4.0 there are no test environments; the `extend_test_<uuid>` schemas older versions made stay until
  a manual cleanup drops them.
- A persistent volume for `EXTEND_DATA_DIR` isn't needed in production (uploads are staged there for
  seconds before going to Briefcase), but it must be writable.

Required environment in production (`EXTEND_ENVIRONMENT=production` refuses the local stand-ins and
plain-http URLs; `crates/extend-service/.env.example` lists every variable):

| Variable | Value |
|---|---|
| `EXTEND_ENVIRONMENT` | `production`. Required everywhere: an unset value refuses to start rather than guess (`development` and `test` run with the local stand-ins). The Docker image sets it. |
| `EXTEND_DATABASE_URL` | PostgreSQL URL |
| `EXTEND_PUBLIC_URL` | `https://backend.extend.teamofsilicons.com` |
| `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com`: the Silicon Accounts public origin, every access token's `iss` |
| `ACCOUNTS_API_URL` | Optional: how the service reaches Silicon Accounts server to server (defaults to `ACCOUNTS_URL`) |
| `EXTEND_APP_ID`, `EXTEND_APP_SECRET` | Extend's app in Silicon Apps (`extend`) and its app secret, for introspection, lookups and proofs |
| `EXTEND_ACCOUNTS_WEBHOOK_SECRET` | The signing secret of Extend's webhook in Silicon Accounts (`POST /webhooks/accounts`; `EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET` during a rotation). Production refuses to start without it. |
| `EXTEND_DELEGATION_ENCRYPTION_KEY` | 32 random bytes, unpadded base64url: seals the Briefcase proof refresh tokens Extend keeps. Production refuses to start without it. |
| `EXTEND_BRIEFCASE_URL`, `EXTEND_BRIEFCASE_WEB_URL` | Briefcase API and web origins |
| `EXTEND_TING_URL` | Optional: Ting's API. Unset, notifications through Ting are off (requests and wake requests stay on the website, in the CLI and on the device). |
| `EXTEND_POSTMARK_SERVER_TOKEN` | For `extend report` emails. Production refuses to start without it. |
| `EXTEND_WEBSITE_URL`, `EXTEND_DOCS_URL` | Public links returned by `/api/v2/accounts` |
| `EXTEND_CORS_ORIGINS` | Optional: origins allowed to call the API from a page directly (the website calls it from its own server) |
| `EXTEND_TRUSTED_PROXY_CIDRS` | The proxy or load balancer addresses (comma-separated CIDRs) whose `X-Forwarded-For` Extend believes, so the per-address enrollment limit counts clients, not the proxy |

Optional, for API versioning (TECHNICAL.md section 10): `EXTEND_DEPRECATED_API_VERSIONS` (a comma
list of majors to deprecate, applied at start; never the newest one this build serves),
`EXTEND_API_V{n}_CLIENT_CRATE` and `EXTEND_API_V{n}_CLI` (the compatible version ranges the matrix
reports; API v1 defaults to `>=1.0.0, <4.0.0` and API v2 to `>=4.0.0, <5.0.0`), and
`EXTEND_DEVICE_APP_MIN_VERSION` (default `1.0.0`).
`EXTEND_REPORT_RECIPIENTS` overrides where bug reports go, and `EXTEND_MAX_PAIRS_PER_DEVICE` (default 8)
limits Carbons per device. Variables of 3.x (`EXTEND_IAM_*`, `EXTEND_HONEYCOMB_SERVICE_TOKEN`,
`EXTEND_LOCAL_MEMBERS`, the membership-sweep and owner-check settings, `EXTEND_TEST_LINK_WINDOW_S`) are
ignored, and the service logs each one it finds at start.

Extend is the app `extend` in Silicon Apps and Silicon Accounts (its app secret goes in
`EXTEND_APP_SECRET`). In Silicon Accounts it has:

- its sign-in setup: the website's callback (`https://extend.teamofsilicons.com/auth/callback`) and
  the CLI as a public client (device flow, and short-lived tokens from Silicons);
- the webhook `https://backend.extend.teamofsilicons.com/webhooks/accounts` with the events
  `account.id_changed`, `account.updated`, `account.deleted`, `membership.signed_out`,
  `membership.access_removed` and `silicon.custodian_changed`; its signing secret goes in
  `EXTEND_ACCOUNTS_WEBHOOK_SECRET`.

Briefcase accepts User verification proofs from `extend` for `briefcase.uploads.reserve`,
`briefcase.uploads.commit`, `briefcase.uploads.status`, `briefcase.uploads.cancel`,
`briefcase.files.read`, `briefcase.invitations.create` and `briefcase.entries.trash` (Briefcase's
allow-list). When Ting delivery is turned on (`EXTEND_TING_URL`), Ting accepts App verification
proofs from `extend` for `tings.send` and User verification proofs for `tings.subscribe`, and its
four types are registered in Ting (`extend.device.requested`, `extend.device.wake_requested`,
`extend.device.woken`, `extend.device.wake_declined`).

### Moving a 3.x deployment to Extend 4

The service migrates the data in place (schema 9, additive), and the old public ids are re-keyed
to Silicon Accounts uuids with `extend-service identity suggest` and `identity apply`. A 3.x
service can't run on schema 9, so snapshot the database first: rolling back is restoring it. Every
step, in order, with its checks and its rollback, is in [the cutover runbook](migration/cutover.md).

## Website (`extend.teamofsilicons.com`)

The Vercel project `silicon-extend-web` (root directory `web/`) serves the website at
`extend.teamofsilicons.com`. Extend 4's website is a Next.js server that signs Carbons in through
Silicon Accounts' hosted pages and calls the service with their access token from its own server:
the browser never holds a token, and the app secret and the session key are server-only Vercel
settings, never in the repository. Silicon Accounts must list
`https://extend.teamofsilicons.com/auth/callback` among Extend's redirect URIs. Its settings are
the shared web kit's: `APP_ID` (`extend`), `APP_SECRET` (Extend's app secret), `ACCOUNTS_URL`
(`https://accounts.teamofsilicons.com`), `APP_API_URL` (`https://backend.extend.teamofsilicons.com`),
`SESSION_SECRET` (a new `openssl rand -base64 48`) and `PUBLIC_URL`
(`https://extend.teamofsilicons.com`); `web/README.md` has how to run and check it. (The Next.js
site is built in the migration's web stages; until it lands, `web/` still holds Extend 3's Vite
site, which can't sign in to the 4.0 service.)

## CLI (Silicon Apps)

The `extend` CLI is the app `extend` in Silicon Apps. Silicons install it with
`silicon-apps install extend`; Silicon Apps' updater moves every installed copy to a new production
release within about a minute, so the CLI has no updater of its own and never asks anyone to update
by hand (`silicon-apps update extend` checks at once). A release is one `.tar.gz` per target, each
with an `apps.yaml` that lists only that target, the binary at `bin/extend` (`bin/extend.exe` on
Windows) and `licences/`; the version is the CLI crate's (`crates/extend-cli/Cargo.toml`).

**Build the archives.** Tag `cli-v<CLI version>` (a CLI-only release) or `v<workspace version>` (the
whole product, desktop apps included) and push the tag; `.github/workflows/release.yml` builds the
six targets (Linux on Ubuntu 24.04, so the binaries need glibc 2.39 at most), checks each binary on
the runner that built it, and packs the archives with `scripts/package-apps.sh`. Its artifact
`extend-silicon-apps-release` holds `extend-<version>-<target>.tar.gz`, a `.sha256` for each and
`SHA256SUMS`. It publishes nothing. To pack one by hand:

```sh
cargo build --release --locked -p silicon-extend-cli
scripts/package-apps.sh 4.0.0 macos-aarch64 target/release/extend   # dist/apps/extend-4.0.0-macos-aarch64.tar.gz
```

The script refuses a version other than the CLI crate's, a binary built for another system or
processor, a Linux binary that needs a newer glibc, and, where this machine can run the binary, one
that doesn't answer `extend --help`, `extend accounts --json` and `extend login status --json` the
way Silicon Apps requires in an empty home. `python3 -m unittest discover -s scripts -p 'test_*.py'`
tests it (the full pack only where `silicon-apps` 0.2 is installed:
`cargo install --locked silicon-apps-cli --version 0.2.0`).

**Upload and release**, from a Carbon session that is an author of `extend` (run at release time):

```sh
silicon-apps login --slt "$(silicon-accounts login --app silicon-apps -q)"
sha256sum --check SHA256SUMS                     # in the downloaded artifact
silicon-apps upload extend --target linux-x86_64 extend-4.0.0-linux-x86_64.tar.gz
silicon-apps upload extend --target linux-aarch64 extend-4.0.0-linux-aarch64.tar.gz
silicon-apps packages extend                     # both accepted: the three commands passed on their workers
silicon-apps release extend --version 4.0.0 --package <linux-x86_64 id> --package <linux-aarch64 id>
silicon-apps install 'extend>dev' --yes          # on a test machine: the development release works
silicon-apps promote extend <development release id> --version 4.0.0
```

Today Silicon Apps validates only Linux targets (the Silicon fleet runs `linux-x86_64` and
`linux-aarch64`); an upload for macOS or Windows is refused until its validation worker is live
(`silicon-apps capabilities` shows which are). Keep those four archives from the same run and
upload them, then cut a release that includes them, once their workers answer. A bad release is
withdrawn with `silicon-apps withdraw extend <release id> --reason "…"`; installed copies move off it
on their next check.

**Crates.** Publish `silicon-extend-protocol`, then `silicon-extend-client`, then
`silicon-extend-cli` (each depends on the one before), waiting for each to appear on crates.io:
`cargo publish --locked -p <crate>` from the release tag after
`cargo package --locked -p silicon-extend-protocol -p silicon-extend-client -p silicon-extend-cli`.
For a CLI-only patch, change only `crates/extend-cli/Cargo.toml` (and `Cargo.lock`), tag
`cli-v<version>`, publish only `silicon-extend-cli`, and create its GitHub release with
`--latest=false` so the website's latest desktop and Android downloads keep pointing at the full app
release.

## Device apps

The device apps don't sign in to Silicon Accounts: they pair with a code and keep their own Extend
device credential. Extend 4 didn't change what they speak to the service (`/api/v1/device…`,
`/api/v1/enrollments…` and the WebSocket frames are unchanged), so installed apps keep working
without an update; their next release carries Extend 4's wording (a pair is the Carbon's own, with
no grouping to import it into). They stay on GitHub Releases and the website's download pages,
because Silicon Apps distributes command-line apps.

Android: `apps/android` builds the APK, package `com.teamofsilicons.extend` (`versionName` 1.1.2,
`versionCode` 6 in `apps/android/app/build.gradle.kts`). A release APK is signed
only when `EXTEND_ANDROID_SIGNING_PROPERTIES` names a properties file with `storeFile`,
`storePassword`, `keyAlias` and `keyPassword` (a relative `storeFile` is resolved against
`apps/android/app`); otherwise it is left unsigned, never debug-signed.

Desktop: `apps/desktop` builds the macOS app bundle, the Linux tarball and `.deb`, and the Windows
zip. Distribute only the Mac zip without a suffix (`Silicon-Extend-<version>-macos-<arch>.zip`),
which `build-app.sh` produces only after Apple accepted the notarization and the ticket is stapled;
`-unnotarized` and `-adhoc` zips are for testing. Releases use the existing Developer ID Application
identity of Apple Developer team `LTBSK59BJ2`; the 1.1 Mac candidate is signed, notarized and stapled. Changing the
signing identity can reset the Accessibility and Screen Recording grants Carbons gave the app.
Windows x64 has been built and verified on Windows. Windows ARM64 has a manual terminal-only
verification option because the hosted runner's privacy setup screen blocks the owned GUI fixture;
an artifact from that option must disclose that its GUI behavior remains unverified. Default and
tag-triggered Windows verification still requires both native fixtures.

## Licences in what ships

Every artifact carries Team of Silicons' MIT licence and third-party code
([`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md), which says where each artifact keeps them).
Every Silicon Apps archive of the CLI (in `licences/`), the service image and the Mac, Linux
and Windows packages ship `LICENSE`,
`THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` (every Rust crate's licence text, from
`cargo about`); the Mac and Linux packages also ship the device engine's licence (the MIT licence of
the project it is forked from, `vendor/extend-engine/LICENSE`) and Node.js's; the Android app shows
its notices. The website serves `/licences.txt`, written by
`web/scripts/gen-licences.mjs` on every build.
