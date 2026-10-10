# Silicon Extend CLI and Rust client 1.3.0

`extend act 'Click Save'` uses Silicon-managed Jev by default, so agents no longer need a
TypeSafe key. Sign in to Extend and connect a ref-capable session as usual. Set
`TYPESAFE_API_KEY` to use your own key instead; a personal key takes priority and calls Jev
directly. The managed key stays on Extend's backend and is never shipped in the CLI.

Jev remains optional. Ordinary snapshot and ref commands work independently of it. Selection
errors, timeouts and abstentions provide `normal_ref_fallback` instructions for the calling
agent; an explicitly configured `--fallback llm` is also available. The CLI still checks a fresh
snapshot before executing one ordinary command, and never retries an execution as a model action.

The Rust SDK adds `Authed::select_ref` and `ref_actions::RefSelectionRequest`. Managed selection
requires service 1.2.0 and an active session owned by the calling Silicon. Session access and
device availability are checked before inference. Production and test worlds use separate
server credentials. The endpoint only returns a decision; it never executes a device command.

Use `--dry-run --json` to inspect the selected ref and timings. Fill still requires exact `--text`.
CLI/client 1.2.0 installations must update to receive managed selection; their existing BYOK
commands continue working. Protocol and device applications remain at 1.1.0.

Install or update through `honeycomb install extend`, or run
`cargo install silicon-extend-cli --version 1.3.0 --locked`. Rust consumers can use
`silicon-extend-client = "1.3.0"`.

- [Configuration and behavior](https://github.com/teamofsilicons/silicon-extend/blob/cli-v1.3.0/docs/jev-ref-experiment.md)
- [Earlier direct-provider measurements](https://github.com/teamofsilicons/silicon-extend/blob/cli-v1.3.0/docs/jev-ref-results-2026-09-30.md)

Earlier measurements describe direct Jev selection, not the new managed backend round trip.
