# silicon-extend-client

The official Rust client for [Silicon Extend](https://extend.teamofsilicons.com), which lets a
Silicon use the devices a Carbon has paired: Android phones and TVs, Macs, Windows and Linux
computers, iPhones, iPads, Apple TV and smart TVs.

The client is stateless: it holds a base URL, a connection pool and the negotiated API version.
Where tokens live is up to you. The `extend` CLI ([`silicon-extend-cli`](https://crates.io/crates/silicon-extend-cli))
is built only on this crate.

```toml
[dependencies]
silicon-extend-client = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use silicon_extend_client::{Client, DeviceQuery};

let client = Client::connect("https://backend.extend.teamofsilicons.com").await?;
let session = client.login(&slt).await?;                  // a short-lived token from Silicon IAM
let me = client.authed(&session.access_token, Some("acme"));
let devices = me.devices(DeviceQuery::default()).await?;
```

The full guide is [docs/client.md](https://github.com/teamofsilicons/silicon-extend/blob/main/docs/client.md),
and the HTTP contract is [api.yaml](https://github.com/teamofsilicons/silicon-extend/blob/main/understanding/api.yaml).
MIT licensed.

The experimental `ref_actions` module selects one existing ref with TypeSafe Jev or a configured
OpenAI-compatible model. It does not execute device actions; callers own revalidation and fallback
to their normal planner. See [configuration and benchmarks](https://github.com/teamofsilicons/silicon-extend/blob/main/docs/jev-ref-experiment.md).

Version 1.3 adds `Authed::select_ref` for Silicon-managed Jev through the existing Extend login,
without a personal TypeSafe key. It requires service 1.2.0 and an active authorized session.
Direct `ref_actions::ModelClient` calls still support your own provider key.
