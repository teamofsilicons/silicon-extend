# Extend delegated-access cutover (local, unreleased)

The service now uses the official IAM 4.1.0 and Briefcase 2.1.0 workspace clients.
Their vendored source provenance is recorded in `vendor/*/SOURCE.md`. Pin reviewed
upstream revisions before publishing a package; these snapshots do not establish
that either dependency is deployed.

Account login authorizes Extend itself. It never creates an OBO grant. The
[feature permission flow](FEATURE_PERMISSIONS.md) requests separate consent and
keeps all resulting access/refresh credentials on the service. The browser and
CLI receive only the pending request ID, IAM consent URL, expiry and safe grant
metadata. There is no callback URL in this integration: the authenticated user
copies IAM's short-lived code back into the same account, team and testing plane.

## Configuration and storage

`EXTEND_DELEGATION_ENCRYPTION_KEY` is required whenever `EXTEND_IAM_MODE=sdk`.
It is a cryptographically random 32-byte key encoded as unpadded base64url. Add it
to the protected runtime secret before upgrading. Do not put its value in source,
CLI arguments, support reports or deployment logs. The AWS runtime renderer now
requires and forwards this variable into the existing mode-0600 runtime file.
No production key was generated or installed by this change.

AES-256-GCM encrypts pending requests and complete token pairs, with a fresh nonce
for every write. Authenticated associated data binds ciphertext to the world,
origin account, origin team, provider and endpoint. Debug output redacts the key
and runtime token. Pending requests retain an encrypted immutable IAM payload so
an uncertain start can retry after the normal app login rotates.

World schema version 6 adds `obo_requests` and `obo_grants`. Each testing world
uses its existing separate PostgreSQL schema. Test clean truncates both tables;
test deletion drops them with the schema. Ordinary account logout does not remove
feature grants. The receiving provider verifies current authority for each call;
revocation, account or membership changes and testing invalidation remain IAM's
checks. A failed grant refresh requests a new feature approval and never logs the
user out of Extend.

Keep this encryption key unchanged across service restarts and rollbacks. Back up
it separately from the database with the same access controls as app secrets. A
replacement key cannot decrypt existing grants. Key rotation requires a planned
reencryption migration or explicit reapproval; a silent fallback or plaintext
storage is not supported. Downgrading to the old service cannot use the new IAM
OBO protocol even though the additive tables are safe to retain.

## Exact provider endpoints

| Provider | Endpoint | Runtime use |
| --- | --- | --- |
| Briefcase | `briefcase.uploads.reserve` | Reserve one immutable upload manifest |
| Briefcase | `briefcase.uploads.commit` | Publish staged bytes after current authorization |
| Briefcase | `briefcase.uploads.status` | Reconcile an uncertain transfer or commit |
| Briefcase | `briefcase.invitations.create` | Share a stored file read/update with its device owner |
| Briefcase | `briefcase.files.read` | Read stored media, with a bounded response |
| Briefcase | `briefcase.entries.trash` | Scheduled deletion with a stable operation UUID |
| Ting | `tings.send` | Requests, wake and other immediate/background notifications |
| Ting | `subscriptions.register` | Register the recipient for Extend notifications |

`briefcase.uploads.cancel` is allowed in the permission request catalog for
explicit cleanup, but the current store flow does not call it. `types.register`
is not an OBO endpoint; Ting type setup stays in the app manager workflow.
The retired raw `briefcase.files.create` and `/api/v1/obo/files` are not used.

Briefcase uses the approved provider account and organization. Reserve, status,
commit and sharing must agree on that context. A device-issued upload UUID makes
the destination name and manifest stable; content staging carries only the narrow
upload capability. An uncertain transfer/commit is reconciled by status, without
blind byte retransmission. Read/trash still obey provider resource ACLs. Changing
the selected storage account later does not transfer an existing file's ownership.

Ting's frozen notifications name an existing device-team recipient. The service
refuses a different selected Ting organization rather than rerouting that payload.
Subscription registration additionally requires the selected account to match the
recipient. Choose the device team and recipient account at consent. Delivery retry
keeps its original JSON and idempotency identity.

## Retry and release evidence

Start and completion require non-nil UUID `Idempotency-Key` headers. The broker
commits a request identity before contacting IAM. Its stable downstream mutation
key is derived from that identity. Completion binds to the original account,
organization, world and exact approved roots, and returns no token material.
Database row locks serialize refresh across processes. A deterministic refresh
mutation survives a lost response and rollback, so retrying the old locally held
refresh token retrieves the same rotation rather than creating a new mutation.

Local verification includes the real PostgreSQL broker lifecycle with loopback
IAM responses, ciphertext/AAD tamper tests, provider request fixtures and the
consumer/provider wire-contract replay. These prove the local integration and
failure handling. They do not prove live consent, deployed endpoint catalogs,
provider storage, external notification delivery or production data migration.
Deploy IAM, Briefcase, Ting and their current endpoint definitions before enabling
this caller. Complete real approval, upload/read/trash, notification subscription,
refresh, revocation and testing-clean checks in an isolated live testing plane
before production cutover. Preserve the previous binaries and take a database
snapshot before changing runtime versions.

Local checks on 2026-10-03: `cargo test -p extend-service --lib --test obo_requests --test contracts` passed 45 unit, 12 provider-request and 15 provider/consumer contract tests; `--test obo_grants` passed its real-PostgreSQL lifecycle test. `cargo clippy -p extend-service --all-targets -- -D warnings` passed. The AWS runtime-secret renderer passed 4 tests. The historical `e2e/real-iam` harness still pins the retired provider contracts; it is explicitly marked as historical and is not part of this release evidence.

The additive version-6 schema also passed four fresh/upgrade/rollback/roll-forward rehearsals. The production-schema capture test remains intentionally ignored until a captured production schema and hash are supplied; no production data was restored or modified.

Website and CLI checks: 174 frontend unit tests and production build pass; 35 SDK/CLI unit tests, three generated consumer-fixture checks and strict SDK/CLI Clippy pass. Local browser QA used an explicitly synthetic mock service: a wrong approval code retained login, corrected approval displayed the provider account/org, and switching organizations cleared the pending request and code. This visual fixture did not call real IAM or a provider.
