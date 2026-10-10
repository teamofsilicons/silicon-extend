# Deployment, 1.1.0 to 3.1.1

How releases were made before Extend 4, kept as a record ([history index](README.md)). It describes
the Honeycomb archive, Silicon IAM and organizations as they were; the current procedures are in
[docs/deployment.md](../deployment.md). The 1.0.0 rollback below still applies to a database at
schema 8 or older.

## CLI (Honeycomb)

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

For 3.1.1, publish `silicon-extend-protocol` 1.1.2 first, then `silicon-extend-client`
and `silicon-extend-cli` 3.1.1. Their minimum protocol version must preserve the new
organization-visible default for omitted pair, attach and import visibility. Deploy
service 3.1.1 with schema 8 and the matching website. Existing stored visibility stays
unchanged, and explicitly hidden devices still allow their granted same-organization
Silicons. Follow [the 3.1.1 release checklist](releases/cli-3.1.1.md); this guidance does
not itself confirm publication or deployment. Physical desktop and Android packages
need no binary republish for this service policy change.

Historical 3.1.0 packaging: publish `silicon-extend-protocol` 1.1.1 before the client and CLI crates. Their minimum
protocol dependency is 1.1.1 so installed SDKs preserve the private `Visibility::default()` and
include the organization import request type. The protocol crate has an independent package
version: the physical device HTTP API stays v1, desktop/workspace packages stay 1.1.0, and Android
stays 1.1.2. This packaging correction changes no runtime source or version constants; verified
3.1.0 binaries already contain that earlier private default. The later default-visible policy
requires matching protocol, client and CLI packages; do not use 3.1.0 as its verification.
Verify the packages together with
`cargo package -p silicon-extend-protocol -p silicon-extend-client -p silicon-extend-cli`, then
publish in that order, waiting for each registry dependency to become available.

CLI/client/service 2.0.0 use independent crate versions; the CLI follows the same `cli-v` archive
lane. Publish `silicon-extend-client` before `silicon-extend-cli` and deploy service 2.0.0 as part
of the release. The ordinary HTTP API remains v1. Android stays at 1.1.2 and desktop applications
and the device protocol stay at 1.1.0. The [2.0.0 release notes](releases/cli-2.0.0.md) describe
the intentionally removed API and the ordinary snapshot/ref workflow for upgrading callers.

Refresh the existing host's environment renderer from this release and remove retired provider
settings from its runtime secret (see the [AWS runbook](../../deploy/aws/README.md)). An image update
alone does not refresh the renderer's allowlist. Preserve all unrelated secret settings and
verify only setting names or presence, never their values. No database migration or device-app
update is required for this release.

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
