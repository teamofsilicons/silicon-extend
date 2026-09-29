# Jev ref-selection measurements — 2026-09-30

Jev selects a ref in about **0.32 seconds with a reused connection**, or **0.51 seconds for the
first request on a fresh connection**, in this local experiment. A 254-ref screen took about
0.60 seconds with a reused connection. This is selection latency including network time, not
the time to capture a screen, execute a click, or complete a user task.

## Method

- TypeSafe `jev-1.13.0`, supplied test key; production key unused.
- Release Rust executable on macOS ARM64. Requests ran sequentially from this machine.
- One request contains the operation and all applicable target questions. Threshold stayed 0.7.
- Main corpus: 12 small synthetic cases, each repeated five times; seven actionable cases and
  five requiring abstention. One separately recorded warmup precedes measurements.
- Scaling corpus: four synthetic screens with unique, easy targets, each repeated five times.
  These isolate candidate-count effects; they do not represent difficult real screens.
- Connection probe: five separate processes, each making a first request and then a second
  request using the same client, over the same two-ref click case. Reported times exclude process startup.
- Percentiles use nearest rank. With five samples, p95 is the observed maximum. Small sample
  sizes cannot establish a service latency guarantee or reliable tail behavior.
- The user dropped the GPT comparison. There is no measured improvement over another model.

## Results after the fill-context correction

| Workload | Measured requests | Median | p95 | Expected decisions |
|---|---:|---:|---:|---:|
| Small ref screens, reused connection | 60 | 323 ms | 392 ms | 60/60 |
| 8 refs, reused connection | 5 | 332 ms | 400 ms | 5/5 |
| 32 refs, reused connection | 5 | 335 ms | 409 ms | 5/5 |
| 128 refs, reused connection | 5 | 417 ms | 513 ms | 5/5 |
| 254 refs, reused connection | 5 | 598 ms | 635 ms | 5/5 |
| Fresh connection, two-ref click | 5 | 512 ms | 680 ms | 5/5 |
| Same client's next request, two-ref click | 5 | 315 ms | 375 ms | 5/5 |

The main run's 60 expected decisions include 35 accepted actions and 25 correct abstentions.
They represent 12 unique fixtures, not 60 independent tasks. No device actions were executed.
The corrected and scaling runs returned no provider errors. The main warmup was 500 ms.

## Failure found and corrected

The initial 60-request run produced 54 expected decisions, five fill refusals, and one HTTP 520
lasting 5.1 seconds. Its median was 305 ms and p95 was 425 ms, with the error retained. The fill
request offered a fill operation but did not explicitly tell the model that code already had
the caller's literal text. Jev either abstained or fell below the unchanged confidence threshold.

The shared request now includes `caller_supplied_fill_text` and explains that choosing fill does
not require generating or seeing the text. After this change, the same five fill repetitions
passed. This is evidence that the integration fix addressed this fixture; the reused corpus is
not an independent evaluation of general accuracy. All initial results are retained below.

## Implication for Extend

The SDK can reuse one `ModelClient` across steps, as the main benchmark did. The current `extend act`
CLI creates a new client for each invocation, so the fresh-connection measurement is the more
relevant model-call estimate for that entry point. Its snapshots, session lookup and device
execution add latency beyond these numbers. Keeping a client alive would avoid repeated connection
setup, but a persistent CLI worker has not been implemented or measured here.

Native task success and full action latency remain unverified. A connected device and labelled
real snapshots are the next evidence needed before judging this suitable for general use.

## Reproduce and inspect

Set `TYPESAFE_API_KEY` locally, then run:

```sh
cargo run --release -p silicon-extend-client --example ref_benchmark -- \
  e2e/ref-actions/cases.json --provider jev --repeats 5 --out /tmp/jev-small.json
cargo run --release -p silicon-extend-client --example ref_benchmark -- \
  e2e/ref-actions/scaling.json --provider jev --repeats 5 --out /tmp/jev-scaling.json
```

For the connection probe, use one case and launch the runner in five fresh processes with
`--provider jev --repeats 1`. Compare each report's `warmups[0].wall_ms` with its `rows[0].wall_ms`.

- [Initial run, including failures](../e2e/ref-actions/results/2026-09-30/jev-initial.json)
- [Corrected run](../e2e/ref-actions/results/2026-09-30/jev-fill-context-fixed.json)
- [Candidate scaling](../e2e/ref-actions/results/2026-09-30/jev-scaling.json)
- [Fresh versus reused connections](../e2e/ref-actions/results/2026-09-30/jev-connection-overhead.json)

Raw corrected/scaling reports include timestamps and source/corpus hashes. No keys are stored
in these artifacts. The temporary credential file used for the first run was removed.
