# Extend against a real Silicon IAM

`realiam.py` runs the Extend service in `EXTEND_IAM_MODE=sdk` against a real, disposable Silicon
IAM (its own Postgres, `iam-api` and `iam-worker`), and drives it with the `extend` CLI, the fake
device and raw HTTP. Unlike `e2e/cli-e2e.sh` and `cargo test`, which use the local IAM stand-in,
every login, refresh, revocation, directory read, removal and webhook here goes through IAM.

```sh
cargo build -p extend-service -p silicon-extend-cli --bins --examples   # the shared target/ (CARGO_TARGET_DIR is honoured)
python3 e2e/real-iam/realiam.py all          # up, check, down; exit 0 only if every check passed
python3 e2e/real-iam/realiam.py all --keep   # leave everything running to poke at it
python3 e2e/real-iam/realiam.py up | check | down | restart-extend

# Files in a real Briefcase and requests through a real Ting, both reached through IAM OBO:
python3 e2e/real-iam/realiam.py build-minio               # once; see "Briefcase and Ting lanes"
python3 e2e/real-iam/realiam.py all --briefcase --ting    # either flag works on its own too
```

`check` expects a fresh `up` (it removes Silicons and revokes logins as it goes). A run takes about
two to three minutes, four to five with both lanes. `up` copies the three binaries into `.state/bin`, so another
build of the shared target directory cannot swap them mid-run. The report is written to
`.state/report.json` and copied with `extend.log` (and the Briefcase and Ting logs) to `last-run/`
on `down`.

## Needs

- For `--briefcase`: the image `briefcase-backend:candidate` and `extend-realiam-minio:…`
  (`build-minio`, which needs `golang:1.24-bookworm` and `debian:bookworm-slim`). For `--ting`:
  network access to github.com once (the archive is checked against its SHA-256 and cached).
- Docker, with the IAM image `silicon-iam:release-433665db296d4cdb26b846e2db97ede81066bf09` (IAM 4
  public ids, `c:`/`si:`, bare app ids; override with `REALIAM_IAM_IMAGE`) and `postgres:16.15-bookworm`.
- The Extend Postgres container `silicon-extend-postgres` on 127.0.0.1:5440 (user/password `extend`).
  The run creates and drops its own database `extend_realiam`; it never touches `extend`.
- The IAM CLI 4.0.0 at `~/.silicon/bin/iam` (`REALIAM_IAM_CLI`), Python 3 with `cryptography`.
- A free port for Extend: 8497 by default (`REALIAM_EXTEND_PORT`). The shared dev Extend on :8480 is
  left alone.

## Briefcase and Ting lanes

`--briefcase` runs Extend with `EXTEND_FILES_MODE=briefcase` against a real Silicon Briefcase;
`--ting` runs it with `EXTEND_TING_MODE=ting` against the published Ting server. Both are reached
only through real IAM OBO proofs minted by Extend's `SdkIam`.

- **Briefcase** is the image `briefcase-backend:candidate` (Briefcase 2.1.0, built from
  `silicon-briefcase`'s Dockerfile; override with `REALIAM_BRIEFCASE_IMAGE`): `briefcase-migrate`,
  `briefcase-api` and `briefcase-worker`, a `briefcase` database with its two runtime roles on the
  fixture's Postgres, and MinIO with a local KMS key so Briefcase's mandatory SSE-S3 works.
  MinIO no longer publishes images or binaries (quay.io answers 401, dl.min.io 410), so
  `build-minio` builds `RELEASE.2025-07-23T15-54-02Z` (the release Briefcase's compose.yaml pins)
  from github.com/minio/minio with `golang:1.24-bookworm` into `extend-realiam-minio:…` (~2 min).
- **Ting** is the checksum-pinned Linux ARM64 server of Ting 0.1.9
  (`server-6853b4e247f434e358f4bbd05e5d23d5ae7870cd`, downloaded once into `.cache/`), run from
  `debian:bookworm-slim` with SQLite. Ting registers types only through Honeycomb, so the fixture
  seeds all four Extend types in `acme` and `globex` while the server is stopped. `untyped` has no local type rows. Ting 0.1.9 resolves type definitions globally by app, so notifications also reach that third Team. The missing-type check temporarily removes the fixture app's type rows, with Ting stopped, then restores them; it does not fake a dependency response.
- Both services accept plain-HTTP IAM only on a literal loopback address, so they run in the IAM
  container's network namespace and reach IAM at `127.0.0.1:8080`; their ports are published
  through the IAM container. Each gets a real IAM webhook endpoint through its own relay
  (`100.128.7.11`, `.12`); IAM imports an application's dependencies into a test environment only
  when each one has a webhook endpoint.
- Seeded in IAM: applications `briefcase` and `ting` (verified, their secrets and approved scopes),
  their OBO catalogs as their docs register them (`briefcase.files.create` with metadata
  `path`/`name`/`content_type`; `briefcase.invitations.create` **critical**;
  `briefcase.entries.trash`; `briefcase.files.read`; Ting's `tings.send` and
  `subscriptions.register`, critical), and Extend's `app_scope.external` plus the matching
  `obo:<app>:<endpoint>` scopes, which `iam login --approve-scopes` then approves for each member.
- The identity `c:bob` (a plain member Carbon), is always seeded; the Briefcase lane pairs a
  second device as bob, because `c:alice` owns the Team and so holds every right in Briefcase anyway.

The Briefcase lane checks, as `si:chef` in a session: `extend screenshot` stores the file through
`briefcase.files.create` in `apps/extend/private/si:chef/` under a name of its own; Extend returns
Briefcase's permanent URL and it resolves to the same entry; si:chef's grant to the device owner is
exactly `read` + `update`; `c:bob`, owning the second device, can read and update but not delete
(Briefcase refuses his `DELETE`); the bytes come back from MinIO; `extend file keep` leaves the
file alone; a file whose `self_destruct_at` is backdated in Extend's database is trashed through
`briefcase.entries.trash` within one 30-second scheduler pass (404 for alice, in si:chef's bin)
while the kept one stays; `extend file get` downloads the file through Extend's delegated content endpoint. All
Briefcase checks are made through Briefcase's own API as `c:alice`, `c:bob` or `si:chef`, each
signed in to Briefcase with a real SLT for app `briefcase`.

The Ting lane verifies real IAM OBO proofs and actual recipient inboxes:

- Extend registers an unseen Silicon itself; the first holder-routed request is delivered with the exact reason.
- A request from `si:scout` in `globex` to a device used in `acme` reaches the granting Carbon in `globex`, without exposing the holder or its session.
- `extend.device.wake_requested`, `extend.device.woken` and `extend.device.wake_declined` arrive in both `acme` and `globex`; woken hides the confirming Carbon and declined names that Carbon.
- A third Team with no local type rows can receive globally registered app types. When the app types are genuinely absent, all four sends remain pending and `extend ting status` supplies every manager command. Opening settings or turning recipient notifications on does not attempt an unsupported type registration or clear missing-type state. The command uses an explicit owning-Team placeholder, never the delivery Team.
- The published Ting returns 404 for `POST /v1/types`. Its real OBO catalog has no `types.register`; the fixture deliberately does not invent one. This is reported as a dependency gap, not hidden with a mock registration.
- A Ting whose IAM proof actor is its own recipient is accepted and appears in that Silicon's inbox.
- A Silicon's Extend application token reads a Carbon directory entry (200), and reads 404 after that Carbon is removed through IAM's real API, while the Silicon's own entry stays readable.

Type scope was verified against the checksum-pinned 0.1.9 binary and its
[source](https://github.com/teamofsilicons/silicon-ting/blob/6853b4e247f434e358f4bbd05e5d23d5ae7870cd/crates/ting-server/src/store.rs#L338).
Type-management routes still require a Ting session with Honeycomb permission in the app's owning organization; app sends resolve the type independently of the delivery Team. Extend's errors and UI identify the owning-Team requirement without guessing that it matches the delivery Team. The protected contract drafts still need reconciliation before release.

`crates/extend-service/tests/real_services.rs` drives Extend's `BriefcaseFiles` and `TingNotifier`
directly through `SdkIam` against the same fixture, including
`FileStore::read` over `briefcase.files.read` and `Notifier::register_recipient`. Run it after `up`
and before `check`:

```sh
python3 e2e/real-iam/realiam.py up --briefcase --ting
EXTEND_REALIAM_STATE=e2e/real-iam/.state/state.json cargo test -p extend-service --test real_services -- --nocapture
python3 e2e/real-iam/realiam.py down
```

Remaining dependency gaps and verification limits:

- Ting 0.1.9 has no delegated `types.register` API. A type manager registers app types through Ting's existing Honeycomb-authorized route; Extend does not attempt the nonexistent call; recipient opt-in and pending-delivery retries stay available.
- **Briefcase gap (reported, not failed):** a delegated invitation to a Carbon Briefcase has not seen yet (no Briefcase sign-in, no IAM webhook naming them) is refused with `invalid_principal`.
- Briefcase's and Ting's testing planes are not exercised (they need Honeycomb pairing); the optional lanes cover the production plane in disposable local services. IAM and Extend's own testing plane is exercised separately.
- Devices in this lane are protocol fakes. This does not establish native screen behavior, memory use, physical-device compatibility or production release readiness.

## What is real and what is seeded

Seeded by SQL into the disposable IAM (as `silicon-hook/scripts/ting_e2e/fixture.py` does): Teams
`acme`, `globex` and `untyped`; Carbons `c:alice` (owner) and `c:bob` (member) in all three, each with verified email and phone
encrypted the way IAM stores them; Silicons `si:chef`/`si:sous` in acme, `si:scout` in globex, and `si:novice`/`si:apprentice` in untyped; direct IAM sessions for the IAM CLI; application `extend`
(verified, public, secret `ask_…`, approved read scopes `self.identity/profile/organizations/
membership.read`, `directory.silicons/carbons/memberships/profiles.read`); and its webhook endpoint
and signing key, encrypted with IAM's AES-GCM row binding. The endpoint is seeded because IAM's
registration API only accepts public HTTPS URLs.

Real IAM: SLTs (`iam login --app-id extend --grant-org acme --approve-scopes`), the SLT exchange,
refresh rotation and reuse detection, revocation, live authorization, directory reads, step-up
(the local provider returns the code), Silicon removal, webhook projection, signing and delivery by
`iam-worker`, test-environment creation, test-plane signup/login/team/Silicon creation, the test
application's testing context, and test-plane webhook delivery.

IAM delivers webhooks only to public addresses, in development too. The fixture's Docker network
uses `100.128.7.0/24` (outside every range IAM blocks); a relay container at `100.128.7.10`
forwards to Extend on the host via `host.docker.internal`. Nothing on that network routes out.

## What `check` verifies

Production plane: startup API-version negotiation; `extend login <slt>` for `c:alice`, `si:chef`,
`si:sous`; `login status --json`; `team ls`; `GET /api/v1/team/silicons`; pairing the fake device;
`device access grant` (IAM directory lookup, including refusing a non-member); session, `snapshot`,
`screenshot`; `POST /api/v1/auth/refresh` rotation; refresh-token reuse refused; the CLI refreshing
after `token_expired`; bare member-id login refused; forged and reused SLTs refused; revoked and
unknown tokens give `token_expired`; removing `si:sous` in IAM → signed webhook → Extend drops its
device access; a refresh family revoked directly at IAM is refused by Extend within 30 s; logout
ends the Silicon's session; a wrongly signed webhook is rejected; a correctly signed SDK-shaped
event is applied exactly once when delivered twice.

Testing plane: Extend's production credential creates an IAM test environment; Extend accepts the
Honeycomb `prepare` instruction; `c:alice` signs up with code 000000 and creates team `kitchen`
with `si:chef` and `si:sous`; `X-Testing-Application-Secret` selects the environment through IAM's
testing context (unknown and production secrets refused); member-id login for `c:alice` and
`si:chef`; test Silicons from the test directory; test tokens refused in production and production
tokens refused in the test plane; pairing, access, session and snapshot in the test world; removing
test `si:sous` → IAM's signed `{"test": …}` delivery → Extend routes it to the test world by the
environment's webhook key digest and leaves production untouched.

## Known limits

- IAM sends applications no webhook for token or session revocation (`oauth.token_revoked` is not
  projected to applications). Extend ends a Silicon's running session on its own logout, and
  refuses a login revoked elsewhere within 30 s, but a session left running by a login revoked
  elsewhere stays open until the Silicon's next request or the session's idle end.
- Test-plane team `kitchen` is used instead of `acme`: IAM's app-driven environment creation owns
  the test world's `acme` with a synthetic Carbon, so a test Carbon cannot join it.
- Not exercised here: OBO proofs to Briefcase and Ting without `--briefcase`/`--ting` (the default
  run uses `EXTEND_FILES_MODE=local` and `EXTEND_TING_MODE=local`), Carbon removal, and the
  Honeycomb coordinator itself (its instruction to Extend is sent directly).

## Native TV display through real Briefcase

`native_display.py` is an opt-in lane for a dedicated Android TV emulator that already has the debug
APK installed and accessibility enabled. Supply both its serial and its exact AVD name. Physical
serials, mismatched AVDs, non-debug APKs, and an existing real-IAM fixture are refused before pairing.
It replaces only the selected emulator's pairing, starts its own real IAM/Briefcase/MinIO services,
and removes those services on exit. The app is left unpaired at its previous debug service URL.

```sh
cargo build -p extend-service -p silicon-extend-cli --bins --examples
python3 e2e/real-iam/native_display.py \
  --serial emulator-5640 --avd ExtendReconnectVerification \
  --adb "$HOME/Library/Android/sdk/platform-tools/adb" \
  --out target/real-briefcase-native-tv
```

The lane displays a four-color card on the native TV, captures it through Extend as `si:chef`, and
verifies the actual PNG in real Briefcase/MinIO, owned by that Silicon and shared with `c:alice`.
It then displays the stored image through `file:<UUID>`, bare UUID and the private Briefcase URL.
Each replay starts from a visibly different text screen, then checks all four RGB samples from
Android's raw screenshot and saves a native PNG. This exercises the Briefcase delegated read,
service attachment and real Android decode together. A missing file and another Silicon's attempt
are refused; damaged image bytes produce native `action_failed`; the next stored image still works.

The Carbon signs into Briefcase first so its existing recipient-projection limitation does not
block sharing. This is a native emulator test, not a result for the user's physical TV. The output
folder retains the report, screenshots, exact command results, service logs, logcat and memory dump.
The lane does not send notifications to real people or change production services.
