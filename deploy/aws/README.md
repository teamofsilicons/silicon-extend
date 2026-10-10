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
| Runtime secret | `silicon-extend/production/runtime`, one JSON object of strings |
| Live image | SSM parameter `/silicon/extend/production/backend-image` |
| Logs | `/silicon-extend/production/service` |
| Database | `silicon-extend-production`, database `silicon_extend`, role `extend_app` |

On the host:
- `/usr/local/sbin/extend-render-env` writes `/etc/extend/runtime.env` from the secret and fixed settings.
- `/usr/local/sbin/extend-release [<image>@sha256:…]` migrates, restarts, and waits for `/ready`.

## The runtime secret

`extend-render-env` passes these keys to the service (every value a non-empty string on one line;
[docs/deployment.md](../../docs/deployment.md) says what each one is):

- **Required:** `EXTEND_DATABASE_URL` (with `sslmode=verify-full&sslrootcert=/etc/ssl/rds/global-bundle.pem`),
  `EXTEND_APP_SECRET` (Extend's app secret from Silicon Apps), `EXTEND_ACCOUNTS_WEBHOOK_SECRET` (the
  signing secret of Extend's Silicon Accounts webhook), `EXTEND_DELEGATION_ENCRYPTION_KEY`,
  `EXTEND_POSTMARK_SERVER_TOKEN`, `EXTEND_REPORT_RECIPIENTS`. A missing one stops the release with its name.
- **Optional:** `ACCOUNTS_URL` (default `https://accounts.teamofsilicons.com`), `ACCOUNTS_API_URL`,
  `EXTEND_APP_ID` (default `extend`), `EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET` (during a rotation),
  `EXTEND_TING_URL` (no default: notifications through Ting are off until it is set),
  `EXTEND_BRIEFCASE_URL`, `EXTEND_BRIEFCASE_WEB_URL`, `EXTEND_WEBSITE_URL`, `EXTEND_DOCS_URL`,
  `EXTEND_REPOSITORY_URL`, `EXTEND_CORS_ORIGINS`, `EXTEND_SPACE_STATION_KEY`,
  `EXTEND_DEVICE_APP_MIN_VERSION`, `EXTEND_LOG`, `EXTEND_MAX_PAIRS_PER_DEVICE`, and the API versioning
  pairs `EXTEND_DEPRECATED_API_VERSIONS`, `EXTEND_API_V{n}_CLI`, `EXTEND_API_V{n}_CLIENT_CRATE`.

Extend 3's keys (`EXTEND_IAM_*`, `EXTEND_HONEYCOMB_SERVICE_TOKEN`, the membership and test settings)
are never passed on; the renderer names any it finds, without their values, so they can be removed
once a release no longer needs them for a rollback. Any other key is ignored and named the same way.

## First deploy

1. Create the stack with `AllowMasterSecretRead=true` and `InstanceType=t4g.medium`.
2. Turn on stop protection, which CloudFormation can't set: `aws ec2 modify-instance-attribute --instance-id <InstanceId output> --disable-api-stop`. A stop/start would change the public IP.
3. Add the DNS `A` record `backend.extend` → the `PublicIp` output (TTL 300). Caddy needs it to get its certificate.
4. Put the runtime secret (above).
5. Push the image and record its digest in the SSM parameter.
6. Over SSM, run `extend-db-bootstrap`, then `extend-release`.
7. Lock the stack down:
   - Deploy again with `AllowMasterSecretRead=false PinnedImageId=<ImageId output> InstanceType=t4g.medium`.
   - Review the change set first. It must not replace `Instance`, because a new instance gets a new public IP.

## Later releases

1. Build the image from the release tag with `--platform linux/arm64` and push it (or take the
   `extend-arm64-backend-image` artifact of `.github/workflows/backend-candidate.yml`).
2. Snapshot the database: `aws rds create-db-snapshot --db-instance-identifier silicon-extend-production --db-snapshot-identifier extend-pre-<version>`.
3. If the release changes a host helper, refresh it (below) and update the runtime secret first.
4. Record the new digest in the SSM parameter.
5. Over SSM, run `/usr/local/sbin/extend-release`.

The outage lasts a few seconds, and devices reconnect by themselves. Keep the stop-old-container,
start-new-container order: the device hub is in the process, and old writers don't reserve
idempotency keys before running an operation, so overlapping writers would lose that guarantee.

The production switch to Silicon Accounts (service 4.0) has its own runbook:
[docs/migration/cutover.md](../../docs/migration/cutover.md).

### Refreshing a host helper

The host's helpers (`extend-render-env`, `extend-release`, `extend-db-bootstrap`) are written once,
by the instance's first boot, from the UserData in `standalone.yaml`. An image release doesn't
rewrite them, and a stack update can't: the instance has stop protection, so CloudFormation can't
stop it to apply new UserData (and a stop would change its public IP). So when a release changes a
helper (4.0.0 changes `extend-render-env`), replace it on the host from the release's checkout:

```sh
python3 deploy/aws/test_render_env.py                                  # the renderer, against fixtures
python3 deploy/aws/refresh-host-helper.py extend-render-env            # shows the SSM command; sends nothing
python3 deploy/aws/refresh-host-helper.py extend-render-env --send --instance <InstanceId>   # run at cutover
```

It keeps the old helper beside the new one as `extend-render-env.<UTC time>.bak` (rolling back is
copying it back) and prints the new one's SHA-256, which must equal the one the script printed
locally. Then check the names in the rendered environment without printing values:
`sudo cut -d= -f1 /etc/extend/runtime.env` after `sudo /usr/local/sbin/extend-render-env`.

Never put credentials in images, CLI archives, repository files, deployment logs or SSM command text.
The helpers hold none, which is why they can travel in an SSM command.

Pass `PinnedImageId`, `InstanceType` and `--tags Service=silicon-extend Environment=production` on
every stack update (leaving the tags out strips them from every resource), and read the change set
first: `Instance` must never show a replacement, and a change set that modifies its UserData must
not be executed.

### Interrupted operations

An interrupted operation or an uncertain final database write can leave a pending idempotency
claim. It must not expire into an automatic rerun: first reconcile its session, device, request,
report or wake record and any provider delivery. Don't delete the claim or advise a new key merely
because it is old. Backup and restore must keep the idempotency records together with the
operation data they cover.

## Rollback

- **Image:** put the previous digest in the SSM parameter and run `extend-release`. Migrations only go forward, so restore the pre-release snapshot if the old image can't run against the new schema.
- **4.0 back to 3.1:** a 3.x service can't run on schema 9. Stop the service, restore the
  pre-release snapshot, copy the backed-up `extend-render-env.<time>.bak` over the helper (the 3.x
  renderer requires the Extend 3 keys, so they must still be in the secret), put the 3.1 digest in
  the SSM parameter and run `extend-release`. [docs/migration/cutover.md](../../docs/migration/cutover.md)
  has the whole procedure.
- **1.1 back to 1.0** (schemas up to 8 only): stop the service, run `psql "$EXTEND_DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/rollback/1.1-to-1.0.sql` once, then put the 1.0.0 digest in the SSM parameter and run `extend-release`. What it does: [Rolling back to 1.0.0](../../docs/history/deployment-1.x-3.x.md#rolling-back-to-100).
- **Secret:** move `AWSCURRENT` back to `AWSPREVIOUS`, then run `extend-release`.
