# Contract fixtures

Consumer-driven contract tests (UNDERSTANDING.md "Versioning" 4, TECHNICAL.md section 10). Each
consumer of the Extend service publishes the requests it makes as fixtures here. The service's CI
replays every fixture of every API major it still serves, so a service change that would break a
published consumer fails before it ships.

```
contracts/
  v1/client/    silicon-extend-client (and so the extend CLI, which calls Extend only through it)
  v1/device/    the Android app (android.*) and the Mac, Windows and Linux app (agent.*)
  internal/honeycomb/   Honeycomb's test-environment lifecycle instructions (not API-versioned)
```

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
  `extend-protocol` types). Until the apps dump their own frames from their frame tests, update
  these by hand when an app changes what it sends or reads.
- **Honeycomb:** derived from `silicon-honeycomb`'s participant client
  (`crates/server/src/participant_management.rs`), which sends exactly these twelve fields and
  checks that the receipt echoes six of them.

## Format

```jsonc
{
  "contract": 1,                          // fixture format version
  "consumer": "silicon-extend-client",    // who depends on this
  "consumer_version": "1.0.0",
  "api_version": 1,                       // must match the v{n} directory; null under internal/
  "operation": "devices.update",
  "given": ["device"],                    // provider states the replay sets up first
  "kind": "http",                         // or "device_socket", "enrollment_socket"; default http
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
says otherwise when it isn't.

A `device_socket` fixture pairs a device, opens `request.path` with its headers, and sends each
frame of `sends` in order. Each frame must still parse as the service's `DeviceFrame`, and its
`effect` must happen: `hello` (the device shows the hello's app version, model and OS version),
`setup_progress`, `result` (answers a real command, uploading the listed file first), `takeover_done`,
`stop`, `attached`, `stop_target`, or `none`. Every frame the service sends meanwhile must carry the
fields `reads` lists for its type. An `enrollment_socket` fixture checks the `code` and `paired`
frames the same way.

## Placeholders and provider states

Always available: `{carbon_token}` (c:alice), `{silicon_token}` (si:chef), `{other_silicon_token}`
(si:sous), `{carbon_slt}`, `{silicon_id}`, `{team}`, `{isi}`, `{honeycomb_token}`, and
`{idempotency_key}` (fresh for each request).

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
| `paired` | (device sockets) the device the fixture pairs | `{device_id}`, `{device_credential}`, `{command_id}`, `{upload_id}`, `{attached_id}` |

A fixture that needs a new state fails `every_fixture_is_well_formed` until the state is added to
`crates/extend-service/tests/contracts.rs` and this table.

## Running the replay

```sh
cargo test -p extend-service --test contracts        # needs PostgreSQL, EXTEND_TEST_ADMIN_URL
```

It replays `v{n}/client` and `v{n}/device` for every major the service still serves (not sunset),
and `internal/honeycomb` in its `sequence` order.

CI runs both sides in the `rust` job's "Consumer contracts" step, before the workspace tests:

```sh
cargo test --locked -p silicon-extend-client --test contract_fixtures   # the fixtures match what the crate sends
cargo test --locked -p extend-service --test contracts                  # a real service accepts every fixture
```
