# Silicon Extend CLI, Rust client and service 2.0.0

This major release removes Jev and the model-based ref-selection experiment. The CLI no longer
provides `extend act`, the Rust client no longer provides `Authed::select_ref` or `ref_actions`,
and the service no longer exposes the ref-selection endpoint. Managed provider keys, personal
provider keys and model fallback configuration are no longer used by Extend.

Agents continue through ordinary snapshots and device commands. Read the current screen, choose
an exposed ref, then execute the command with the exact requested text where applicable:

```sh
extend snapshot -i
extend click @e2
extend snapshot -i
extend fill @e3 "hello"
```

The refs in this example are placeholders: use refs returned by your current snapshot. Inspect
the resulting screen before the next action. `extend snapshot --raw` reads the full accessibility
tree exposed by the device. Rust consumers use `Authed::run` with ordinary `CommandRequest`
values; [the client guide](../client.md) shows session and command handling.

The ordinary HTTP API remains v1. Existing ordinary command calls continue to work; callers of
the removed model-selection APIs must move to the snapshot/ref workflow. Prior release notes
describe their historical versions and do not describe 2.0.0 behavior.

Android's optional debugging connection, protected-screen coordinate taps and keyboard text
input remain available in Android 1.1.2. This release does not rebuild Android, desktop apps or
the device protocol; desktop and protocol remain at 1.1.0. Their existing download links remain
unchanged.

Update the CLI through `honeycomb install extend`, or install
`cargo install silicon-extend-cli --version 2.0.0 --locked`. Rust consumers should use
`silicon-extend-client = "2"`. Remove obsolete provider configuration from agent launch
environments. Server operators should follow the [deployment guide](../deployment.md) to remove
retired runtime settings and refresh the host environment renderer. There is no database
migration for this release.
