# The Rust client: `silicon-extend-client`

The client is the primary interface to Extend; the `extend` CLI is built on it and has no feature it
lacks. It is **stateless**: it holds a base URL, a connection pool, the negotiated API version and
(optionally) a test-environment secret. Where tokens live is your choice.

```toml
[dependencies]
silicon-extend-client = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Connect and sign in

```rust
use silicon_extend_client::{Client, DeviceQuery};

let client = Client::connect("https://backend.extend.teamofsilicons.com").await?; // negotiates the API version
let session = client.login(&slt).await?;          // a short-lived token from Silicon IAM
let me = client.authed(&session.access_token, Some("acme"));   // team handle, sent as X-Org-ID
```

`Client::builder(url).testing_secret(secret).connect()` selects a test environment; every call then
runs there, and a production token won't work in it.

## Use a device (as a Silicon)

```rust
use silicon_extend_client::protocol::model::CommandRequest;

let devices = me.devices(DeviceQuery::default()).await?;
let device = &devices.items[0];
let s = me.start_session(&device.device_id).await?;         // Err(code = device_in_use) if busy
let sid = s.session_id.to_string();
let snap = me.run(&sid, &CommandRequest {
    command: "snapshot".into(),
    args: vec!["-i".into()],                                 // the device engine's command-line tokens
    timeout_ms: None, self_destruct_minutes: None, permanent: false, attachments: vec![],
}).await?;
println!("{}", snap.text.unwrap_or_default());
me.end_session(&sid).await?;
```

A command that ran but failed on the device returns `Ok(result)` with `result.ok == false` and
`result.error`. Everything the service refused is an `Err(Error::Api { error, .. })` with a stable
`error.code`, a `message` that says what and why, and a `hint` saying what to do. A session that
ended while the command ran is `Err` with code `session_ended` and `details.may_have_run: true`.
`result.warnings` lists what went wrong around the command without failing it, such as a file
Briefcase refused to store (an older service leaves it out; it reads as empty).

## Files

```rust
let id = snap.files[0].file_id.to_string();                // each file also has its Briefcase link (`url`)
let whole = me.file_content(&id).await?;                   // bytes, content_type, name
let mut dl = me.file_download(&id, Some((0, Some(1023)))).await?;   // one range (or None), in chunks
while let Some(chunk) = dl.chunk().await? { /* write it */ }
```

Both read the file through Extend (`GET /api/v1/files/{file_id}/content`), which reads it from
Briefcase as the caller: the Silicon that made it, or the Carbon whose device made it.

## Local files sent with a command

`silicon_extend_client::attachments` builds a command's `attachments` the way the CLI does:
`Attachment::from_path` for one file, or `attach_local_files(command, &mut args)` to read every
argument that names a local file and replace it with `attachment:<name>`. It enforces the limits
(`MAX_ATTACHMENTS` = 8 files, `MAX_ATTACHMENT_BYTES` = 8 MiB in total) before reading anything.

## Manage devices (as a Carbon)

`pair`, `attach`, `update_device` (with the version from `device()` for optimistic concurrency),
`remove_device`, `stop_device`, `access`/`grant`/`revoke`, `activity`, `device_requests`,
`team_silicons`, `setup`, `setup_code`. `devices_including_removed` also lists the Carbon's removed
devices (with `removed_at` and `removed_reason`), whose `device()` and `activity()` stay readable.

1.1 (the crate is semver-compatible: `silicon-extend-client = "1"` picks it up):

- A Carbon's own devices, sessions, files and history answer in every Team, so
  `client.authed(&token, None)` works for them; the Team you pass is still the Team `grant` gives
  access in.
- `stop(id)` returns `StopOutcome::Session` (the session ran through your pair) or
  `StopOutcome::Other(DeviceStopped)` (another Carbon gave that Silicon access, so it isn't named).
  `stop_device` keeps its 1.0 signature and fails to decode the second answer after the stop worked.
- `revoke_in_team(id, silicon_id, team)` takes one Team's grant away; `revoke` takes every Team's.
- `team_silicons_all()` lists the Silicons of every Team your login reaches, with how each Team's
  read went.
- `retry_setup(id, step)` runs a failed setup step (or every failed step with `None`) again and
  returns `RetryResult { retrying }`; follow progress with `setup(id)`.
- Waking: `wake(device_id, reason)` and `cancel_wake(device_id, wake_id)` (Silicons),
  `wake_requests(device_id, query)` (both), `answer_wake(device_id, &WakeAnswer::woken())` or
  `WakeAnswer::declined()`, and `set_wake_settings(device_id, &WakeSettings::new(true).silicon(id))`
  (Carbons).
- Ting: `ting_registration(team)`, `ting_registrations()` (every Team) and `ting_turn_on(team)`.
- New response fields are optional and new enums read unknown values as `Other`, so a 1.1 client
  reads a 1.0 service and a 1.0 client reads a 1.1 service. `crates/silicon-extend-client/CHANGELOG.md`
  lists them.

## Tokens

Refresh with `client.refresh(refresh_token, idempotency_key)`. Refresh one at a time per token and
store the new pair atomically; reusing a spent refresh token revokes the family (Silicon IAM rule).
`silicon_extend_client::needs_refresh(&err)` tells you when an error means "refresh and retry".

## Building a device app

The same crate has the device side: `enroll`, `enrollment`, `device_self`, `revoke_pair`,
`device_stop`, `upload_artifact`, `ws_url`, and (1.1) `pair_enrollment(credential)` for "Pair with
another Carbon". The socket protocol, including one socket per pair, is
[device-protocol.md](device-protocol.md).

## Contract fixtures

Every public call is recorded as a fixture in `contracts/v1/client/` by the crate's
`contract_fixtures` test, and the service's CI replays them against a real service, so a service
change that would break this crate fails before it ships (`contracts/README.md`). The released 1.0.0
crate's fixtures are frozen in `contracts/v1/client-1.0.0/` and replayed too, so 1.0.0 callers keep
working. After an intended
change to what a call sends, rewrite them with
`EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures`.
