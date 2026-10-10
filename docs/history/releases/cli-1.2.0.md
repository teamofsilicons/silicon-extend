# Silicon Extend CLI and Rust client 1.2.0

`extend act 'Click Save'` now lets a Silicon delegate one reference-based action to TypeSafe Jev.
The model chooses an operation and an existing element ref in one request. The CLI checks a fresh
snapshot before executing through the normal authorized session. Low-confidence decisions abstain;
an LLM fallback is available when explicitly configured.

Jev is optional: ordinary snapshot and ref commands keep working without a key or after provider
errors, timeouts, malformed responses and low-confidence decisions. Selection errors and abstentions
include a `normal_ref_fallback` handoff for the calling agent to resume that workflow. The CLI does
not guess a target after model failure. An execution failure never triggers a duplicate action.

This is opt-in per command. Configure `TYPESAFE_API_KEY` locally; no provider key is included in
the release. Use `--dry-run --json` to inspect decisions and timings. Fill requires exact `--text`.
Supported actions are click, fill, focus and get-text, subject to the device's advertised commands.
The shared implementation handles nested Android and flat desktop/Apple snapshots. Screenshots,
coordinate actions, scrolling and multi-step planning continue through the existing commands.

The Rust client adds the experimental `ref_actions` module and a reproducible benchmark runner.
Live tests with `jev-1.13.0` measured 323 ms median / 392 ms p95 on small synthetic ref screens
with a reused connection, 512 ms median on fresh connections, and 598 ms median at 254 refs.
After fixing missing supplied-text context, 60/60 decisions matched the 12-case development corpus
repeated five times. These are model-selection measurements, not native task success or end-to-end
action timings. The initial provider failure and fill refusals are retained in the full report.

The release provides all six CLI targets: Linux, macOS and Windows on x86_64 and ARM64. Service,
protocol and device apps stay at 1.1.0. No service deployment or device update is required.

Install or update through `honeycomb install extend`, or run
`cargo install silicon-extend-cli --version 1.2.0 --locked`. Rust consumers can use
`silicon-extend-client = "1.2.0"`.

- [Configuration and behavior](https://github.com/teamofsilicons/silicon-extend/blob/cli-v1.2.0/docs/jev-ref-experiment.md)
- [Measurements and limitations](https://github.com/teamofsilicons/silicon-extend/blob/cli-v1.2.0/docs/jev-ref-results-2026-09-30.md)
