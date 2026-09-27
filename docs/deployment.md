# Deployment

Nothing here has been deployed. This is how the pieces are meant to ship. For AWS,
[`deploy/aws/README.md`](../deploy/aws/README.md) has a stack (`deploy/aws/standalone.yaml`) for one
ARM64 EC2 host with Caddy in front and a private RDS PostgreSQL 17, with its first-deploy, release
and rollback steps; it has not been deployed either.

## Service (`backend.extend.teamofsilicons.com`)

- Image: `docker build -t silicon-extend .` (see `Dockerfile`; `extend-service serve`, port 8080,
  healthcheck `/ready`). The image sets `EXTEND_ENVIRONMENT=production` and carries `LICENSE`,
  `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` in `/usr/share/doc/silicon-extend/`. Run `extend-service migrate` once per release, or let `serve` migrate at
  start (migrations take an advisory lock, so concurrent starts are safe).
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
| `EXTEND_POSTMARK_SERVER_TOKEN` | For `extend report` emails |
| `EXTEND_WEBSITE_URL`, `EXTEND_DOCS_URL` | Public links returned by `/api/v1/iam` |
| `EXTEND_TRUSTED_PROXY_CIDRS` | The proxy or load balancer addresses (comma-separated CIDRs) whose `X-Forwarded-For` Extend believes, so the per-address enrollment limit counts clients, not the proxy |

Optional, for API versioning (TECHNICAL.md section 10): `EXTEND_DEPRECATED_API_VERSIONS` (a comma
list of majors to deprecate, applied at start; never the newest one this build serves),
`EXTEND_API_V{n}_CLIENT_CRATE` and `EXTEND_API_V{n}_CLI` (the compatible version ranges the matrix
reports, default `>=n.0.0, <n+1.0.0`), and `EXTEND_DEVICE_APP_MIN_VERSION` (default `1.0.0`).
`EXTEND_REPORT_RECIPIENTS` overrides where bug reports go.

In Silicon IAM, register the OBO endpoints Extend calls: Briefcase `briefcase.files.create`,
`briefcase.invitations.create` (critical — needs Briefcase's approval), `briefcase.entries.trash`
and `briefcase.files.read` (the file download route, `GET /api/v1/files/{file_id}/content`), and
Ting `tings.send` and `subscriptions.register` (called when a Silicon starts a session; a real Ting
refuses requests to unregistered recipients); and the scopes `self.identity.read`,
`self.membership.read`. Register Extend's webhook endpoint (`/webhook/`) and put its signing secret
in `EXTEND_IAM_WEBHOOK_SECRET`. `e2e/real-iam/realiam.py --briefcase --ting` seeds
exactly this catalog against local services and is the reference for it.

## Website (`extend.teamofsilicons.com`)

A static Vite build (`web/dist`), deployed like the sibling sites on Vercel with
`VITE_EXTEND_API_URL=https://backend.extend.teamofsilicons.com`. The service can also serve it with
`EXTEND_WEB_DIR`.

## CLI

`.github/workflows/release.yml` builds the six targets on tag `v<version>`, and
`scripts/package-cli.py` validates and packs the Honeycomb archive (`honeycomb.yaml`, and in each
target's root the executable and `licences/`; `honeycomb pack` drops anything outside the target
roots). `python3 -m unittest discover -s scripts -p 'test_*.py'` runs it with stand-in executables
when `honeycomb` is installed. Then upload the archive with `honeycomb releases upload` from a
Carbon session, like the sibling apps.

## Device apps

Android: `apps/android` builds the APK, package `com.teamofsilicons.extend` (sign with the release
key; host the download on the website). APKs are development-signed today.

Desktop: `apps/desktop` builds the macOS app bundle, the Linux tarball and `.deb`, and the Windows
zip. Distribute only the Mac zip without a suffix (`Silicon-Extend-<version>-macos-<arch>.zip`),
which `build-app.sh` produces only after Apple accepted the notarization and the ticket is stapled;
`-unnotarized` and `-adhoc` zips are for testing. Which Developer ID signs releases is still the
Carbon's decision, and changing it after release resets the Accessibility and Screen Recording
grants Carbons gave the app. The Windows zip has never been built on Windows.

## Licences in what ships

Every artifact carries Team of Silicons' MIT licence and third-party code
([`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md), which says where each artifact keeps them).
The CLI archive, the service image and the Mac, Linux and Windows packages ship `LICENSE`,
`THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` (every Rust crate's licence text, from
`cargo about`); the Mac and Linux packages also ship agent-device's and Node.js's licence files; the
Android app shows its notices. The website does not yet ship its fonts' and libraries' licence
texts.
