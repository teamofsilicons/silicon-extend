# Extend 4: decisions (service stage)

Decisions taken while moving the Extend service from Silicon IAM and Honeycomb to Silicon Accounts
and Silicon Apps, without a Carbon to ask. Each says what was decided and why. The product brief
(`apps/extend.md` in the migration plan) and the cross-app matrix came first; these refine them
where the code needed an answer. Contract changes are proposed, not applied:
`docs/migration/understanding-proposal.md` and `docs/migration/contracts/` (`api.yaml`,
`TECHNICAL.md`).

## API shape

1. **Account routes move to API v2; API v1 keeps only the device wire.** `/api/v1/device…`,
   `/api/v1/enrollments…`, the WebSocket frames, `/api/v1/contracts`, `/live`, `/ready` and
   `/api/version` stay byte-identical, because installed Android and desktop apps speak them and
   can't be updated with the service. Every other `/api/v1` route answers `410
   api_version_sunset`, hint exactly `silicon-apps update extend`, `details {retired,
   use_api_version: 2}`. `SERVED = [1, 2]`; v1 is never deprecated (apps keep it alive, and the
   7-day sunset rule would never fire while they run). Compat ranges: client/CLI `<4.0.0` on v1,
   `>=4.0.0, <5.0.0` on v2.
2. **v2 keeps v1's shapes and adds uuids.** Field names stay (`silicon_id`, `granted_by`, `from`,
   `to`, `owner.id` …) holding the current public id, and every person gets its permanent uuid next
   to it (`silicon_uuid`, `granted_by_uuid`, `from_uuid`, `to_uuid`, `created_by_uuid`,
   `shared_with_uuid`, `member_uuid`, `Member.uuid`). `team` is never sent on v2. This keeps the
   client and website diff small and lets callers show ids while keying on uuids.
3. **Error codes are unchanged** (no new codes for the client to learn). Retired routes use
   `api_version_sunset` with HTTP 410; a request with the old test header uses
   `testing_secret_invalid`.
4. **`/api/v2/accounts`** (no sign-in) replaces `/api/v1/iam` for discovery: app id, Accounts URL,
   links, and whether Ting delivery is on. **`/api/v2/accounts/lookup?id=`** resolves an id for the
   website's access picker and the CLI's confirmations. **`/api/v2/silicons…`** are the custodian
   views (replacing `/api/v1/team/silicons`).
5. **CORS is restricted** to `EXTEND_CORS_ORIGINS` (default none): the new website is a BFF that
   adds the bearer token server-side, so pages never call the API directly with credentials.

## Signing in

6. **One extractor, local verification.** `Authorization: Bearer <access token>` is verified with the
   cached JWKS (refetch on an unknown `kid` at most every 10 s, refresh hourly), `aud = extend`,
   `iss = ACCOUNTS_URL`. The principal (the stage's `Actor`) is `{uuid, kind, id, scope, family,
   issued_at}` from the claims; its `Debug` never prints the token. `GET /api/v2/me` names the
   account kind `type`, like every account object in Extend's API (`Member`), not `kind`.
7. **Introspection only where a sign-out must bite at once** (cached ≤ 30 s, dropped by any webhook
   about the account): pairing, granting/removing access, removing a device, starting a session,
   commands, takeovers, requests, wake requests, file content, Ting "Turn on". Reads don't introspect.
8. **Revocation by time.** `membership.signed_out` / `access_removed` / `account.deleted` set the
   account's `revoked_before` to the event's `occurred_at`; tokens with an earlier `iat` are refused
   (`401 token_expired`). `membership.signed_out` with reason `app_revoked` is Extend's own revoke
   of one sign-in (a logout): it never refuses the account's other sign-ins, and it ends what the
   account runs exactly as `POST /api/v2/auth/logout` does (decision 10), limited to sessions
   started before the logout. Normally the logout route already did it and the event changes
   nothing; when the CLI or website could only revoke at Silicon Accounts directly (Extend
   unreachable), the event is what keeps "a Carbon's logout ends their Silicons' sessions" true.
   (Revised by the second service pass: the first said `app_revoked` changes nothing, which left
   that fallback path open.)
9. **No inbound proofs.** No other app calls Extend on an account's behalf, so no route accepts
   `Authorization: Proof`.
10. **Logout** (`POST /api/v2/auth/logout`): revokes the refresh token sent (or the access token's
    sign-in) with the app credentials, then ends the caller's side: a Silicon's sessions
    (`silicon_logged_out`), a Carbon's Silicons' sessions through the Carbon's own pairs
    (`access_removed`), as the 1.1 contract decided. If Accounts can't be reached, nothing ends.
11. **A signed-out Silicon without a webhook** can't run commands once the cached introspection is
    gone; its session ends when the event arrives or at its idle timeout. The 3.x "end sessions
    15 s after a refused login" heuristic was IAM-specific and is gone.
12. **Ids from tokens never undo an event.** `accounts.id_set_at` records when an event or lookup set
    the id; an older token's `id` claim doesn't overwrite it (a token lives 30 minutes).

## No Teams

13. **Devices are private to the Carbon who paired them**; `visibility` other than `personal` is
    `422`. No org bindings, no discovery, no import routes. Device lists: `mine` (Carbon) and
    `accessible` (Silicon); any other scope is `422`.
14. **One grant per pair and Silicon**, to any active Silicon by current `si:` id or uuid (resolved
    through Accounts). The Silicon doesn't accept it; it is visible to the Silicon and its custodian.
    A Silicon whose account isn't `active` (custodian not accepted yet) can't be granted.
15. **The custodian circle** replaces the Team: two accounts are in one circle when they are the same
    account, a Silicon and its custodian, or two Silicons with the same custodian. A custodian sees
    and stops what its Silicons do (sessions `?silicon=`, grants and renounce, files and keep,
    requests `?silicon=`, wake request withdrawal, ending a session as `stopped_by_carbon`), and
    never acts as them (no session start, no grants, no device changes). An owner who is also the
    custodian may end the session as its custodian.
16. **Request routing**: to the holder Silicon only when it runs through the same pair and is in the
    asker's circle; otherwise to the Carbon who gave the holder access (hidden to the asker). Sides
    are `HMAC(salt, owner_uuid)`; the one-Silicon-per-device lock is unchanged.
17. **"It's awake" ends every open wake request on the physical device** (as in 1.1); 3.x limited it
    to the selected organization, which no longer exists. "Declined" stays per pair.
18. **Removing a device unpairs the Carbon's pair** (sessions end `device_removed`, the app gets
    `unpaired`): without org bindings, removal and unpairing are the same thing. Removing a computer
    also unpairs a device attached through it while the removal waited for the computer's lock, and
    an attachment that gets the lock after the removal is refused (`device_not_found`). 3.x had this
    guarantee for organization removal only; with removal now always an unpair, the unpair keeps it.
19. **Custodian change** (`silicon.custodian_changed`) ends the grants the previous custodian gave
    (sessions end `access_removed`); other Carbons' grants stay, and their logs say so.
20. **Account deletion**: a Carbon's pairs are unpaired; a Silicon's grants end and its wake requests
    are withdrawn; the account's id, name and photo are wiped from the cache so history reads
    "deleted account <uuid>"; telemetry stops naming it and its Ting enrolment record goes. Others'
    rows stay. Bug reports it sent stay as support records (already emailed), under the uuid only.
21. **Wake settings**: muted per pair or per Silicon on the pair; a `team` field is `422`.

## Test environments

22. **Gone.** The Honeycomb lifecycle routes are removed (404), `World::test` and per-environment
    schemas are gone, and a request that still sends `X-Testing-Application-Secret` is refused
    (`401 testing_secret_invalid`) instead of silently running in production. Existing
    `extend_test_*` schemas and `extend_global.test_environments` stay untouched for a manual
    cleanup. Development uses the local Accounts stand-in (`EXTEND_ACCOUNTS_MODE=local`, refused in
    production) or the local Accounts stack.

## Data

23. **Additive schema version 9.** The accounts cache, webhook dedupe (`accounts_events`), sealed
    proofs (`proof_grants`), per-account Ting tables, `identity_links` and `identity_link_runs`, a
    `*_iam_id` shadow column beside every identity column, and nullable Team columns
    (`devices.team` stays a string, `''` for new pairs, because installed apps read it). Historical
    migrations are byte-identical to origin/main's (checked), so schema 8 is origin/main's schema.
24. **Merging per-Team rows**: duplicate grants of one Silicon on one pair keep the earliest (with
    the latest use and muted if any copy was) and move the others to `device_access_archive`;
    duplicate open wake requests keep the latest ask and withdraw the rest (`left_team`, the closest
    existing end reason). A pair's org-level "wake requests off" moves to the pair. Nothing is
    deleted.
25. **Identity re-key in place, with originals kept.** Identity columns hold uuids for new rows; old
    rows keep IAM public ids (which contain `:` and so match no token: inert). `extend-service
    identity apply --file mapping.csv [--dry-run]` (alias `link-identities`) re-keys them in one
    transaction from the shadow originals, so a corrected mapping re-derives everything and the
    re-key is reversible until cutover; `identity suggest` drafts the mapping from Accounts
    lookups for review. The mapping is keyed by IAM public id (Extend never stored IAM principal
    ids); `iam_principal_id` is accepted and recorded when given.
26. **A mapping that would merge two pairs of one device or two grants on one pair is refused
    whole**, with a report; open wake requests that would collide are withdrawn (`left_team`).
    Pending requests and wake Tings addressed through IAM are marked failed with why.
27. **Rollback** from schema 9 is restoring the pre-migration snapshot (a 3.x binary can't run on it:
    grants lost their Team key). The 1.1→1.0 rollback rehearsals stay, pinned to schema 8.

## Cross-app

28. **Briefcase** through its delegated routes with a User verification proof for the Silicon
    (subject token: its live access token), scopes exactly `briefcase.uploads.{reserve, commit,
    status, cancel}`, `briefcase.files.read`, `briefcase.invitations.create`,
    `briefcase.entries.trash`; one proof per (account, app, scope set), refresh token sealed with
    `EXTEND_DELEGATION_ENCRYPTION_KEY`, refreshed single-flight with `Idempotency-Key =
    refresh-<sha256(refresh token)[..32]>`. Readers (the Carbon, the custodian) read with their own
    proof scoped `briefcase.files.read`. Shares go to the Carbon by uuid (Briefcase's invitations
    take a uuid or a current id; a uuid can't have moved to someone else) with read and update.
    The byte transfer follows Briefcase's 4.0 contract (`PUT /api/v1/obo/uploads/{id}/content`):
    the upload capability plus `X-Org-ID` = the Silicon's uuid (the drive the reservation belongs
    to), and no proof. File links are Briefcase's permanent form
    `https://briefcase.teamofsilicons.com/org/{silicon uuid}/apps/extend/{name}`. The vendored
    `briefcase-client` (3.x, IAM OBO) is replaced by a small HTTP adapter. (The second service pass
    aligned these three with Briefcase's migrated `openapi.yaml`; the first sent the Carbon's id,
    no `X-Org-ID`, and a `/{si:id}/apps/extend/…` link.)
29. **Self-destruct** trashes with the proof Extend holds for the creating Silicon; without one it
    waits for the Silicon's next use (as before with logins).
30. **Ting (D4)**: off unless `EXTEND_TING_URL` is set (cutover state). While off, a request is
    recorded `failed` with the plain reason and is never retried; wake requests keep working on the
    website, CLI and device; `GET /api/v2/ting-registration` and `/api/v2/accounts` say so. When on:
    recipients are enrolled with their own User verification proof (`tings.subscribe`), Tings are
    sent as Extend with an App verification proof (`tings.send`), addressed by uuid with the
    current id. `EXTEND_TING_MODE=local` is a development stand-in.

## Configuration

31. **New variables**: `ACCOUNTS_URL` (the `iss`), `ACCOUNTS_API_URL` (server-to-server, defaults to
    `ACCOUNTS_URL`), `EXTEND_APP_ID` (default `extend`), `EXTEND_APP_SECRET`,
    `EXTEND_ACCOUNTS_WEBHOOK_SECRET` (+ `_PREVIOUS_` during rotation), `EXTEND_CORS_ORIGINS`,
    `EXTEND_TING_URL`. Plain http only for loopback hosts (`localhost`, `*.localhost`, `127.0.0.0/8`,
    `::1`). Production refuses to start without the app secret, the webhook secret, the delegation
    key and the Postmark token, with messages that say what each is for.
32. **Obsolete variables** (`EXTEND_IAM_*`, `EXTEND_HONEYCOMB_SERVICE_TOKEN`, `EXTEND_LOCAL_MEMBERS`,
    the membership-sweep and owner-check settings, test-link window) are reported as ignored at start
    rather than refused, so an old deployment file doesn't stop the new image.

## Tests

33. Tests that only exercised removed features were deleted with them (IAM membership and OBO
    grants/requests, organization devices, test environments and their device limits, the IAM
    real-services lane): `membership.rs`, `obo_grants.rs`, `obo_requests.rs`, `org_devices.rs`,
    `test_env.rs`, `testenv_gaps.rs`, `real_services.rs`, six test-environment tests in
    `devices_gaps.rs`, one each in `in_use_indicator.rs` and `e2e.rs`. Every other baseline test was
    ported to the new harness (a local Accounts stand-in minting real EdDSA tokens) and passes, some
    renamed where Teams were in the name. New coverage: `accounts_auth`, `accounts_webhook`,
    `accounts_migration`, `accounts_stub` (the official client against an HTTP stand-in),
    `cross_app_stub` (Ting and Briefcase), `authz` (every route family), and an opt-in lane against
    the local Accounts stack (`real_accounts`). Two tests in the deleted files covered behaviour
    that still exists and were restored by the second pass: the Docker image defaulting to
    production (now a `config` unit test) and a computer's removal taking along a device attached
    while it waited for the lock (now in `carried_reconnect_removal.rs`, on API v2). The rest of
    the deleted tests (the IAM OBO adapters' own cases) have proof-era counterparts in
    `cross_app_stub`, `accounts_stub` and `core_gaps` (frozen Ting bodies, missing types,
    unenrolled recipients, one trash operation per file, bounded reads).
34. The released 3.1.1 client's fixtures were frozen as `contracts/v1/client-3.1.1/` before the live
    `v1/client` fixtures were regenerated (the protocol made `team` optional); Honeycomb's lifecycle
    fixtures moved to `contracts/retired/honeycomb/`.
