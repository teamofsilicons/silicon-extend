# Deployment

Nothing here has been deployed. This is how the pieces are meant to ship.

## Service (`backend.extend.teamofsilicons.com`)

- Image: `docker build -t silicon-extend .` (see `Dockerfile`; `extend-service serve`, port 8080,
  healthcheck `/ready`). Run `extend-service migrate` once per release, or let `serve` migrate at
  start (migrations take an advisory lock, so concurrent starts are safe).
- **Run one instance.** Device sockets and in-flight commands live in the process (see
  [operations.md](operations.md)). Put it behind the shared ALB with WebSocket idle timeout ≥ 60 s
  (the service pings every 15 s) and sticky nothing.
- PostgreSQL 16. Each test environment gets its own schema (`extend_test_<uuid>`), created on
  Honeycomb's `prepare` and dropped on `purge`.
- A persistent volume for `EXTEND_DATA_DIR` isn't needed in production (uploads are staged there for
  seconds before going to Briefcase), but it must be writable.

Required environment in production (`EXTEND_ENVIRONMENT=production` refuses local stand-ins and
plain-http public URLs):

| Variable | Value |
|---|---|
| `EXTEND_DATABASE_URL` | PostgreSQL URL |
| `EXTEND_PUBLIC_URL` | `https://backend.extend.teamofsilicons.com` |
| `EXTEND_IAM_APP_ID`, `EXTEND_IAM_APP_SECRET` | Extend's Silicon IAM application |
| `EXTEND_IAM_WEBHOOK_SECRET`, `EXTEND_IAM_WEBHOOK_SECRET_VERSION` | IAM webhook signing secret for `/webhook/` (and `…_PREVIOUS_…` during rotation) |
| `EXTEND_IAM_LOGIN_URL` | IAM sign-in page (default `https://auth.iam.teamofsilicons.com/login`) |
| `EXTEND_BRIEFCASE_URL`, `EXTEND_BRIEFCASE_WEB_URL` | Briefcase API and web origins |
| `EXTEND_TING_URL` | Ting API |
| `EXTEND_HONEYCOMB_SERVICE_TOKEN` | Honeycomb's lifecycle credential |
| `EXTEND_POSTMARK_SERVER_TOKEN` | For `extend report` emails |
| `EXTEND_WEBSITE_URL`, `EXTEND_DOCS_URL` | Public links returned by `/api/v1/iam` |

In Silicon IAM, register the OBO endpoints Extend calls: Briefcase `briefcase.files.create`,
`briefcase.invitations.create` (critical — needs Briefcase's approval), `briefcase.entries.trash`, and
Ting `tings.send`; and the scopes `self.identity.read`, `self.membership.read`.

## Website (`extend.teamofsilicons.com`)

A static Vite build (`web/dist`), deployed like the sibling sites on Vercel with
`VITE_EXTEND_API_URL=https://backend.extend.teamofsilicons.com`. The service can also serve it with
`EXTEND_WEB_DIR`.

## CLI

`.github/workflows/release.yml` builds the six targets on tag `v<version>`, and
`scripts/package-cli.py` validates and packs the Honeycomb archive (`honeycomb.yaml`). Then upload
it with `honeycomb releases upload` from a Carbon session, like the sibling apps.

## Device apps

Android: `apps/android` builds the APK (sign with the release key; host the download on the
website). Desktop: `apps/desktop` builds the macOS app bundle (sign and notarize with the Team's
Apple Developer ID before distributing), the Linux tarball and the Windows zip.
