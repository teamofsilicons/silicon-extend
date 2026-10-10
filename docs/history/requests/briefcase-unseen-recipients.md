# Briefcase: delegated sharing with an active member who has not signed in

Prepared for review on 2026-09-28. This has not been sent to a maintainer or posted as an issue.

## Required behavior

Extend stores a session file in the producing Silicon's private Briefcase folder, then shares
read/update access with the Carbon who granted access to that device. An active Carbon should
not need to sign into Briefcase before the first file can be shared. This is the Files requirement
in `understanding/UNDERSTANDING.md`; failed sharing must not be mistaken for a failed upload.

## Reproduction and evidence

The final real IAM/Briefcase/Ting fixture stores the first screenshot successfully, but its
delegated `briefcase.invitations.create` returns HTTP 422 `invalid_principal` for `c:alice`, an
active Carbon Briefcase has not projected yet. After that Carbon signs into Briefcase and lists
it, the same sharing operation succeeds. This is recorded in
`target/realiam-1.1-verification/release-audit-final/report.json` and reproduced by
`e2e/real-iam/realiam.py`'s Briefcase lane. The fixture uses Briefcase 2.1.0 source `2e6ffef`;
the current local Briefcase HEAD `0c2e260` has no changes to the invitation handler or repository
relative to that source.

In Briefcase's `src/api/handlers/invitations.rs`, ordinary authenticated invitations call
`extract::with_directory_recipients` when request headers are available. Delegated invitations
use the same `perform` function without those headers and do not follow that recipient lookup
path. This is a code-path observation, not a proposal to bypass recipient authorization.

Extend keeps the uploaded file, returns `shared_with: null` and an explicit `share_error`, and
does not automatically retry that grant after the Carbon later signs into Briefcase. Positive
sharing/native-TV checks pre-initialize the Carbon in Briefcase and do not prove this first-file
case.

## Needed resolution and verification

Use a supported IAM-authorized recipient lookup/initialization path for delegated invitations,
preserving caller visibility, active membership, Team and test-environment boundaries. No new
IAM endpoint, scope or undocumented OBO operation is assumed here.

Verify the first delegated file share with a never-before-seen active Carbon, then read its exact
bytes as that Carbon without a preceding Briefcase sign-in. Verify read/update succeeds and
delete remains refused for a non-owner Carbon. Unknown, removed, inaccessible and cross-world
recipients must still be refused. Keep the existing visible sharing error for genuine upstream
failures, and separately decide whether previously failed grants should be retried.

No changes to Briefcase, IAM, production services or the human-owned requirements are included
in this request.
