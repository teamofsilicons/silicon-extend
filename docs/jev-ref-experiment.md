# Jev reference-action experiment

This opt-in experiment adds `extend act` on the caller's machine. It selects **one** existing
reference action with TypeSafe Jev, or an explicitly configured OpenAI-compatible LLM. It does
not change existing `click @ref` commands, install a planner in the service, or release an update.
The calling Silicon retains multi-step planning. The device service still owns authentication,
authorization, takeover, revocation, and command execution.

## Scope

The integration sits above the shared session API, so it uses whichever ref-capable driver the
session already uses. Nested Android snapshots and flat engine/Windows snapshots are normalized
into the same decision space. This covers the schema paths for Android phones/tablets, Android
TV/Fire OS, iPhone/iPad, macOS, Linux, and Windows. A session must advertise `snapshot` and at least
one of `click`, `fill`, `focus`, or `get`. Remote-button-only TVs and screenshot-only paths are not
eligible. Schema coverage is not proof of native-device execution on each platform.

Scrolling, arbitrary coordinates, keyboard sequences, custom accessibility actions, generated
selectors, and multi-step autonomy are outside this first ref-only experiment. A normal Silicon
can still scroll, then delegate another `act`. Password controls are excluded from this model
path. Exact fill text comes from `--text`; the selection model cannot generate or change it.
Text beginning with `-` (except a lone `-`) is refused before inference because existing device
parsers do not consistently support escaping flag-like text.

## Configuration and use

Configure credentials in your local environment (never commit keys):

- `TYPESAFE_API_KEY`: Jev key.
- `EXTEND_JEV_MODEL`: optional; pinned default `jev-1.13.0`.
- `EXTEND_JEV_URL`: optional; default `https://api.typesafe.ai/v1/systemone`.
- `EXTEND_REF_LLM_URL`: full OpenAI-compatible chat-completions endpoint for baseline/fallback.
- `EXTEND_REF_LLM_KEY`, `EXTEND_REF_LLM_MODEL`: baseline/fallback credentials and model.

The configured providers receive the instruction and structured screen text, including ordinary
field values. They do not receive Extend credentials. Model endpoints require HTTPS except
loopback test servers; redirects are disabled. API errors do not echo provider response bodies.

```sh
cargo build -p silicon-extend-cli
target/debug/extend act 'Click Save' --dry-run --json
target/debug/extend act 'Click Save' --json
target/debug/extend act 'Fill the Email field' --text 'alice@example.test'
target/debug/extend act 'Click Save' --provider jev --fallback llm --json
target/debug/extend act 'Click Save' --provider llm --json
```

Normal global options, including `--session`, `--team`, and `--test`, keep their meaning. `--scope`
passes the same scope to both snapshots. `--timeout` bounds each provider/device request, not
the combined wall time. No provider is silently substituted. Provider config is checked before
screen capture, including the fallback config if requested.

## Decision and execution contract

1. Read the live session's supported commands and a full interactive snapshot.
2. Offer only observed, enabled, non-hidden refs, and only the session's supported operations.
   Fill/focus require editable controls; fill additionally requires `--text`.
3. Jev gets the operation and speculative target questions in one request. Every question
   includes `BLOCKED`. Unused target answers cannot execute.
4. Validate the choice, full probability distribution, and membership in the candidate set.
   Jev's confidence and winning probability must both meet the threshold (default 0.7).
   The LLM baseline receives the same state and choices, but produces an operation/ref JSON pair;
   it has no calibrated confidence threshold. Both can abstain.
5. If explicitly requested, LLM fallback handles abstention or provider failure, before execution.
6. Capture again and require identical normalized context and refs before executing. If changed,
   return `stale` without an action. Pin the fresh generation where the driver exposes
   `refsGeneration`; preserve plain refs on other drivers. Existing driver freshness checks remain
   authoritative. This does not create atomic snapshot-plus-execution on legacy drivers.
7. Send the chosen ordinary device command once. An execution failure never invokes model fallback
   or an automatic retry. Each normal command is still logged by the service.

No candidate truncation: over 254 eligible refs or 128 KB of request context requires a narrower
scope. Ref-based execution remains subject to the quality and completeness of the native tree.

`--json` includes the decision, each model attempt, device result, and timings: `snapshot_ms`,
`selection_ms` (including fallback), `revalidate_ms`, `execution_ms`, and `total_ms` (including
session lookup). `selected` means dry-run only; `executed` means the device command succeeded,
not that a larger user task was verified. `blocked`, `stale`, and `execution_failed` exit nonzero.

Jev is optional. Ordinary snapshot and ref commands have no model dependency and remain usable
after API failures, timeouts, invalid responses, absent credentials or abstentions. Selection
errors carry `error.details.normal_ref_fallback`; blocked/stale outputs carry `normal_ref_fallback`.
This tells the calling agent to take a fresh snapshot in the same session and continue using normal
ref commands. It is a handoff to the existing planner, not an automatic guess at the intended ref.
An explicitly configured `--fallback llm` can instead recover during selection. Execution failures
do not provide this retry handoff because an action may have already affected the device.

## Benchmark

The paired runner uses the production selection implementation for both providers, alternates
provider order, records warmups separately, and retains all errors and incorrect choices. It
reports accuracy, abstentions, errors, p50/p95 over all attempts, and median speedup on pairs where
both were correct. Failed provider calls make the report incomplete and exit nonzero.

```sh
cargo run -p silicon-extend-client --example ref_benchmark -- \
  e2e/ref-actions/cases.json --validate-only

cargo run --release -p silicon-extend-client --example ref_benchmark -- \
  e2e/ref-actions/cases.json --repeats 5 --out /tmp/extend-ref-benchmark.json

# Measure Jev alone while the baseline is unavailable; this cannot establish speedup.
cargo run --release -p silicon-extend-client --example ref_benchmark -- \
  e2e/ref-actions/cases.json --provider jev --repeats 5 --out /tmp/extend-jev-benchmark.json
```

The bundled 12 cases are **synthetic schema/decision fixtures**, not captured device sessions.
They include eight platform labels, ambiguous/missing targets, disabled controls, read-only
commands, and requests requiring text generation. For a representative accuracy measurement,
replace or extend them with labelled captured snapshots and retain `provenance` per case.

This runner measures **model selection only**, not end-to-end device operation. To measure total
action speed, reset a controlled app to the same state before each `extend act` arm, alternate
`--provider jev` and `--provider llm`, and independently verify the resulting UI state. Include
stale refusals and failed actions. Direct `click @ref` timing is a transport/execution control,
not an equivalent baseline for semantic ref selection. Record device/OS, service location, model
versions, corpus, fallback frequency, success rates, and timings. Do not derive task-cost savings
from token counts without provider billing data.

## Evidence as of 2026-09-30

The CLI/client all-target suite passed 94 tests, including 14 new experiment tests across snapshot
normalization, decision validation, CLI orchestration, and benchmark accounting. Clippy passed
with warnings denied. All 12 synthetic benchmark cases validated without model calls or device
actions. These checks validate the integration logic, not Jev's decision quality.

Live Jev API measurements now exist using the supplied **test** key and pinned `jev-1.13.0`:
the corrected 12-case smoke corpus, repeated five times, produced 60/60 expected decisions,
323 ms median and 392 ms p95 selection latency with a reused connection. At 254 refs the median
was 598 ms. Five fresh-connection probes had a 512 ms median, versus 315 ms for their subsequent
requests. These numbers include the network round trip, not capture or device execution.

The initial run exposed a fill-context omission: the model was not explicitly told that the
caller already supplied the text. The request now carries that fact without transmitting the
literal field value. Initial results, including five fill refusals and one HTTP 520, remain in
the evidence. The corrected corpus is a development smoke test, not a held-out accuracy test.

See [full measurements and raw reports](jev-ref-results-2026-09-30.md). Native end-to-end timing
is still unmeasured: the installed Extend CLI reported not signed in, ADB had no attached devices,
and no iOS simulator was booted. The user dropped the LLM comparison; no comparative speedup is claimed.

## References

- [TypeSafe HTTP request and response contract](https://docs.typesafe.ai/api)
- [Browserbase Stagehand Jev integration](https://github.com/browserbase/stagehand/pull/2953)
- [Browser Use decision loop](https://github.com/browser-use/jev-ultrafast)
- [TypeSafe parallel questions](https://docs.typesafe.ai/patterns/fan-out)
- [Jev-Mobile](https://arxiv.org/abs/2609.30186)
