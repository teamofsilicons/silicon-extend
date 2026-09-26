# The Rust client: `silicon-bridge-client`

The client is the primary interface to Bridge; the `bridge` CLI is built on it and has no feature it
lacks. It is **stateless**: it holds a base URL, a connection pool, the negotiated API version and
(optionally) a test-environment secret. Where tokens live is your choice.

```toml
[dependencies]
silicon-bridge-client = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Connect and sign in

```rust
use silicon_bridge_client::{Client, DeviceQuery};

let client = Client::connect("https://backend.bridge.teamofsilicons.com").await?; // negotiates the API version
let session = client.login(&slt).await?;          // a short-lived token from Silicon IAM
let me = client.authed(&session.access_token, Some("acme"));   // team handle, sent as X-Org-ID
```

`Client::builder(url).testing_secret(secret).connect()` selects a test environment; every call then
runs there, and a production token won't work in it.

## Use a device (as a Silicon)

```rust
use bridge_protocol::model::CommandRequest;

let devices = me.devices(DeviceQuery::default()).await?;
let device = &devices.items[0];
let s = me.start_session(&device.device_id).await?;         // Err(code = device_in_use) if busy
let sid = s.session_id.to_string();
let snap = me.run(&sid, &CommandRequest {
    command: "snapshot".into(),
    args: vec!["-i".into()],                                 // agent-device CLI tokens
    timeout_ms: None, self_destruct_minutes: None, permanent: false, attachments: vec![],
}).await?;
println!("{}", snap.text.unwrap_or_default());
me.end_session(&sid).await?;
```

A command that ran but failed on the device returns `Ok(result)` with `result.ok == false` and
`result.error`. Everything the service refused is an `Err(Error::Api { error, .. })` with a stable
`error.code`, a `message` that says what and why, and a `hint` saying what to do.

## Manage devices (as a Carbon)

`pair`, `attach`, `update_device` (with the version from `device()` for optimistic concurrency),
`remove_device`, `stop_device`, `access`/`grant`/`revoke`, `activity`, `device_requests`,
`team_silicons`, `setup`, `setup_code`.

## Tokens

Refresh with `client.refresh(refresh_token, idempotency_key)`. Refresh one at a time per token and
store the new pair atomically; reusing a spent refresh token revokes the family (Silicon IAM rule).
`silicon_bridge_client::needs_refresh(&err)` tells you when an error means "refresh and retry".

## Building a device app

The same crate has the device side: `enroll`, `enrollment`, `device_self`, `revoke_pair`,
`upload_artifact` and `ws_url`. The socket protocol is [device-protocol.md](device-protocol.md).
