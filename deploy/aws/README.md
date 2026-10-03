# Production on AWS

`standalone.yaml` runs `backend.extend.teamofsilicons.com` on one ARM64 EC2 host in us-east-1:

- Caddy terminates HTTPS and WebSockets with a Let's Encrypt certificate.
- The service container runs under systemd, exactly one instance, because the device WebSocket hub lives in the process.
- The database is a private RDS PostgreSQL 17 `db.t4g.micro`.

There is no load balancer. Administration is through SSM only; the host has no SSH.

| Resource | Name |
|---|---|
| Stack | `silicon-extend-production` |
| Image repository | ECR `silicon-extend-production` (tags are immutable; releases are pinned by digest) |
| Runtime secret | `silicon-extend/production/runtime`, one JSON object of `EXTEND_*` strings |
| Live image | SSM parameter `/silicon/extend/production/backend-image` |
| Logs | `/silicon-extend/production/service` |
| Database | `silicon-extend-production`, database `silicon_extend`, role `extend_app` |

On the host:
- `/usr/local/sbin/extend-render-env` writes `/etc/extend/runtime.env` from the secret and fixed settings.
- `/usr/local/sbin/extend-release [<image>@sha256:…]` migrates, restarts, and waits for `/ready`.

## First deploy

1. Create the stack with `AllowMasterSecretRead=true` and `InstanceType=t4g.medium`.
2. Turn on stop protection, which CloudFormation can't set: `aws ec2 modify-instance-attribute --instance-id <InstanceId output> --disable-api-stop`. A stop/start would change the public IP.
3. Add the DNS `A` record `backend.extend` → the `PublicIp` output (TTL 300). Caddy needs it to get its certificate.
4. Put the runtime secret. `extend-render-env` lists the required keys.
5. Push the image and record its digest in the SSM parameter.
6. Over SSM, run `extend-db-bootstrap`, then `extend-release`.
7. Lock the stack down:
   - Deploy again with `AllowMasterSecretRead=false PinnedImageId=<ImageId output> InstanceType=t4g.medium`.
   - Review the change set first. It must not replace `Instance`, because a new instance gets a new public IP.

## Later releases

1. Build the image from the release tag with `--platform linux/arm64` and push it.
2. Snapshot the database: `aws rds create-db-snapshot --db-instance-identifier silicon-extend-production --db-snapshot-identifier extend-pre-<version>`.
3. Record the new digest in the SSM parameter.
4. Over SSM, run `/usr/local/sbin/extend-release`.

For 1.1.0, follow the order and the two release gates in [Releasing 1.1.0](../../docs/deployment.md#releasing-110): the service goes out first.

The template's host helpers are installed at instance bootstrap. An image-only release does not
refresh `/usr/local/sbin/extend-render-env`. When a release changes supported runtime settings,
back up that helper and replace it with the corresponding script from the release's
`standalone.yaml` (retain root ownership and mode 0750). The 1.1 allowlist includes all five
documented tuning variables, including `EXTEND_OWNER_CHECK_AT_USE=false`. Run
`python3 deploy/aws/test_render_env.py` before deployment; verify the selected non-secret settings
in the rendered environment afterward. Absent overrides continue using service defaults.

For service 2.0.0, remove the retired provider settings from the runtime secret while preserving
every unrelated setting, then refresh the host renderer before running `extend-release`.
Verify that the rendered environment no longer contains retired settings without printing secret
values. Never put credentials in images, CLI archives, repository files, deployment logs, or SSM
command text. This release has no database migration and does not change device applications.

Keep the existing stop-old-container/start-new-container order for the idempotency update. Old
writers do not reserve keys before running an operation, so overlapping old and new writers would
not provide the new concurrency guarantee. Reservations use the existing table and a valid 503
error envelope; completed successes and explicit errors are both stored before they are returned.

An interrupted operation or uncertain final database write can leave a pending claim. It must
not expire into an automatic rerun: first reconcile its session/device/request/report/wake record
and any provider delivery. Do not delete the claim or advise a new key merely because it is old.
Backup/restore must preserve the idempotency records together with the affected operation data.
The 503 marker is readable by older code only under the same route namespace; the existing 1.0
session/request route keys differ from 1.1, so marker compatibility alone is not rollback replay
proof for those operations.

The outage lasts a few seconds, and devices reconnect by themselves. Pass `PinnedImageId`, `InstanceType` and `--tags Service=silicon-extend Environment=production` on every stack update (leaving the tags out strips them from every resource), and read the change set first: `Instance` must never show a replacement.

## Rollback

- **Image:** put the previous digest in the SSM parameter and run `extend-release`. Migrations only go forward, so restore the pre-release snapshot if the old image can't run against the new schema.
- **1.1.0 back to 1.0.0** needs a down step as well. Stop the service, run `psql "$EXTEND_DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/rollback/1.1-to-1.0.sql` once (it changes production and every test environment in one transaction, and running it twice does no harm), then put the 1.0.0 digest in the SSM parameter and run `extend-release`. What it does: [Rolling back to 1.0.0](../../docs/deployment.md#rolling-back-to-100).
- **Secret:** move `AWSCURRENT` back to `AWSPREVIOUS`, then run `extend-release`.

## Coordinated IAM OBO cutover

The new service requires `EXTEND_DELEGATION_ENCRYPTION_KEY` in the protected runtime secret whenever SDK IAM is configured. Use an independently generated 32-byte unpadded base64url key and preserve it across releases. Never print or commit it. World schema version 6 stores encrypted feature grants; deployment and rollback depend on the coordinated provider contract versions in [the cutover guide](../../docs/OBO_CUTOVER.md). This source change has not changed the running stack or its secrets.
