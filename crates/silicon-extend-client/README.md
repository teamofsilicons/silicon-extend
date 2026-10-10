# silicon-extend-client

The official Rust client for [Silicon Extend](https://extend.teamofsilicons.com), which lets a
Silicon use the devices a Carbon has paired: Android phones and TVs, Macs, Windows and Linux
computers, iPhones, iPads, Apple TV and smart TVs.

Every Carbon and Silicon signs in with [Silicon Accounts](https://accounts.teamofsilicons.com).
The client is stateless: it holds a base URL, a connection pool and the negotiated API version,
and you pass the access token per call. Where tokens live is up to you. The `extend` CLI
([`silicon-extend-cli`](https://crates.io/crates/silicon-extend-cli)) is built only on this crate.

```toml
[dependencies]
silicon-extend-client = "4"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use silicon_extend_client::{Client, DeviceQuery, auth::SignIn};

// A Silicon: the short-lived token comes from `silicon-accounts login --app extend -q`.
let sign_in = SignIn::new("https://accounts.teamofsilicons.com")?;
let tokens = sign_in.exchange_slt(&slt).await?;

let client = Client::connect("https://api.extend.teamofsilicons.com").await?;
let me = client.authed(tokens.access_token.expose());
let devices = me.devices(DeviceQuery::default()).await?;
```

## Signing in

`auth::SignIn` signs in the way Extend's own tools do: as a public client, with `client_id`
`extend` and no secret.

- **Carbons**: `start_device(label)` returns a code and a link; the Carbon approves it on the
  Silicon Accounts site, on any device; `wait_for_device(&code, progress)` polls, honouring
  `interval` and `slow_down`, until it is approved, denied (`device_denied`) or expires after 10
  minutes (`device_expired`).
- **Silicons**: `exchange_slt(slt)` exchanges a short-lived token (single use, 2 minutes, for
  Extend only). A refusal says exactly why: `slt_already_used`, `slt_expired`, `slt_wrong_app`,
  `slt_unknown`, `slt_sign_in_ended`; a value that isn't an `slt_…` token is refused before
  anything is sent (`not_an_slt`). The token never appears in an error.
- `refresh(refresh_token)` rotates the refresh token. The old one stops working, and presenting it
  twice revokes the whole sign-in: refresh once per token (under a lock if several processes share
  it) and store the new pair before using it. `sign_in_ended` means it is over: sign in again.
- `revoke(token)` ends the sign-in.

Access tokens live 30 minutes. When Extend answers `401 token_expired`
(`silicon_extend_client::needs_refresh`), refresh once and retry.

## What it covers

- `Client`: the version handshake (API v2 for accounts; the device wire stays v1), `accounts()`
  (the Silicon Accounts Extend trusts, and its links), `contracts()`, and the device side the
  Extend apps use (`enroll`, `device_self`, `upload_artifact`, …).
- `Authed` (`client.authed(access_token)`): who you are (`me`), signing out (`sign_out`), looking
  an account up by id (`lookup`), devices, pairing, access, setup, sessions and commands, requests,
  waking a device, Ting status, files, reports, and the custodian views: `silicons()`,
  `silicon(id)`, `silicon_grants(id)`, `renounce(silicon, device)`, and the `silicon` filter on
  sessions, files and requests.

A device belongs to the Carbon who paired it and the Silicons they give access
to, by `si:` id or uuid. A Carbon sees and can stop what the Silicons they look after do in
Extend, and never acts as them.

Errors keep Extend's envelope: `Error::Api { status, error }` with `code`, `message`, `hint`,
`request_id` and `details`; sign-in errors are `auth::AuthError` with `code`, `message` and `hint`.

The full guide is [docs/client.md](https://github.com/teamofsilicons/silicon-extend/blob/main/docs/client.md).
MIT licensed.
