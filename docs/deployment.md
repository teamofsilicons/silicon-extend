# Deployment

How the pieces ship: the service on the AWS stack in
[`deploy/aws/README.md`](../deploy/aws/README.md) (`deploy/aws/standalone.yaml`: one ARM64 EC2 host
with Caddy in front and a private RDS PostgreSQL 17, with its first-deploy, release and rollback
steps), the website on Vercel, the CLI through Honeycomb, and the apps on the releases page. 1.1.0
follows the service-first order in [Releasing 1.1.0](#releasing-110) below. Its behavior changes and
disclosed coverage gaps are in [the release notes](releases/1.1.0.md).

## Service (`backend.extend.teamofsilicons.com`)

- Image: `docker build -t silicon-extend .` (see `Dockerfile`; `extend-service serve`, port 8080,
  healthcheck `/ready`). The image sets `EXTEND_ENVIRONMENT=production` and carries `LICENSE`,
  `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` in `/usr/share/doc/silicon-extend/`. Run `extend-service migrate` once per release, or let `serve` migrate at
  start (migrations take an advisory lock, so concurrent starts are safe). Both migrate production
  and every test environment.
- **Run one instance.** Device sockets and in-flight commands live in the process (see
  [operations.md](operations.md)). Whatever terminates HTTPS in front of it (a load balancer, or
  Caddy on the host as in `deploy/aws/`) needs a WebSocket idle timeout ≥ 60 s (the service pings
  every 15 s), and its address in `EXTEND_TRUSTED_PROXY_CIDRS`.
- PostgreSQL 17 (CI runs 17.9). Each test environment gets its own schema (`extend_test_<uuid>`), created on
  Honeycomb's `prepare` and dropped on `purge`.
- A persistent volume for `EXTEND_DATA_DIR` isn't needed in production (uploads are staged there for
  seconds before going to Briefcase), but it must be writable.

Required environment in production (`EXTEND_ENVIRONMENT=production` refuses local stand-ins,
member-id logins and plain-http public URLs):

| Variable | Value |
|---|---|
| `EXTEND_ENVIRONMENT` | `production`. Required everywhere: an unset value refuses to start rather than guess (`development` and `test` run with the local stand-ins). The Docker image sets it. |
| `EXTEND_DATABASE_URL` | PostgreSQL URL |
| `EXTEND_PUBLIC_URL` | `https://backend.extend.teamofsilicons.com` |
| `EXTEND_IAM_APP_ID`, `EXTEND_IAM_APP_SECRET` | Extend's Silicon IAM application |
| `EXTEND_IAM_WEBHOOK_SECRET`, `EXTEND_IAM_WEBHOOK_SECRET_VERSION` | IAM webhook signing secret for `/webhook/` (and `…_PREVIOUS_…` during rotation). Production refuses to start without it. |
| `EXTEND_IAM_LOGIN_URL` | IAM sign-in page (default `https://auth.iam.teamofsilicons.com/login`) |
| `EXTEND_BRIEFCASE_URL`, `EXTEND_BRIEFCASE_WEB_URL` | Briefcase API and web origins |
| `EXTEND_TING_URL` | Ting API |
| `EXTEND_HONEYCOMB_SERVICE_TOKEN` | Honeycomb's lifecycle credential |
| `EXTEND_POSTMARK_SERVER_TOKEN` | For `extend report` emails. Production refuses to start without it. |
| `EXTEND_WEBSITE_URL`, `EXTEND_DOCS_URL` | Public links returned by `/api/v1/iam` |
| `EXTEND_TRUSTED_PROXY_CIDRS` | The proxy or load balancer addresses (comma-separated CIDRs) whose `X-Forwarded-For` Extend believes, so the per-address enrollment limit counts clients, not the proxy |

Optional, for API versioning (TECHNICAL.md section 10): `EXTEND_DEPRECATED_API_VERSIONS` (a comma
list of majors to deprecate, applied at start; never the newest one this build serves),
`EXTEND_API_V{n}_CLIENT_CRATE` and `EXTEND_API_V{n}_CLI` (the compatible version ranges the matrix
reports; API v1 defaults to `>=1.0.0, <4.0.0` for ordinary operations), and
`EXTEND_DEVICE_APP_MIN_VERSION` (default `1.0.0`).
`EXTEND_REPORT_RECIPIENTS` overrides where bug reports go. Optional since 1.1: `EXTEND_MAX_PAIRS_PER_DEVICE`,
`EXTEND_MEMBERSHIP_SWEEP_HOURS`, `EXTEND_OWNER_CHECK_CACHE_S`, `EXTEND_OWNER_CHECK_AT_USE` and
`EXTEND_TEST_LINK_WINDOW_S` ([operations.md](operations.md#settings-added-in-11)); the defaults are
the ones the Carbon accepted.

Extend is registered through Honeycomb: `honeycomb apps create application.json` creates the IAM
application (app id `extend`, team `tos`) and returns the app secret once. `application.json`
declares:

- the webhook `https://backend.extend.teamofsilicons.com/webhook/` and its signing secret, which
  also goes in `EXTEND_IAM_WEBHOOK_SECRET`. The destination then needs a Carbon's step-up approval:
  `honeycomb apps webhook approve`;
- the IAM scopes `self.identity.read`, `self.profile.read`, `self.organizations.read`,
  `self.membership.read`, `directory.silicons.read`, `directory.carbons.read`,
  `directory.memberships.read` and `directory.profiles.read`;
- the external OBO endpoints Extend calls: Briefcase `briefcase.uploads.reserve`,
  `briefcase.uploads.commit`, `briefcase.uploads.status`, `briefcase.invitations.create`,
  `briefcase.entries.trash` and `briefcase.files.read`, plus Ting `tings.send` and
  `subscriptions.register`. Each feature needs its own approved OBO grant after login.
  `briefcase.uploads.cancel` is supported for explicit cleanup; the normal store flow does
  not call it. The retired raw `briefcase.files.create` endpoint is not used. See the
  [OBO cutover guide](OBO_CUTOVER.md#exact-provider-endpoints) for the full contract.

Ting 0.1.9 resolves types by context and application, across delivery Teams. A manager of the
app's owning Team (`tos` for production Extend) registers all four once in production and in each
used test context, and again after a clean removes them. Delivery Teams do not duplicate them:

```sh
ting --org <app-owning-team> types register --type extend.device.requested --description 'A Silicon asks to use a device another Silicon is using'
ting --org <app-owning-team> types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'
ting --org <app-owning-team> types register --type extend.device.woken --description 'A device a Silicon asked to wake is awake'
ting --org <app-owning-team> types register --type extend.device.wake_declined --description 'A Carbon turned down a request to wake a device'
```

All four types are now registered in the production app-owning Team, `tos`; no notification was
sent during registration. The runbook for new
Teams and for finding missing types is in [operations.md](operations.md#extends-ting-types).
`e2e/real-iam/realiam.py --briefcase --ting` seeds this catalog against local services and is the
reference for it; it verifies delivery across Teams and genuine missing-type failures. The current
Ting OBO catalog has no `types.register` endpoint, so registration uses the manager CLI.

## Website (`extend.teamofsilicons.com`)

A static Vite build (`web/dist`), deployed like the sibling sites on Vercel with
`VITE_EXTEND_API_URL=https://backend.extend.teamofsilicons.com`. The service can also serve it with
`EXTEND_WEB_DIR`.

## CLI

`.github/workflows/release.yml` builds the six CLI targets on tag `v<version>` or `cli-v<version>`, and
`scripts/package-cli.py` validates and packs the Honeycomb archive (`honeycomb.yaml`, and in each
target's root the executable and `licences/`; `honeycomb pack` drops anything outside the target
roots). `python3 -m unittest discover -s scripts -p 'test_*.py'` runs it with stand-in executables
when `honeycomb` is installed. Then upload the archive with `honeycomb releases upload` from a
Carbon session, like the sibling apps.

For a CLI-only patch, set the version in `crates/extend-cli/Cargo.toml` and `honeycomb.yaml`,
update `Cargo.lock`, and tag `cli-v<version>`. This skips desktop app builds and leaves the
workspace/client/protocol versions unchanged. Publish only `silicon-extend-cli` to crates.io.
Create its GitHub release with `--latest=false` so the website's latest desktop/Android downloads
continue pointing at the full app release.

CLI/client/service 2.0.0 use independent crate versions; the CLI follows the same `cli-v` archive
lane. Publish `silicon-extend-client` before `silicon-extend-cli` and deploy service 2.0.0 as part
of the release. The ordinary HTTP API remains v1. Android stays at 1.1.2 and desktop applications
and the device protocol stay at 1.1.0. The [2.0.0 release notes](releases/cli-2.0.0.md) describe
the intentionally removed API and the ordinary snapshot/ref workflow for upgrading callers.

Refresh the existing host's environment renderer from this release and remove retired provider
settings from its runtime secret (see the [AWS runbook](../deploy/aws/README.md)). An image update
alone does not refresh the renderer's allowlist. Preserve all unrelated secret settings and
verify only setting names or presence, never their values. No database migration or device-app
update is required for this release.

## Device apps

Android: `apps/android` builds the APK, package `com.teamofsilicons.extend` (1.1.0: `versionName`
1.1.0, `versionCode` 4). A release APK is signed
only when `EXTEND_ANDROID_SIGNING_PROPERTIES` names a properties file with `storeFile`,
`storePassword`, `keyAlias` and `keyPassword` (a relative `storeFile` is resolved against
`apps/android/app`); otherwise it is left unsigned, never debug-signed.

Desktop: `apps/desktop` builds the macOS app bundle, the Linux tarball and `.deb`, and the Windows
zip. Distribute only the Mac zip without a suffix (`Silicon-Extend-<version>-macos-<arch>.zip`),
which `build-app.sh` produces only after Apple accepted the notarization and the ticket is stapled;
`-unnotarized` and `-adhoc` zips are for testing. Releases use the existing Developer ID Application
identity for Team `LTBSK59BJ2`; the 1.1 Mac candidate is signed, notarized and stapled. Changing the
signing identity can reset the Accessibility and Screen Recording grants Carbons gave the app.
Windows x64 has been built and verified on Windows. Windows ARM64 has a manual terminal-only
verification option because the hosted runner's privacy setup screen blocks the owned GUI fixture;
an artifact from that option must disclose that its GUI behavior remains unverified. Default and
tag-triggered Windows verification still requires both native fixtures.

## Licences in what ships

Every artifact carries Team of Silicons' MIT licence and third-party code
([`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md), which says where each artifact keeps them).
The CLI archive, the service image and the Mac, Linux and Windows packages ship `LICENSE`,
`THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` (every Rust crate's licence text, from
`cargo about`); the Mac and Linux packages also ship the device engine's licence (the MIT licence of
the project it is forked from, `vendor/extend-engine/LICENSE`) and Node.js's; the Android app shows
its notices. The website serves `/licences.txt`, written by
`web/scripts/gen-licences.mjs` on every build.

## Releasing 1.1.0

1.1.0 is additive on API v1: 1.0.0 apps, CLIs, clients and the 1.0.0 website keep working against the
1.1.0 service. The service must go first: a 1.0.0 service drops the 1.1 frames and has no
`POST /api/v1/device/enrollments` or hardware salt.

0. Reconcile the 1.1 drafts of `TECHNICAL.md`, `api.yaml` and `cli.yaml` with the accepted design
   and final implementation; the Carbon's `UNDERSTANDING.md` changes are already committed.
1. **Ting types, by app and context.** The app-owning Team's manager registers the four types
   above (`tos` in production); repeat in each active test context. Verify delivery to the actual
   Teams Extend serves. Do not attempt the unavailable OBO `types.register` endpoint.
2. **Release gates**, with `e2e/real-iam/realiam.py --ting` against real IAM and Ting:
   - a Silicon reading its Carbon's directory entry gets 200, and 404 after the Carbon is removed.
     If not, ship with `EXTEND_OWNER_CHECK_AT_USE=false` and record why;
   - Ting accepts a Ting whose recipient is its own sender (a Carbon notifying themselves). If not,
     record it: those Tings then fail visibly and requests stay on the website and in the CLI.
3. **Service 1.1.0.** Build the CI image and deploy it (`deploy/aws/README.md`); the migration runs
   at start. Check that production and every test schema reached schema version 5 (including the
   shared banner setting migration). Smoke-test with the 1.0.0 CLI,
   website, Android app and desktop agent.
4. **Website 1.1.0**, right after. The 1.0 website keeps working, but it can't show another side's
   "in use", per-Team grants, waking, Ting type status or several Carbons.
5. `silicon-extend-protocol` and `silicon-extend-client` 1.1.0 to crates.io, then the `extend` CLI
   1.1.0 as a Honeycomb release (`honeycomb.yaml` version 1.1.0).
6. Android 1.1.0 (`versionCode` 4) and the desktop app 1.1.0 (Mac, Windows, Linux, with the carried
   device drivers), in either order.

Release notes to carry: devices a Carbon made Team-visible stop being visible to Team colleagues;
the same TV added through two different computers reads not ready on the later one (share it
through one computer); a Carbon's logout now ends their Silicons' sessions; on a computer several
Carbons paired, only the installer's Silicons get the terminal.

### Rolling back to 1.0.0

Rolling back needs a down step. With the service stopped, before the 1.0.0 image starts, run
`psql "$EXTEND_DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/rollback/1.1-to-1.0.sql` once. It changes
every world schema (`extend` and each `extend_test_*`) and the global schema in one transaction, and
running it twice does no harm. It:

- sets aside the grants 1.0.0 would read wrongly (a grant in another Team than the pair's recorded
  Team), and ends the sessions using them;
- marks requests routed to a Carbon that are still pending as failed, so 1.0.0 doesn't retry them;
- withdraws open wake requests, and clears credentials rotated but not yet confirmed (apps fall back
  to the one they had);
- deletes waiting "Pair with another Carbon" codes, which 1.0.0 would pair as new devices.

While rolled back, triggers the 1.1 migration added keep 1.0.0 safe: devices stay personal, new
grants and requests get their Team and holder columns, and one Silicon uses a physical device at a
time. Extra pairs show to 1.0.0 as separate devices of their Carbons, and 1.1 apps' new frames are
logged as unreadable. Rolling forward, 1.1.0 restores the set-aside grants that weren't revoked (and
whose pair didn't end) meanwhile, and logs each one.
