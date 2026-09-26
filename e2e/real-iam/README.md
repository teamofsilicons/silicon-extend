# Bridge against a real Silicon IAM

`realiam.py` runs the Bridge service in `BRIDGE_IAM_MODE=sdk` against a real, disposable Silicon
IAM (its own Postgres, `iam-api` and `iam-worker`), and drives it with the `bridge` CLI, the fake
device and raw HTTP. Unlike `e2e/cli-e2e.sh` and `cargo test`, which use the local IAM stand-in,
every login, refresh, revocation, directory read, removal and webhook here goes through IAM.

```sh
CARGO_TARGET_DIR=$PWD/target/realiam cargo build -p bridge-service -p bridge-cli --bins --examples
python3 e2e/real-iam/realiam.py all          # up, check, down; exit 0 only if every check passed
python3 e2e/real-iam/realiam.py all --keep   # leave everything running to poke at it
python3 e2e/real-iam/realiam.py up | check | down | restart-bridge
```

`check` expects a fresh `up` (it removes Silicons and revokes logins as it goes). A run takes about
two minutes. The report is written to `.state/report.json` and copied with `bridge.log` to
`last-run/` on `down`.

## Needs

- Docker, with the IAM image `silicon-iam:release-433665db296d4cdb26b846e2db97ede81066bf09` (IAM 4
  public ids, `c:`/`si:`, bare app ids; override with `REALIAM_IAM_IMAGE`) and `postgres:16.15-bookworm`.
- The Bridge Postgres container `silicon-bridge-postgres` on 127.0.0.1:5440 (user/password `bridge`).
  The run creates and drops its own database `bridge_realiam`; it never touches `bridge`.
- The IAM CLI 4.0.0 at `~/.silicon/bin/iam` (`REALIAM_IAM_CLI`), Python 3 with `cryptography`.
- A free port for Bridge: 8497 by default (`REALIAM_BRIDGE_PORT`). The shared dev Bridge on :8480 is
  left alone.

## What is real and what is seeded

Seeded by SQL into the disposable IAM (as `silicon-hook/scripts/ting_e2e/fixture.py` does): team
`acme`; Carbon `c:alice` (owner, with verified email and phone encrypted the way IAM stores them);
Silicons `si:chef` and `si:sous`; direct IAM sessions for the IAM CLI; application `bridge`
(verified, public, secret `ask_…`, approved read scopes `self.identity/profile/organizations/
membership.read`, `directory.silicons/carbons/memberships/profiles.read`); and its webhook endpoint
and signing key, encrypted with IAM's AES-GCM row binding. The endpoint is seeded because IAM's
registration API only accepts public HTTPS URLs.

Real IAM: SLTs (`iam login --app-id bridge --grant-org acme --approve-scopes`), the SLT exchange,
refresh rotation and reuse detection, revocation, live authorization, directory reads, step-up
(the local provider returns the code), Silicon removal, webhook projection, signing and delivery by
`iam-worker`, test-environment creation, test-plane signup/login/team/Silicon creation, the test
application's testing context, and test-plane webhook delivery.

IAM delivers webhooks only to public addresses, in development too. The fixture's Docker network
uses `100.128.7.0/24` (outside every range IAM blocks); a relay container at `100.128.7.10`
forwards to Bridge on the host via `host.docker.internal`. Nothing on that network routes out.

## What `check` verifies

Production plane: startup API-version negotiation; `bridge login <slt>` for `c:alice`, `si:chef`,
`si:sous`; `login status --json`; `team ls`; `GET /api/v1/team/silicons`; pairing the fake device;
`device access grant` (IAM directory lookup, including refusing a non-member); session, `snapshot`,
`screenshot`; `POST /api/v1/auth/refresh` rotation; refresh-token reuse refused; the CLI refreshing
after `token_expired`; bare member-id login refused; forged and reused SLTs refused; revoked and
unknown tokens give `token_expired`; removing `si:sous` in IAM → signed webhook → Bridge drops its
device access; a refresh family revoked directly at IAM is refused by Bridge within 30 s; logout
ends the Silicon's session; a wrongly signed webhook is rejected; a correctly signed SDK-shaped
event is applied exactly once when delivered twice.

Testing plane: Bridge's production credential creates an IAM test environment; Bridge accepts the
Honeycomb `prepare` instruction; `c:alice` signs up with code 000000 and creates team `kitchen`
with `si:chef` and `si:sous`; `X-Testing-Application-Secret` selects the environment through IAM's
testing context (unknown and production secrets refused); member-id login for `c:alice` and
`si:chef`; test Silicons from the test directory; test tokens refused in production and production
tokens refused in the test plane; pairing, access, session and snapshot in the test world; removing
test `si:sous` → IAM's signed `{"test": …}` delivery → Bridge routes it to the test world by the
environment's webhook key digest and leaves production untouched.

## Known limits

- IAM sends applications no webhook for token or session revocation (`oauth.token_revoked` is not
  projected to applications). Bridge ends a Silicon's running session on its own logout, and
  refuses a login revoked elsewhere within 30 s, but a session left running by a login revoked
  elsewhere stays open until the Silicon's next request or the session's idle end.
- Test-plane team `kitchen` is used instead of `acme`: IAM's app-driven environment creation owns
  the test world's `acme` with a synthetic Carbon, so a test Carbon cannot join it.
- Not exercised here: OBO proofs to Briefcase and Ting (the run uses `BRIDGE_FILES_MODE=local` and
  `BRIDGE_TING_MODE=local`), Carbon removal (the only Carbon is the sole owner), and the Honeycomb
  coordinator itself (its instruction to Bridge is sent directly).
