# Extend cutover to Silicon Accounts: runbook notes

Notes for the production switch, collected by each stage. Nothing here has been run against
production. Production today: service 3.1.x on the EC2 stack `silicon-extend-production` (RDS
`silicon_extend`, world schema 7, schema 8 prepared but not recorded as deployed), website on Vercel.

## From the service stage, 2026-10-10

Before deploying the 4.0 service:

1. **Silicon Accounts app `extend`** (with Extend's own app credentials; read-modify-write with
   `expected_version`, arrays replace):
   - `PATCH /v1/apps/extend/signin-config`: `redirect_uris` with
     `https://extend.teamofsilicons.com/auth/callback` (the website stage adds its local ones),
     `allowed_origins` `https://extend.teamofsilicons.com`, `device_flow: true`,
     `public_client: true` (the CLI exchanges short-lived tokens and refreshes as a public client).
   - `PUT /v1/apps/extend/webhook {"url": "https://backend.extend.teamofsilicons.com/webhooks/accounts", "events": null}`
     (every update, so custodian changes arrive). Keep the `whsec_…` it returns (a PUT keeps an
     existing secret and returns `null`; `generate-secret` makes one first).
2. **Runtime secret** (Secrets Manager `silicon-extend/production/runtime`) and the host's env
   renderer in `deploy/aws/standalone.yaml`: add `ACCOUNTS_URL=https://accounts.teamofsilicons.com`,
   `EXTEND_APP_ID=extend`, `EXTEND_APP_SECRET`, `EXTEND_ACCOUNTS_WEBHOOK_SECRET`; keep
   `EXTEND_DELEGATION_ENCRYPTION_KEY` (it now seals proof refresh tokens) and
   `EXTEND_POSTMARK_SERVER_TOKEN`; leave `EXTEND_TING_URL` unset (below). Remove `EXTEND_IAM_*` and
   `EXTEND_HONEYCOMB_SERVICE_TOKEN` (the service only warns about them). The renderer requires the
   IAM and Honeycomb keys and drops unknown ones today, and an image-only release doesn't refresh
   it: update it first (the ship stage changes it on the branch). The service refuses to start in
   production without `ACCOUNTS_URL`, the app secret, the webhook secret, the delegation key or the
   Postmark token, and says which.
3. **Snapshot RDS**, then stop the service (one instance; the device Hub is in memory).
4. **Migrate**: the 4.0 image migrates at start, or `extend-service migrate` (schema 9, additive;
   per-Team duplicate grants are archived in `device_access_archive`, duplicate open wake requests
   withdrawn; nothing is deleted). A 3.x binary can't run on schema 9: rolling back is restoring
   the snapshot.
5. **Re-key identities** (with the writer stopped): `extend-service identity suggest --out
   mapping.csv` (looks up each stored `c:`/`si:` id in Silicon Accounts with the app credentials),
   review every line (an id may belong to someone else now; an empty uuid keeps it unmapped),
   `extend-service identity apply --file mapping.csv --dry-run`, read the report, then apply
   without `--dry-run`. A mapping that would merge two pairs of one device or two grants on one
   pair is refused whole. Unmapped rows stay inert. Pending requests and wake Tings addressed
   through IAM are marked failed with why.
6. Start the service and check `/ready`, `GET /api/v2/accounts`, and a Silicon Accounts test ping
   (`POST /v1/apps/extend/webhook/test` → the delivery shows `delivered`).

Switch together (they break otherwise):

- **Briefcase** must accept User verification proofs from `extend` for
  `briefcase.uploads.reserve`, `.commit`, `.status`, `.cancel`, `briefcase.files.read`,
  `briefcase.invitations.create` and `briefcase.entries.trash` (its `BRIEFCASE_PROOF_ISSUERS`
  production value already lists them) and run its 4.0 service. Until then every screenshot,
  recording and log store fails (reported as a warning on the command), and downloads and stored
  display media fail. Extend's cutover waits for Briefcase's.
- **The extend CLI 4.x and the website** (later stages) must ship with the service: older CLIs
  and the 3.x website get `410 api_version_sunset` with `silicon-apps update extend` on every
  account route. Every Silicon signs in again (`silicon-accounts login --app extend -q | extend
  login --slt-stdin`); the CLI stage keeps `extend login <slt>` and a hidden `extend iam --json`
  for the Silicon runtime.
- **Installed device apps** (Android 1.1.2, desktop 1.1.0) need nothing: the device wire
  (`/api/v1/device…`, `/api/v1/enrollments…`, the WebSocket frames) is byte-identical. They show
  the owner's current id.

Ting: stays off at cutover (`EXTEND_TING_URL` unset), because Ting is still on IAM and can't verify
Silicon Accounts proofs yet. Requests and wake requests stay visible on the website, in the CLI and
on the device; `GET /api/v2/accounts` and `GET /api/v2/ting-registration` say notifications are
off. Turning it on later needs Ting to accept App verification proofs from `extend` for
`tings.send` and User verification proofs for `tings.subscribe`, and Extend's four types
registered in Ting.

After the switch: remove the IAM webhook registration (`/webhook/` answers 410). The `obo_*`,
`device_organizations`, `membership_checks`, `ting_recipients`/`ting_type_status`, `iam_*` tables,
the `extend_test_*` schemas and `extend_global.test_environments` are no longer read; drop them
only with a Carbon's approval, after a backup.
