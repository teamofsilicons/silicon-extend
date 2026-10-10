# Feature permissions

Extend login identifies your account and organization. It does not authorize
Briefcase storage or Ting notifications. In **Settings → Feature access**, choose
the feature, review its request in IAM, and paste the single-use approval code
back into Extend. Approved entries show each provider's selected account and
organization. Return to your original action and retry it after approval.

For Briefcase, the storage, commit and sharing permissions used by one operation
must select the same provider context. For Ting notifications, select the account
and device organization already receiving the notification; approval cannot
silently reroute it to another recipient. Turn on notifications after approval.

The CLI has the same feature approval flow:

```sh
extend --team tos permission ls
extend --team tos permission request briefcase \
  briefcase.uploads.reserve,briefcase.uploads.commit,briefcase.uploads.status
extend --team tos permission complete REQUEST_ID --code-file /private/path/code
```

Request and completion commands print a UUID retry key. Supply
`--idempotency UUID` when retrying the identical command. A corrected code is a
new completion attempt and should use a new key. Keep the same account, team and
`--test` selection. Credentials are encrypted on the service and are never printed
by these commands or returned to the website. The original operation is not
executed by permission completion.

The official client exposes `permissions`, `request_permissions`, and
`complete_permissions`. All use the existing authenticated account/team/testing
context. Mutations require a UUID idempotency key. The corresponding API uses
`GET /api/v1/permissions`, `POST /api/v1/permissions`, and
`POST /api/v1/permissions/{id}/complete` with Extend's `{type,data}` envelopes.
Requests use type `permission`; lists and successful completion use `permissions`.

Separate consent survives ordinary logout and can be revoked in IAM. Pending
requests and encrypted grants belong to their original account, team and test
world. Resetting a test world clears them. Feature errors link to Settings without
turning a missing feature permission into a failed login.

This integration is local and unreleased. Deploy the matching IAM, Briefcase and
Ting contracts, endpoint catalogs and dependency graphs together. Preserve the
service's configured OBO encryption key across releases. Validate real consent,
refresh, selected contexts, revocation and test-world reset before production
cutover.
