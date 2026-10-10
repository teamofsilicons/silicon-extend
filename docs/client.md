# The Rust client: `silicon-extend-client`

The client is the primary interface to Extend; the `extend` CLI is built on it and has no feature it
lacks. It is **stateless**: a `Client` holds a base URL, a connection pool and the negotiated API
version. Where tokens live is your choice.

```toml
[dependencies]
silicon-extend-client = "4"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Everyone signs in with [Silicon Accounts](https://accounts.teamofsilicons.com) with a personal
account, and a device belongs to the Carbon who paired it. Account calls use API v2
(`/api/v2/…`); the device wire the installed Extend apps speak stays API v1, and the client pins
each request to the major its path names.

## Sign in

`auth::SignIn` signs in the way Extend's own tools do, as a public client (`client_id` = `extend`,
no secret).

```rust
use silicon_extend_client::auth::SignIn;

let sign_in = SignIn::new("https://accounts.teamofsilicons.com")?;

// A Silicon hands over a short-lived token from `silicon-accounts login --app extend -q`.
let tokens = sign_in.exchange_slt(&slt).await?;

// A Carbon approves a code on the Silicon Accounts site.
let code = sign_in.start_device(Some("my tool on build-box")).await?;
println!("Open {} and enter {}", code.verification_uri, code.user_code);
let tokens = sign_in.wait_for_device(&code, |progress| eprintln!("{progress:?}")).await?;

let account = tokens.account.as_ref().unwrap();        // uuid (key on it), id, kind, custodian
```

- `wait_for_device` polls every `interval` seconds, adds 5 after each `slow_down`, retries network
  failures, and stops when the code is approved, denied (`device_denied`) or expired after 10 minutes
  (`device_expired`).
- A refused short-lived token says exactly why in `AuthError::code`: `slt_already_used`,
  `slt_expired`, `slt_wrong_app`, `slt_unknown`, `slt_wrong_kind`, `slt_sign_in_ended`. A value that
  doesn't start with `slt_` is refused before anything is sent (`not_an_slt`). The token never
  appears in an error.
- Tokens are `Secret`s: `Debug` prints only their prefix; `expose()` returns the value.

The access token is a Silicon Accounts JWT with `aud` = `extend`, valid for 30 minutes.

## Connect

```rust
use silicon_extend_client::{Client, DeviceQuery};

let client = Client::connect("https://api.extend.teamofsilicons.com").await?; // negotiates API v2
let me = client.authed(tokens.access_token.expose());
let who = me.me().await?;                              // uuid, id, kind, display name, custodian
let info = client.accounts().await?;                   // no sign-in: the Silicon Accounts this Extend trusts
```

Connecting to an Extend older than 4.0 fails with `api_version_unsupported`. Service URLs must be
https, or http for this machine (`localhost`, `127.0.0.1`, `[::1]`).

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
`result.error`. Everything the service refused is an `Err(Error::Api { status, error })` with a
stable `error.code`, a `message` that says what and why, a `hint` saying what to do, the
`request_id` and `details`. A session that ended while the command ran is `Err` with code
`session_ended` and `details.may_have_run: true`. `result.warnings` lists what went wrong around the
command without failing it, such as a file Briefcase refused to store.

## Files

```rust
let id = snap.files[0].file_id.to_string();                // each file also has its Briefcase link (`url`)
let whole = me.file_content(&id).await?;                   // bytes, content_type, name
let mut dl = me.file_download(&id, Some((0, Some(1023)))).await?;   // one range (or None), in chunks
while let Some(chunk) = dl.chunk().await? { /* write it */ }
```

Both read the file through Extend (`GET /api/v2/files/{file_id}/content`), which reads it from
Briefcase as the caller: the Silicon that made it, the Carbon whose device made it, or the Silicon's
custodian.

## Local files sent with a command

`silicon_extend_client::attachments` builds a command's `attachments` the way the CLI does:
`Attachment::from_path` for one file, or `attach_local_files(command, &mut args)` to read every
argument that names a local file and replace it with `attachment:<name>`. It enforces the limits
(`MAX_ATTACHMENTS` = 8 files, `MAX_ATTACHMENT_BYTES` = 8 MiB in total) before reading anything.

## Manage devices (as a Carbon)

`pair` (with `silicon_ids` to give Silicons access at once, by `si:` id or uuid), `attach`,
`update_device` (with the version from `device()` for optimistic concurrency), `set_in_use_indicator`,
`remove_device`, `stop` (`StopOutcome::Session` when it ran through your pair,
`StopOutcome::Other(DeviceStopped)` when another Carbon gave that Silicon access), `access`, `grant`
and `revoke` (any active Silicon, by `si:` id or uuid), `activity`, `device_requests`, `setup`,
`setup_code`, `retry_setup`, and the wake requests: `wake_requests`, `answer_wake`
(`WakeAnswer::woken()` or `declined()`), `set_wake_settings` (`WakeSettings::new(true).silicon(id)`).
`devices_including_removed` also lists the Carbon's removed devices, whose `device()` and
`activity()` stay readable. `lookup(id)` resolves a `c:`/`si:` id to its account.

## The Silicons a Carbon looks after

A Carbon who is a Silicon's custodian (in Silicon Accounts) sees what it does in Extend and can stop
it, never act as it:

- `silicons()`: the Silicons the caller looks after, then the ones they gave access to;
  `silicon(id)` one of them; `silicon_grants(id)` every device a looked-after Silicon can use;
  `renounce(silicon, device_id)` takes one away (the Silicon may renounce its own too).
- `ListQuery { silicon: Some("si:chef".into()), .. }` narrows `sessions`, `files` and `requests` to
  that Silicon; `end_session` stops one of its sessions; `cancel_wake` withdraws its wake request.

## Ting

`ting_registration()` says whether Extend's notifications reach the caller (`on`, `off`, `pending`),
which of Extend's types Ting reported missing, and `delivery_enabled` (false while the server sends
no notifications through Ting); `ting_turn_on()` registers the caller again.

## Tokens

Refresh with `SignIn::refresh(refresh_token)`. The refresh token rotates: the old one stops working
at once, and presenting it again revokes the whole sign-in. So refresh once per token (under a lock
when several processes share it), store the new pair before using it, and treat
`AuthError::sign_in_ended()` as "sign in again". `silicon_extend_client::needs_refresh(&err)` is true
when Extend refused an access token as expired (`401 token_expired`): refresh once and retry.

Sign out with `Authed::sign_out(Some(refresh_token))`: Extend revokes the sign-in at Silicon Accounts
and ends what it runs (a Silicon's sessions; a Carbon's Silicons' sessions on their pairs). When
Extend can't be reached, `SignIn::revoke(refresh_token)` revokes it at Silicon Accounts directly.

## Building a device app

The same crate has the device side, on API v1: `enroll`, `enrollment`, `device_self`,
`update_device_self`, `revoke_pair`, `device_stop`, `upload_artifact`, `ws_url`, and
`pair_enrollment(credential)` for "Pair with another Carbon". The socket protocol, including one
socket per pair, is [device-protocol.md](device-protocol.md).

## Contract fixtures

Every public call is recorded as a fixture by the crate's `contract_fixtures` test: account calls in
`contracts/v2/client/`, device-side calls in `contracts/v1/client/`. The service's `contracts` test
replays them against a real service, so a service change that would break this crate fails before it
ships (`contracts/README.md`). Released crates' fixtures are frozen (`contracts/v1/client-1.0.0/` …
`client-3.1.1/`) and replayed too; their account calls now get `410 api_version_sunset` with
`silicon-apps update extend`. After an intended change to what a call sends, rewrite them with
`EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures`.
