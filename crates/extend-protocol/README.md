# silicon-extend-protocol

Wire types shared by the [Silicon Extend](https://extend.teamofsilicons.com) service, its Rust
client ([`silicon-extend-client`](https://crates.io/crates/silicon-extend-client)), the `extend`
CLI and the device apps: identifiers, envelopes, errors, WebSocket frames, capabilities, and the
Silicon Accounts account shapes of API v2.

Most users want the client, which re-exports this crate as `silicon_extend_client::protocol`.
MIT licensed.
