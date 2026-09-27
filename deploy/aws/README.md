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

The outage lasts a few seconds, and devices reconnect by themselves. Pass `PinnedImageId` and `InstanceType` on every stack update.

## Rollback

- **Image:** put the previous digest in the SSM parameter and run `extend-release`. Migrations only go forward, so restore the pre-release snapshot if the old image can't run against the new schema.
- **Secret:** move `AWSCURRENT` back to `AWSPREVIOUS`, then run `extend-release`.
