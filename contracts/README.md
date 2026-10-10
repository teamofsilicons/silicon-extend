# Contract fixtures

Consumer-driven contract tests (UNDERSTANDING.md "Versioning" 4, TECHNICAL.md section 10). Each
consumer of the Extend service publishes the requests it makes as fixtures here. The service's CI
replays every fixture of every API major it still serves, so a service change that would break a
published consumer fails before it ships.

```
contracts/
  v2/client/          silicon-extend-client 4.x (and so the extend CLI, which calls Extend only through it): its account calls
  v1/client/          silicon-extend-client 4.x: its device-side calls (the device wire), the version it shares with the apps
  v1/client-1.0.0/    the released 1.0.0 client's fixtures, frozen: 1.0.0 CLIs and apps built on it are still in use
  v1/client-1.1.0/    the released 1.1.0 client's fixtures, frozen before the 1.2.0 client release
  v1/client-1.2.0/    the released 1.2.0 client's fixtures, frozen before managed selection in 1.3.0
  v1/client-1.3.0/    the released 1.3.0 ordinary API fixtures, frozen before 2.0.0
  v1/client-3.1.1/    the released 3.1.1 client's fixtures, frozen before Extend 4 (Silicon Accounts)
  retired/client-1.3.0/  withdrawn optional selection contract, retained as history only
  retired/honeycomb/  Honeycomb's test-environment lifecycle instructions, retired with test environments in 4.0
  v1/device/          the Android app (android.*) and the Mac, Windows and Linux app (agent.*), every supported version
```

## Extend 4: the account routes of API v1 are retired

Extend 4 signs in with Silicon Accounts and has no Teams and no test environments. API v1 stays
for the device wire (`/api/v1/device…`, `/api/v1/enrollments…`, `/api/v1/contracts`), which every
installed app speaks unchanged, so `v1/device/` replays exactly as before. Every other `/api/v1`
route belonged to the Silicon IAM sign-in and moved to API v2 (`/api/v2/…`, an Accounts access
token as `Authorization: Bearer`). The service replays every published client fixture: those on
the device wire must still be accepted with every field the client reads; those on an account
route must get `410 api_version_sunset` with the hint `silicon-apps update extend` (and a request
that still selects a test environment gets `testing_secret_invalid`). Honeycomb's lifecycle
fixtures moved to `retired/honeycomb/` and must get 404. API v2's consumer is the 4.0 client
crate: its `contract_fixtures` test writes each call under the major its path names, so its account
calls are in `v2/client/` and its device-side calls in `v1/client/` (the 3.x client's account
fixtures left `v1/client/`; their frozen copies in `v1/client-3.1.1/` get the 410). The service
replays `v2/client/` like any served major: every fixture must be accepted with every field the
client reads.

A consumer that is still in use keeps its fixtures until its version stops being supported. So a new
release adds fixtures instead of editing the old ones:

- **Client:** before the crate's fixtures are regenerated for a new release, copy the released ones
  to `v1/client-<released version>/` (1.1.0 did this for 1.0.0). Nothing ever rewrites a frozen
  directory.
- **Device apps:** the 1.0.0 fixtures in `v1/device/` stay exactly as they are (they still send
  `agent_device_version`, which 1.0 apps send and the service still reads). A 1.1 app's fixture is a
  new file, named for a new operation (`android.device.socket.wake.json`) or with the version in its
  name (`android.device.self.1_1.json`, `agent.enrollments.create.1_1.json`), and says
  `"consumer_version": "1.1.0"`.

## Explicit retirement in 2.0.0

At the product owner's request, 2.0.0 removes Jev, `extend act`, the SDK's `ref_actions` and
`select_ref`, and `POST /api/v1/sessions/{session_id}/ref-selection`. This optional operation is
withdrawn for every client, including older 1.3 releases. Its original published fixture is kept
byte-for-byte under `retired/client-1.3.0/`; it is historical evidence, not a supported contract to
replay. Every other 1.3.0 fixture remains in the active frozen suite.

This intentional removal does not retire API v1 or change ordinary device/ref commands. Existing
1.0, 1.1, 1.2 and 1.3 ordinary API contracts remain supported and replayed. Android debugging input
is independent of selection and remains available.

## Who writes them

- **Client crate:** generated. `crates/silicon-extend-client/tests/contract_fixtures.rs` runs every
  public call against a recording stand-in for Extend and writes what it sends. It also finds which
  response fields the crate can't do without, by taking each field out of the expected response in
  turn (and setting it to null) and seeing whether the call still decodes. The test fails when the
  files here no longer match what the crate sends, and when a public call has no fixture. After an
  intended change:

  ```sh
  EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures
  ```

- **Device apps:** derived by hand from `docs/device-protocol.md`, the Android app's Kotlin models
  (`apps/android/.../protocol/Frames.kt`, `net/ExtendApi.kt`: a field without a default is
  required) and the desktop agent's requests (`crates/extend-agent/src/service.rs`, with
  `extend-protocol` types). Until the apps dump their own frames from their frame tests, add
  fixtures by hand when an app changes what it sends or reads. The 1.1.0 fixtures were written from
  the 1.1 protocol before the 1.1 apps were finished; check them against the apps' frame tests when
  those land.
- **Honeycomb (retired in 4.0):** derived from `silicon-honeycomb`'s participant client
  (`crates/server/src/participant_management.rs`), which sent exactly these twelve fields and
  checked that the receipt echoed six of them. Kept under `retired/honeycomb/` as history.

## Format

```jsonc
{
  "contract": 1,                          // fixture format version
  "consumer": "silicon-extend-client",    // who depends on this
  "consumer_version": "1.0.0",
  "api_version": 1,                       // must match the v{n} directory (v1/client-1.0.0 is 1 too); null under retired/honeycomb/
  "operation": "devices.update",
  "given": ["device"],                    // provider states the replay sets up first
  "kind": "http",                         // or "device_socket", "enrollment_socket"; default http
  "effect": "device_enrollment",          // http only, optional: what the answer must then make possible (below)
  "request": {
    "method": "PATCH",
    "path": "/api/v1/devices/{device_id}",
    "headers": {"authorization": "Bearer {carbon_token}", "if-match": "\"{device_version}\""},
    "body": {"type": "device", "data": {"name": "Studio phone"}}   // or "body_base64"
  },
  "response": {
    "status": "2xx",
    "required": ["/device_id", "/owner/id", "/items/*/device_id"],  // must be present
    "non_null": ["/device_id"],                                     // if present, not null
    "equals_request": ["/operation_id"],                            // echoes the request body
    "one_of": {"/state": ["pending", "completed", "failed"]}
  }
}
```

Paths are read inside the envelope's `data` (or the bare body). `*` walks every array element. A
missing or null parent passes: the consumer reads that parent as optional, and a separate entry
says otherwise when it isn't. A field an app reads only when it is there (a 1.1 field a 1.0 service
leaves out, such as `/instance_id`) goes in `non_null` only: it may be absent, but never null.

An HTTP fixture's optional `effect` is checked after a 2xx answer:

- `device_enrollment` (`POST /api/v1/device/enrollments`, "Pair with another Carbon"): c:bob claims
  the `pairing_code` the answer gave, and the new device must be a second pair of the same physical
  device: c:bob's device and the fixture's device report the same `instance_id` in
  `GET /api/v1/device` for their credentials, c:alice's view says `paired_by_others: true`, and
  c:bob's device has its own `device_id`.

A `device_socket` fixture pairs a device, opens `request.path` with its headers, and sends each
frame of `sends` in order. Each frame must still parse as the service's `DeviceFrame`, and its
`effect` must happen. Every frame the service sends meanwhile must carry the fields `reads` lists
for its type. An `enrollment_socket` fixture checks the `code` and `paired` frames the same way.

| `effect` | Before sending, the replay | After sending, it checks |
|---|---|---|
| `hello` | | The device shows the hello's app version, model and OS version, and its `engine_version` when the hello has one |
| `setup_progress` | | The device's setup state is the frame's |
| `result` | Starts a session and runs a command; uploads the listed file | The command answers as the frame says |
| `takeover_done` | Starts a session and a takeover | The takeover ended |
| `stop` | Starts a session (its `session_started` is read like any frame) | The session ended `stopped_by_carbon` |
| `attached` | Attaches an Apple TV through the device (fills `{attached_id}`) | The carried device's `online` is the frame's; its awake state too, when the frame has one |
| `stop_target` | Gives si:chef access to the carried device and starts a session on it | That session ended `stopped_by_carbon` |
| `awake` | | c:alice's view of the device has `awake` (and `sleep_state`) as sent. When the frame says awake, with `input_seen` not false, and a wake request is open, a `wake_request_ended` for it arrives and the request reads `woken` |
| `wake_request_shown` | si:chef asks to wake the device (`POST .../wake-requests`) and waits for its `wake_request` frame (fills `{wake_id}`) | The request's `device_notice` is `shown` or `not_shown` as sent, with the frame's `note` |
| `credential_saved` | Makes the device a computer two Carbons paired (c:bob pairs it through Pair with another Carbon, and a scripted 1.1 app keeps c:bob's pair connected), runs and ends a session of si:chef through the fixture's pair, and waits for the `credential` frame | `GET /api/v1/device` answers 200 with the credential that frame gave, and 401 with the old one |
| `setup_retry` | c:alice retries the first failed step of the device's setup (or of the carried device the frame names): `POST /api/v1/devices/{id}/setup/retry` with `{"step": <key>}` answers 202 naming that step, and the `setup_retry` frame arrives | The setup of that device (the fixture's, or the carried device the `attached` frame names) is the frame's |
| `none` | | Nothing |

## Placeholders and provider states

Always available: `{carbon_token}` (c:alice), `{other_carbon_token}` (c:bob, also in acme),
`{silicon_token}` (si:chef), `{other_silicon_token}` (si:sous), `{carbon_slt}`, `{silicon_id}`,
`{team}`, `{isi}`, `{honeycomb_token}`, and `{idempotency_key}` (fresh for each request).

| `given` | Sets up | Fills |
|---|---|---|
| `refresh_token` | a second sign-in of c:alice | `{refresh_token}` |
| `enrollment` | an unpaired Android enrollment | `{enrollment_id}`, `{enrollment_secret}`, `{pairing_code}` |
| `device` | an Android phone paired by c:alice, online, access for si:chef and si:sous, answering commands | `{device_id}`, `{device_credential}`, `{device_version}` |
| `session` | `device`, used by si:chef | `{session_id}` |
| `takeover` | `session`, paused by a takeover | |
| `file` | `session`, with a screenshot taken | `{file_id}` |
| `upload` | `device`, with an unused upload slot | `{upload_id}` |
| `host` | a Mac paired by c:alice, online | `{host_id}` |
| `attached` | `host`, carrying an Apple TV | `{attached_id}` |
| `test_environment` | a prepared test environment and its test application | `{testing_secret}` |
| `honeycomb_environment` | one environment id for a whole `sequence` | `{environment_id}`, `{org_id}`, `{testing_key}`, `{operation_id}` (fresh per fixture) |
| `shared_device` | `device`, also paired by c:bob through Pair with another Carbon: a second pair of the same phone, with c:bob's own id, access for si:chef in acme | `{shared_device_id}` (c:bob's pair) |
| `shared_computer` | `host`, also paired by c:bob the same way, both pairs connected by 1.1 apps | `{shared_host_id}` (c:bob's pair) |
| `wake_request` | `device`, reporting itself not awake (screen off), with an open wake request from si:chef | `{wake_id}` |
| `failed_setup` | an Android phone paired by c:alice (si:chef has access), connected by a 1.1.0 app that lists `setup_retry` in its hello, with its `wireless_debugging` step failed | `{device_id}`, `{device_credential}` |
| `paired` | (device sockets) the device the fixture pairs | `{device_id}`, `{device_credential}`, `{command_id}`, `{upload_id}`, `{attached_id}`, `{wake_id}` |

A fixture that needs a new state fails `every_fixture_is_well_formed` until the state is added to
`crates/extend-service/tests/contracts.rs` and this table.

## Running the replay

```sh
cargo test -p extend-service --test contracts        # needs PostgreSQL, EXTEND_TEST_ADMIN_URL
```

It replays `v{n}/client`, every frozen `v{n}/client-<version>/`, and `v{n}/device` for every major
the service still serves (not sunset), with the 4.0 rules above for retired account routes, and
`retired/honeycomb` (each must be gone).
`EXTEND_CONTRACTS_DIR` points it at another copy of `contracts/` (for example fixtures taken from a
release tag).

CI runs both sides in the `rust` job's "Consumer contracts" step, before the workspace tests:

```sh
cargo test --locked -p silicon-extend-client --test contract_fixtures   # the fixtures match what the crate sends
cargo test --locked -p extend-service --test contracts                  # a real service answers every fixture as promised
```
