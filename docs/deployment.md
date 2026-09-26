# Deployment

Nothing here has been deployed. This is how the pieces are meant to ship.

## Service (`backend.bridge.teamofsilicons.com`)

- Image: `docker build -t silicon-bridge .` (see `Dockerfile`; `bridge-service serve`, port 8080,
  healthcheck `/ready`). Run `bridge-service migrate` once per release, or let `serve` migrate at
  start (migrations take an advisory lock, so concurrent starts are safe).
- **Run one instance.** Device sockets and in-flight commands live in the process (see
  [operations.md](operations.md)). Put it behind the shared ALB with WebSocket idle timeout ≥ 60 s
  (the service pings every 15 s) and sticky nothing.
- PostgreSQL 16. Each test environment gets its own schema (`bridge_test_<uuid>`), created on
  Honeycomb's `prepare` and dropped on `purge`.
- A persistent volume for `BRIDGE_DATA_DIR` isn't needed in production (uploads are staged there for
  seconds before going to Briefcase), but it must be writable.

Required environment in production (`BRIDGE_ENVIRONMENT=production` refuses local stand-ins and
plain-http public URLs):

| Variable | Value |
|---|---|
| `BRIDGE_DATABASE_URL` | PostgreSQL URL |
| `BRIDGE_PUBLIC_URL` | `https://backend.bridge.teamofsilicons.com` |
| `BRIDGE_IAM_APP_ID`, `BRIDGE_IAM_APP_SECRET` | Bridge's Silicon IAM application |
| `BRIDGE_IAM_WEBHOOK_SECRET`, `BRIDGE_IAM_WEBHOOK_SECRET_VERSION` | IAM webhook signing secret for `/webhook/` (and `…_PREVIOUS_…` during rotation) |
| `BRIDGE_IAM_LOGIN_URL` | IAM sign-in page (default `https://auth.iam.teamofsilicons.com/login`) |
| `BRIDGE_BRIEFCASE_URL`, `BRIDGE_BRIEFCASE_WEB_URL` | Briefcase API and web origins |
| `BRIDGE_TING_URL` | Ting API |
| `BRIDGE_HONEYCOMB_SERVICE_TOKEN` | Honeycomb's lifecycle credential |
| `BRIDGE_POSTMARK_SERVER_TOKEN` | For `bridge report` emails |
| `BRIDGE_WEBSITE_URL`, `BRIDGE_DOCS_URL` | Public links returned by `/api/v1/iam` |

In Silicon IAM, register the OBO endpoints Bridge calls: Briefcase `briefcase.files.create`,
`briefcase.invitations.create` (critical — needs Briefcase's approval), `briefcase.entries.trash`, and
Ting `tings.send`; and the scopes `self.identity.read`, `self.membership.read`.

## Website (`bridge.teamofsilicons.com`)

A static Vite build (`web/dist`), deployed like the sibling sites on Vercel with
`VITE_BRIDGE_API_URL=https://backend.bridge.teamofsilicons.com`. The service can also serve it with
`BRIDGE_WEB_DIR`.

## CLI

`.github/workflows/release.yml` builds the six targets on tag `v<version>`, and
`scripts/package-cli.py` validates and packs the Honeycomb archive (`honeycomb.yaml`). Then upload
it with `honeycomb releases upload` from a Carbon session, like the sibling apps.

## Device apps

Android: `apps/android` builds the APK (sign with the release key; host the download on the
website). Desktop: `apps/desktop` builds the macOS app bundle (sign and notarize with the Team's
Apple Developer ID before distributing), the Linux tarball and the Windows zip.
