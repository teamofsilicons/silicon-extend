# Organization-scoped devices

Extend 3.1 adds organization bindings without pairing a physical device again.
The configured pair keeps its device ID, physical instance, credential, host
attachment and socket. Each organization has its own visibility, Silicon grants,
notifications, sessions and activity. The physical device lock still prevents
simultaneous control across organizations.

## Visibility and import

New pairings, attachments and imports default to `team` (Organization). Owners can
explicitly choose `personal` (Hidden) for the binding in that organization.
Hidden devices remain visible and usable to their configuring Carbon and Silicons
with an explicit owner grant in the same organization. Other Carbons (including
organization administrators) and ungranted Silicons cannot discover or access a
hidden device, even by a known ID. Hiding preserves existing grants and active
sessions; revoking a Silicon's grant remains the separate way to end its access.
Existing bindings retain their stored visibility.

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
also binds a missing or removed configured host with the same chosen visibility. Omitting
visibility uses `team`; the example explicitly chooses Hidden. Existing active host visibility is
preserved. The response is a normal `device` envelope with `team` set to the
current organization. Reusing an operation key with a different body is refused.

`GET /devices?scope=mine` shows the owner's bindings in the selected organization;
`scope=team` shows other owners' organization-visible devices; a Silicon uses
`scope=accessible` for devices it may control. Device details and all resource
operations use the same selected organization. Hidden devices answer as absent to
other Carbons and ungranted Silicons, including when their ID is already known.
An owner can grant same-organization Silicon access before or after hiding the device.

Deleting a device from the website or CLI removes that organization's binding and
its grants, sessions and carried-device bindings. It preserves the physical
configuration and other organizations. The owner can still read its historical
record with `include_removed=true`, and can import the configuration again.
**Revoke pair in the native app** removes the physical pair from every organization.
Native pairing, accessibility, device controls and device credential storage remain
compatible with the existing protocol.

## Migration and deployment

World schema version 7 added `device_organizations`, keyed by `(device_id, org_id)`.
It seeded **private** bindings for each device's original pairing organization and
each organization with an existing Silicon grant. Schema version 8 changes only
the default for newly created bindings to `team`; it does not rewrite existing
visibility. Physical credentials, instance IDs and existing data remain in place.
The insert trigger uses the new default for a new pair, while an explicit Hidden
choice is stored in the same transaction. Existing activity retains its session's
organization, or its configuring organization for physical lifecycle events.

The initial schema-7 rollout also denied granted Silicons access to hidden devices
and cancelled their sessions. The current policy supersedes that behavior: hidden
bindings honor explicit same-organization grants, and visibility changes do not
remove grants or end sessions. Removed bindings and revoked grants continue to
prevent access. Previously ended sessions are not resurrected.

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
protocol crate 1.1.1 was required by SDK/CLI 3.1.0 so those registry installs retained
the then-current private default. This is historical release evidence, not validation
of the current visible-by-default policy. That packaging-only revision retained the
same runtime implementation. All three Rust crates
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
Pairing and granting control are independent of discovery visibility. Explicitly
granted same-organization Silicons can use either visibility.

## Local validation

`org_devices` exercises the real HTTP service, PostgreSQL and a scripted native
WebSocket: own-device import, visibility defaults and explicit choices, wrong owner and testing-world
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

Historical acceptance of the initial private-default policy completed on 2026-10-03 UTC against the deployed
API and published CLI 3.1.0: all 64 checks passed using real IAM identities in the
designated disposable testing world. The matrix covered two organizations, owner
import and idempotent retry, organization-specific My devices, organization-wide
discovery without control, Hidden defaults, and known-ID and old-grant denial for
another member, an administrator and a Silicon. Import preserved the physical
device identity and native credential. CLI checks also covered retained contexts,
organization switching, unbinding and private reimport.

Cleanup revoked the disposable pair and verified that its native credential was
rejected. No production device was changed. These checks establish organization
bindings and device authorization using synthetic devices; physical device
control and reboot behavior were not exercised.

The historical results above do not establish the updated default or hidden Silicon
access policy. The current change requires focused service and web regression
validation before release; existing organization bindings must remain unchanged.
