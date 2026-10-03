# Organization-scoped devices

Extend 3.1 adds organization bindings without pairing a physical device again.
The configured pair keeps its device ID, physical instance, credential, host
attachment and socket. Each organization has its own visibility, Silicon grants,
notifications, sessions and activity. The physical device lock still prevents
simultaneous control across organizations.

## Visibility and import

New pairings and imports default to `personal` (Hidden). Only the Carbon who
configured that device can see a hidden binding. Other Carbons, Silicons and
organization administrators cannot discover it or access it by a known ID.
Existing grants never override hidden visibility. Hiding a previously shared
binding removes its Silicon grants and ends its sessions in that organization.

`team` (Organization) allows members of the selected organization to discover the
device. The owner still manages settings and grants. Silicon control additionally
requires an explicit owner grant in that organization; discovery does not grant
screen, terminal, wake, file or activity access.

`GET /api/v1/devices/importable` lists only the current Carbon's active configured
pairs that are not bound to the selected organization. Its paginated items contain
`device_id`, `name`, `os`, `model`, and `host_device_id`. It does not reveal source
organization names, grants, session state or credentials. Production and testing
worlds never share this inventory.

`POST /api/v1/devices/{device_id}/import` accepts
`{"type":"device_import","data":{"visibility":"personal"}}` and an
`Idempotency-Key`. The authenticated organization is the destination; the caller
cannot supply another owner or destination organization. Importing an attachment
also binds its configured host privately. Existing active host visibility is
preserved. The response is a normal `device` envelope with `team` set to the
current organization. Reusing an operation key with a different body is refused.

`GET /devices?scope=mine` shows the owner's bindings in the selected organization;
`scope=team` shows other owners' organization-visible devices; a Silicon uses
`scope=accessible` for devices it may control. Device details and all resource
operations use the same selected organization. Hidden devices answer as absent to
other members, including when their ID is already known.

Deleting a device from the website or CLI removes that organization's binding and
its grants, sessions and carried-device bindings. It preserves the physical
configuration and other organizations. The owner can still read its historical
record with `include_removed=true`, and can import the configuration again.
**Revoke pair in the native app** removes the physical pair from every organization.
Native pairing, accessibility, device controls and device credential storage remain
compatible with the existing protocol.

## Migration and deployment

World schema version 7 adds `device_organizations`, keyed by `(device_id, org_id)`.
It seeds **private** bindings for each device's original pairing organization and
each organization with an existing Silicon grant. This deliberately avoids
publishing any previously personal device to colleagues. Existing grants are
ineffective until the owner explicitly chooses Organization visibility. Native
credentials, instance IDs and existing data remain in place. An insert trigger
creates the initial private binding in the same transaction as a new pair.
Existing activity gets its session's organization, or its configuring organization
for physical lifecycle events. Before serving after startup, persisted sessions,
open wake requests and pending notifications on private or removed bindings are
ended or cancelled. Dormant grants and native credentials remain intact. The
scheduler also repairs an interrupted visibility change.
Testing cleanup removes bindings with the other test data.

An old service does not enforce organization bindings or hidden visibility.
Rolling back only the binary is therefore unsafe after schema 7 is enabled.
Restore matching database, runtime configuration and durable-session backups while
access is stopped or restricted, then reapply schema 7 before exposing the service.
Do not reopen an older globally visible runtime against the migrated database.
The physical agent transport needs no new credential or re-pairing step.

The paired service and website cutover completed on 2026-10-03. Both production and
the existing testing schema reached version 7; all backfilled bindings were private,
old grants had organization bindings, and physical credential/instance/salt
fingerprints and live pairing counts were unchanged. Encrypted RDS and host-volume
snapshots captured the stopped-writer database, runtime configuration and session
state before migration. Runtime source `67c7469` and the matching website passed
readiness, version, anonymous-access and canonical static-asset checks.

CLI 3.1.0 binaries were built at `b74e712`. The `cli-v3.1.0` release at `5bb6e40`
adds only packaging metadata, a regression test and deployment documentation:
protocol crate 1.1.1 is required by SDK/CLI 3.1.0 so registry installs retain the
private default. The runtime implementation is identical. All three Rust crates
and the six-platform GitHub archive were published and their public checksums
verified. Physical desktop and Android application versions remain unchanged.

IAM production login and authorization now accept a single account and
organization. Legacy multi-organization application sessions prompt a new login.
Revoking or expiring one organization login affects only that organization's
sessions and keeps the account's other organization sessions intact. Ordinary
logout still preserves durable feature OBO grants.

## Client compatibility

The physical device protocol remains compatible. Website/CLI delete now unbinds
the selected organization; use native Revoke pair to remove the configuration.
Website stop cannot end another owner's session through a physical alias.
Released cross-owner stop fixture requests are replayed unchanged and asserted
to fail without revealing or ending the holder. The current SDK's `device_stopped`
contract exercises an owner stopping their own carried device through its host.
A pairing with Silicon grants must explicitly choose `team` visibility.

## Local validation

`org_devices` exercises the real HTTP service, PostgreSQL and a scripted native
WebSocket: own-device import, private defaults, wrong owner and testing-world
refusal, separate organization credentials, discovery versus control, hiding an
active session, physical credential preservation, cross-organization session
redaction, removal, logout, and additive schema/cleanup behavior. IAM and provider
services are local stand-ins in these tests. They establish local authorization
behavior; authenticated live consent and resource acceptance remain separate.

Verified locally on 2026-10-03: the full service suite passed 189 tests. After the
final host-removal race and private-default changes, all 15 affected integration
tests passed, including four organization tests. All 37 protocol tests and strict
service all-target Clippy also passed. Before the production cutover, an additional
captured-schema 6-to-7 rehearsal passed on isolated PostgreSQL 17 using synthetic
rows, including private backfill, preserved physical credentials, idempotent
reapplication, backup restoration and private creation by the native-pair trigger.
The older ignored suite fixture is not the evidence for that operator rehearsal.

Authenticated live own-device import, known-ID privacy denials for another
member/admin/Silicon, and old-grant denial still await designated disposable
identities and devices. Credential-gated live provider tests and physical control
were not exercised. Anonymous production UI checks and unchanged pairing
fingerprints must not be described as those authenticated or physical tests.
