# Extend: production cutover to Silicon Accounts and Silicon Apps

What a Carbon does to switch production Extend from Silicon IAM and Honeycomb (Extend 3.1) to
Silicon Accounts and Silicon Apps (Extend 4.0). Nothing here was done by the migration agents: they
never touch production. Every command that changes production is marked **run at cutover**; read
the whole runbook before starting, and keep it open during the window.

Production today: service 3.1.x on the EC2 stack `silicon-extend-production` (one ARM64 host,
`backend.extend.teamofsilicons.com`, RDS `silicon-extend-production`, database `silicon_extend`,
world schema 7 live, schema 8 prepared but not recorded as deployed), the website on the Vercel
project `silicon-extend-web`, the CLI as the Honeycomb app `extend` 3.1.1. The production app
`extend` exists in Silicon Apps and Silicon Accounts (created 2026-10-10); its app secret was shown
once to the Carbon who created it.

The switch is all at once for account-facing callers: the 4.0 service answers every Extend 3
account route with `410`, and Silicon IAM tokens stop working the moment it runs. Paired devices
are not affected: the device wire is byte-identical, so installed Android and desktop apps
reconnect on their own after the restart. Plan a window of about an hour; the outage devices see
is the restart (seconds) plus however long the re-key review takes with the writer stopped.

The commands assume a shell with:

```sh
export AWS_PROFILE=silicon-production AWS_REGION=us-east-1
export STACK=silicon-extend-production SECRET=silicon-extend/production/runtime
export INSTANCE=$(aws cloudformation describe-stacks --stack-name $STACK \
  --query "Stacks[0].Outputs[?OutputKey=='InstanceId'].OutputValue" --output text)
export ECR=$(aws cloudformation describe-stacks --stack-name $STACK \
  --query "Stacks[0].Outputs[?OutputKey=='RepositoryUri'].OutputValue" --output text)
export ACCOUNTS=https://accounts.teamofsilicons.com
read -rs EXTEND_APP_SECRET && export EXTEND_APP_SECRET   # Extend's app secret from Silicon Apps; never echo it
```

Commands on the host run as root through SSM (`aws ssm start-session --target $INSTANCE`, then
`sudo -i`), the way earlier releases did ([deploy/aws/README.md](../../deploy/aws/README.md)).

## 0. Order, and what must be ready outside Extend

- **Briefcase first.** Extend stores every screenshot, recording, log and replay in the Silicon's
  Briefcase with a User verification proof. Briefcase 4.0 must be live and accept proofs from
  issuer `extend` for `briefcase.uploads.reserve`, `briefcase.uploads.commit`,
  `briefcase.uploads.status`, `briefcase.uploads.cancel`, `briefcase.files.read`,
  `briefcase.invitations.create` and `briefcase.entries.trash` (its `BRIEFCASE_PROOF_ISSUERS`
  production value lists them). Until it does, every store fails (a warning on the command), and
  downloads and stored display media fail. Briefcase's latest cutover notes say the byte transfer
  no longer needs `X-Org-ID` and permanent links take the owner-id form, with the `/org/{…}/…`
  form Extend writes kept as an alias: re-check both with Briefcase's final contract before the
  window (integration stage), and fix Extend first if they differ.
- **Ting stays on Silicon IAM**, so Extend's notifications through Ting are off at cutover:
  `EXTEND_TING_URL` stays unset (the host's renderer gives it no default). Requests and wake requests
  stay visible on the website, in the CLI and on the device; `GET /api/v2/accounts`,
  `GET /api/v2/ting-registration` and `extend ting status` say notifications are off.
- **No app calls Extend**, and Extend accepts no proofs, so nothing else has to switch with it.
  The Silicon Interface doesn't call Extend.
- **The Silicon runtime** (stemcell `silicon connect`, outside this migration) keeps working
  unchanged with the 4.0 CLI: `extend login <token>` (positional) and a hidden `extend iam --json`
  are kept for one minor release. It must mint the token from Silicon Accounts
  (`silicon-accounts login --app extend -q`) and install Extend with Silicon Apps instead of
  Honeycomb; a token from the old identity service is refused before it is sent (`not_an_slt`).
- **The fleet**: every Silicon machine needs `silicon-apps` (with its updater) and
  `silicon-accounts`; the Silicon Apps installer sets up both.
- **The website** (the Next.js site from the migration's web stages) ships in the same window: the
  3.x site gets `410` on every account call.
- **Release artifacts ready before the window**: the 4.0 service image (from the release tag,
  `.github/workflows/backend-candidate.yml` or `docker build --platform linux/arm64`), the CLI
  archives (`.github/workflows/release.yml`, tag `cli-v4.0.0`, artifact
  `extend-silicon-apps-release`), the crates `silicon-extend-protocol` 2.0.0,
  `silicon-extend-client` 4.0.0, `silicon-extend-cli` 4.0.0 packaged (not yet published), and the
  website's production build.

## 1. Before the window (nothing changes for anyone)

### 1.1 The sign-in setup of `extend` at Silicon Accounts (run at cutover, any time before the window)

Read it, then send back the arrays you want whole (arrays replace) with the version you read:

```sh
curl -s -u "extend:$EXTEND_APP_SECRET" "$ACCOUNTS/v1/apps/extend" \
  | python3 -c 'import json,sys; a=json.load(sys.stdin); c=a.get("signin_config",a); print(json.dumps({"config_version":a.get("config_version"),"redirect_uris":c.get("redirect_uris"),"device_flow":c.get("device_flow"),"public_client":c.get("public_client")},indent=1))'
```

It must end up with:

- `https://extend.teamofsilicons.com/auth/callback` in `redirect_uris` (the website; keep every URI
  already there);
- `device_flow: true` (Carbons' `extend login` shows a code to approve) and `public_client: true`
  (the CLI exchanges Silicons' short-lived tokens, refreshes and revokes with `client_id=extend`
  alone; it holds no secret). Without them the CLI says `device_flow_off` / `public_client_off`.

```sh
curl -s -X PATCH -u "extend:$EXTEND_APP_SECRET" "$ACCOUNTS/v1/apps/extend/signin-config" \
  -H 'Content-Type: application/json' -H 'Idempotency-Key: extend-cutover-signin-1' \
  -d '{"expected_version": <config_version>, "device_flow": true, "public_client": true,
       "redirect_uris": [<every uri you read>, "https://extend.teamofsilicons.com/auth/callback"]}'
```

`allowed_origins` only matters for pages that frame the sign-in buttons; the website uses the
hosted pages, so it needs none. A `409 config_version_conflict` means someone changed it meanwhile:
read it again.

### 1.2 The webhook secret (run at cutover, before the window)

Make the signing secret now; save the URL only once the 4.0 service runs (2.6), because the 3.x
service has no `/webhooks/accounts` and would refuse every delivery. Saving the URL later keeps this
secret.

```sh
curl -s -X POST -u "extend:$EXTEND_APP_SECRET" "$ACCOUNTS/v1/apps/extend/webhook/generate-secret" \
  -H 'Idempotency-Key: extend-cutover-whsec-1'          # run at cutover, in a private terminal
```

It prints the `whsec_…` secret once (a retry with the same Idempotency-Key within 10 minutes
returns it again). Keep it for 1.3 and nowhere else.

### 1.3 The runtime secret (run at cutover, before the window)

Add the two new keys and keep every other one, including Extend 3's (`EXTEND_IAM_*`,
`EXTEND_HONEYCOMB_SERVICE_TOKEN`): the 4.0 renderer never passes those to the service, and a
rollback to 3.1 needs them. `EXTEND_DELEGATION_ENCRYPTION_KEY` stays (it now seals the Briefcase
proof refresh tokens), as do `EXTEND_POSTMARK_SERVER_TOKEN`, `EXTEND_REPORT_RECIPIENTS` and the
database URL. `ACCOUNTS_URL` and `EXTEND_APP_ID` default to production in the renderer. This prints
key names only:

```sh
read -rs EXTEND_ACCOUNTS_WEBHOOK_SECRET && export EXTEND_ACCOUNTS_WEBHOOK_SECRET   # the whsec_… from 1.2
umask 077; tmp=$(mktemp)
aws secretsmanager get-secret-value --secret-id $SECRET --query SecretString --output text \
  | python3 -c 'import json,os,sys; s=json.load(sys.stdin); s.update({k: os.environ[k] for k in ("EXTEND_APP_SECRET","EXTEND_ACCOUNTS_WEBHOOK_SECRET")}); json.dump(s, open(sys.argv[1], "w")); print(sorted(s))' "$tmp"
aws secretsmanager put-secret-value --secret-id $SECRET --secret-string "file://$tmp"   # run at cutover
rm -f "$tmp"
```

The 3.x service still running doesn't read the new keys, and its renderer ignores them (it names
them as unknown at its next release, which won't come).

### 1.4 Push the image (run at cutover, before the window)

```sh
docker load < extend-arm64-image.tar.gz                  # the backend-candidate artifact (check SHA256SUMS)
docker tag silicon-extend:candidate $ECR:4.0.0
aws ecr get-login-password | docker login --username AWS --password-stdin ${ECR%%/*}
docker push $ECR:4.0.0                                  # run at cutover
export IMAGE=$ECR@$(aws ecr describe-images --repository-name silicon-extend-production \
  --image-ids imageTag=4.0.0 --query 'imageDetails[0].imageDigest' --output text)
```

### 1.5 Rehearse on a copy (recommended)

Restore the latest automated snapshot into a scratch instance in the same subnet group, point a
local `extend-service` 4.0 at it over an SSM port forward, and run steps 2.4 (`migrate`,
`identity suggest`, `identity apply --dry-run`). The migration stage rehearsed the upgrade with
origin/main's own binary on a fixture of every table (progress.md, "Upgrade path"); this does it on
real data. Delete the scratch instance afterwards.

## 2. The window

### 2.1 Snapshot (run at cutover)

```sh
aws rds create-db-snapshot --db-instance-identifier silicon-extend-production \
  --db-snapshot-identifier extend-pre-4-0-0
aws rds wait db-snapshot-available --db-snapshot-identifier extend-pre-4-0-0
```

### 2.2 Refresh the host's renderer (run at cutover)

The 4.0 renderer requires the new keys and drops Extend 3's; an image release doesn't replace the
host's copy, and a stack update must not try ([deploy/aws/README.md](../../deploy/aws/README.md)).
From the release checkout:

```sh
python3 -m unittest discover -s deploy/aws -p 'test_*.py'
python3 deploy/aws/refresh-host-helper.py extend-render-env                       # what it would send
python3 deploy/aws/refresh-host-helper.py extend-render-env --send --instance $INSTANCE   # run at cutover
```

Read the command's output (`aws ssm get-command-invocation …`, printed by the script): the sha256 must
match, and the old helper is kept as `/usr/local/sbin/extend-render-env.<UTC time>.bak`. On the
host, render once and check the names (no values):

```sh
/usr/local/sbin/extend-render-env          # names the Extend 3 keys it doesn't pass on
cut -d= -f1 /etc/extend/runtime.env        # ACCOUNTS_URL, EXTEND_APP_ID, EXTEND_APP_SECRET, EXTEND_ACCOUNTS_WEBHOOK_SECRET, …; no EXTEND_IAM_*, no EXTEND_TING_URL
```

### 2.3 Stop the 3.x service (run at cutover)

On the host: `systemctl stop extend.service`. Devices show they are reconnecting; Silicons get
connection errors until 2.5.

### 2.4 Migrate and re-key, with the writer stopped (run at cutover)

On the host, with the new image:

```sh
docker pull "$IMAGE"
run() { docker run --rm --network extend --env-file /etc/extend/runtime.env \
  -v /opt/extend/rds-global-bundle.pem:/etc/ssl/rds/global-bundle.pem:ro \
  -v /root/extend-cutover:/cutover --read-only --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --cap-drop ALL --security-opt no-new-privileges:true "$IMAGE" "$@"; }
install -d -o 10001 -g 10001 -m 0700 /root/extend-cutover
run migrate                                              # schema 9, additive
run identity suggest --out /cutover/mapping.csv          # asks Silicon Accounts who has each old id today
```

Review every line of `/root/extend-cutover/mapping.csv` (columns `iam_public_id, accounts_uuid,
current_id, kind, status`). An old id can belong to someone else now: when the account that has it
isn't the one who used it in Extend 3, empty its `accounts_uuid` (the rows stay unmapped and inert)
or put the right uuid there (ask the Carbon or custodian concerned; `silicon-accounts` can look an
account up). Lines Silicon Accounts didn't know stay unmapped. Then:

```sh
run identity apply --file /cutover/mapping.csv --dry-run    # the report: rows per column, ids left unmapped
run identity apply --file /cutover/mapping.csv              # one transaction
```

A mapping that would merge two pairs of one device, or two grants on one pair, is refused whole
with a report; fix those lines and run it again. Applying again with a corrected file re-derives
every row from the originals kept in the `*_iam_id` columns, so the re-key can be redone until the
service writes again. Pending requests and wake Tings addressed through Silicon IAM are marked
failed with why; per-Team duplicate grants were archived in `device_access_archive` and duplicate
open wake requests withdrawn by the migration (nothing is deleted).

### 2.5 Start 4.0 (run at cutover)

```sh
aws ssm put-parameter --name /silicon/extend/production/backend-image --type String --overwrite --value "$IMAGE"
```

On the host: `/usr/local/sbin/extend-release` (it migrates again, which changes nothing, starts
the service and waits for it to be healthy). It refuses to start, naming the setting, if
`ACCOUNTS_URL`, the app secret, the webhook secret, the delegation key or the Postmark token is
missing. Then:

```sh
curl -s -o /dev/null -w '%{http_code}\n' https://backend.extend.teamofsilicons.com/ready      # 204
curl -s https://backend.extend.teamofsilicons.com/api/v2/accounts   # app_id extend, accounts_url https://accounts.teamofsilicons.com, ting_enabled false
curl -s -o /dev/null -w '%{http_code}\n' https://backend.extend.teamofsilicons.com/api/v1/devices   # 410 (Extend 3 callers are told to update)
```

The service logs once at start that notifications through Ting are off. Paired devices reconnect
within a minute (watch `docker logs extend-service` for `device connected`).

### 2.6 Point Silicon Accounts' webhook at Extend (run at cutover)

```sh
curl -s -X PUT -u "extend:$EXTEND_APP_SECRET" "$ACCOUNTS/v1/apps/extend/webhook" \
  -H 'Content-Type: application/json' -H 'Idempotency-Key: extend-cutover-webhook-1' \
  -d '{"url":"https://backend.extend.teamofsilicons.com/webhooks/accounts","events":null}'   # secret: null (kept from 1.2)
curl -s -X POST -u "extend:$EXTEND_APP_SECRET" "$ACCOUNTS/v1/apps/extend/webhook/test"
curl -s -u "extend:$EXTEND_APP_SECRET" "$ACCOUNTS/v1/apps/extend/webhook/deliveries" | head -c 600   # the test: delivered
```

`events: null` means every update, so custodian changes, sign-outs, access removals, id changes
and deletions all arrive. A delivery Extend refuses with 401 means the secret in the runtime secret
isn't this webhook's: fix 1.3 and re-run `extend-release`.

### 2.7 The website (run at cutover)

On the Vercel project `silicon-extend-web` (root `web/`), set the production environment the web's
`.env.example` lists, as sensitive values where they are secrets: `APP_ID=extend`, `APP_SECRET`
(Extend's app secret), `ACCOUNTS_URL=https://accounts.teamofsilicons.com`,
`APP_API_URL=https://backend.extend.teamofsilicons.com`, a new `SESSION_SECRET`
(`openssl rand -base64 48`, used nowhere else) and `PUBLIC_URL=https://extend.teamofsilicons.com`.
Remove `VITE_EXTEND_API_URL` and `VITE_IAM_LOGIN_URL`. Deploy the release commit and promote it.
Check: the home page loads, **Sign in** goes to Silicon Accounts' hosted page for Extend and comes
back signed in, the devices list loads.

### 2.8 The CLI release (run at cutover, once 2.5 to 2.7 check out)

Upload the Linux archives from the `extend-silicon-apps-release` artifact, release, check the
development release on a test machine, then promote ([deployment.md](../deployment.md#cli-silicon-apps)):

```sh
silicon-apps login --slt "$(silicon-accounts login --app silicon-apps -q)"
sha256sum --check SHA256SUMS
silicon-apps upload extend --target linux-x86_64 extend-4.0.0-linux-x86_64.tar.gz
silicon-apps upload extend --target linux-aarch64 extend-4.0.0-linux-aarch64.tar.gz
silicon-apps packages extend
silicon-apps release extend --version 4.0.0 --package <linux-x86_64 id> --package <linux-aarch64 id>
silicon-apps install 'extend>dev' --yes && extend accounts --json && extend login status --json   # on a test machine
silicon-apps promote extend <development release id> --version 4.0.0
silicon-apps readiness extend && silicon-apps publish extend   # the first time: details, access and packages must be set
```

Keep the macOS and Windows archives of the same run; upload them, in a new release, when Silicon
Apps' validation workers for those targets are live. Then publish the crates, in order, from the
tag: `cargo publish --locked -p silicon-extend-protocol`, then `-p silicon-extend-client`, then
`-p silicon-extend-cli`, waiting for each to appear on crates.io.

Silicon Apps' updater moves installed copies to 4.0.0 within about a minute. Not the copies
Honeycomb installed: see section 4.

### 2.9 Check it end to end

- A Silicon signs in: `silicon-accounts login --app extend -q | extend login --slt-stdin`, then
  `extend login status --json` says `authenticated: true` with its uuid and id, and its custodian.
- A Carbon signs in with `extend login` (approve the code at Silicon Accounts) and sees their
  devices (`extend device ls`): the devices paired before the switch are there once the re-key
  mapped their Carbon.
- On a test device: `extend session new <device> --connect`, `extend snapshot -i`,
  `extend screenshot` (stored in the Silicon's Briefcase, the link printed), `extend session end`.
- `extend silicon ls` as a custodian lists the Silicons they look after.
- Signing a test Silicon out of Extend at Silicon Accounts ends its running session (the webhook,
  `membership.signed_out`).

## 3. Rolling back

**Before 2.5 succeeded** (the 3.x data is untouched until `migrate` ran, and `migrate` only adds):
if `migrate` didn't run, restore the backed-up renderer (`cp -p
/usr/local/sbin/extend-render-env.<time>.bak /usr/local/sbin/extend-render-env`) and
`systemctl start extend.service`. If it ran, continue below: a 3.x service can't run on schema 9.

**After 4.0 ran** (writes made by 4.0 since the snapshot are lost):

1. On the host: `systemctl stop extend.service`.
2. Restore the snapshot into a new instance, with the stack's subnet group and database security
   group, and wait for it:
   `aws rds restore-db-instance-from-db-snapshot --db-instance-identifier silicon-extend-production-3x --db-snapshot-identifier extend-pre-4-0-0 --db-subnet-group-name <the stack's DatabaseSubnetGroup> --vpc-security-group-ids <the stack's DatabaseSecurityGroup> --db-instance-class db.t4g.micro --no-publicly-accessible`
   then `aws rds wait db-instance-available --db-instance-identifier silicon-extend-production-3x`.
3. In the runtime secret, change only the host of `EXTEND_DATABASE_URL` to the restored instance's
   endpoint (same role and password; the same python pattern as 1.3, printing names only).
4. Restore the 3.x renderer: `cp -p /usr/local/sbin/extend-render-env.<time>.bak
   /usr/local/sbin/extend-render-env` (it needs the Extend 3 keys, which 1.3 kept).
5. Put the 3.1 image digest back in the SSM parameter and run `/usr/local/sbin/extend-release`.
6. Delete Silicon Accounts' webhook for `extend` (`DELETE /v1/apps/extend/webhook`, Basic app
   credentials) and roll the website back to its previous Vercel deployment.
7. If the CLI was released: withdraw it (`silicon-apps withdraw extend <release id> --reason "…"`).
   Silicon Apps has no earlier `extend` release to fall back to, so Silicons that got 4.0 must
   reinstall 3.1.1 from Honeycomb; that is why 2.8 comes last.

The 4.0 database stays as it was for a later attempt (the stack's `silicon-extend-production`
instance); after the next successful cutover, drop the restored instance.

## 4. Silicons still running the Honeycomb CLI

Honeycomb's `extend` 3.1.1 stays installed on fleet machines until something replaces it, and
Silicon Apps' updater doesn't touch it. After the switch, every account command it runs answers
`410 api_version_sunset` with the hint `silicon-apps update extend`; for these machines the step is
an install. On each Silicon machine (the runtime can do it at its next connect):

```sh
silicon-apps install extend
honeycomb uninstall extend        # or remove the Honeycomb copy from PATH
command -v extend && extend accounts --json      # Silicon Apps' copy, version 4.0.0
silicon-accounts login --app extend -q | extend login --slt-stdin
```

Their Extend 3 sign-in files (`auth.json`, `contexts/`, `test/`) are never used; the first 4.0
`extend login` replaces them. Device apps on phones, TVs and computers need nothing. When no fleet
machine runs the Honeycomb copy any more, withdraw the Honeycomb app `extend` there.

## 5. After the switch

- Once 4.0 is confirmed (no rollback wanted), remove Extend 3's keys from the runtime secret
  (`EXTEND_IAM_*`, `EXTEND_HONEYCOMB_SERVICE_TOKEN`, the membership and test settings), with the
  same names-only pattern as 1.3; the renderer stops naming them.
- Remove Extend's Silicon IAM webhook registration (the 4.0 service answers `410` at `/webhook/`).
- The tables 4.0 no longer reads (`obo_*`, `device_organizations`, `membership_checks`,
  `ting_recipients`, `ting_type_status`, `iam_*`), the `extend_test_*` schemas and
  `extend_global.test_environments` stay. Drop them only with a Carbon's approval, after a backup.
- **Turning Ting on later**: when Ting accepts App verification proofs from `extend` for
  `tings.send` and User verification proofs for `tings.subscribe`, register Extend's four types in
  Ting (`extend.device.requested`, `extend.device.wake_requested`, `extend.device.woken`,
  `extend.device.wake_declined`), add `EXTEND_TING_URL=https://backend.ting.teamofsilicons.com` to the
  runtime secret and run `extend-release`. Accounts are enrolled with their own proof at their next
  pairing, grant, session or "Turn on".
- In 4.1, once the runtime runs `extend login --slt-stdin` and `extend accounts --json`, drop the
  hidden `extend iam --json`.

## 6. Prerequisites outside Extend, in one list

| What | Owner | Needed for |
|---|---|---|
| Briefcase 4.0 live, accepting proofs from `extend` for the seven scopes in section 0 | Briefcase cutover | files, downloads, display media |
| Ting accepting Silicon Accounts proofs | Ting (not in this migration) | notifications through Ting (off until then) |
| The runtime minting `silicon-accounts login --app extend -q` and installing with Silicon Apps | stemcell (not in this migration) | Silicons signing in automatically at connect |
| `silicon-apps` and `silicon-accounts` on every fleet machine | Silicon Apps installer | installing and updating the CLI |
| Silicon Apps validation workers for macOS and Windows | Silicon Apps | uploading those four archives |
