# Extend 3.1.1: organization visibility and Silicon access

## Behavior

New device pairings, attachments and imports are visible to the selected organization
by default. Explicit `personal` visibility hides a device from other Carbons,
including administrators, and from Silicons without a device grant. A Silicon with
an explicit grant in that same organization can still discover and use the hidden
device. Visibility alone does not grant control.

Hiding preserves grants and running sessions. Revoking a Silicon's access remains a
separate owner action. Schema 8 changes the default for new organization bindings;
it does not rewrite existing visibility or physical credentials. Importing an
attachment gives a missing host binding the same visibility chosen for the child;
an existing active host keeps its own setting.

The website starts new pairing and import forms with visibility enabled. Explicit
choices survive failed submissions and retries, and apply to all imports in an open
dialog. “Add another device” and a freshly opened import dialog start visible again.
Owners can grant Silicons access without first making a hidden device public to the
organization.

## Release checklist

- Validate the service's hidden owner/granted-Silicon access matrix, denied other
  Carbons/admins/ungranted Silicons, cross-organization boundaries, preserved active
  sessions and schema-8 default without existing-row changes.
- Run the web unit/type/build checks and browser regressions for visible defaults,
  hidden import retries, same-dialog batch choices and hidden-device grants.
- Publish protocol 1.1.2, then SDK and CLI 3.1.1, retaining exact package and native
  artifact provenance. Older 3.1.0 callers that explicitly send `personal` retain
  that choice.
- Back up the stopped writer and configuration, deploy service 3.1.1 and matching
  website, then verify health, retained login and the synthetic visibility matrix.
  Physical desktop and Android binaries do not need republishing.

This file describes the prepared change. Publication, deployment and live acceptance
must be recorded from their actual release receipts when completed.
