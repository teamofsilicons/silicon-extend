# Operations

## One instance

Each paired device keeps one WebSocket to the service, and a command waits in the process that
holds that socket. A second instance would accept sockets and commands that can't see each other.
Scale-out needs a shared relay (PostgreSQL `LISTEN/NOTIFY` or a small broker) that forwards frames
to whichever instance holds the device's socket. Until then, run exactly one instance and let the
orchestrator restart it; devices reconnect with backoff (1–60 s) and the in-use indicator is
re-sent on reconnect.

## What happens on its own

A scheduler in the service runs every 2 seconds:

- Pairing codes rotate every 5 minutes while the app waits on its enrollment socket.
- Sessions end after 5 idle minutes (`idle_timeout`), when a takeover runs past 30 minutes, and when
  a device stays offline for 2 minutes (`device_offline`).
- Every 30 seconds: pairs that went unused longer than their owner allowed end (`pair_expired`);
  files past their self-destruct time are deleted; requests Ting hasn't accepted are retried (up to
  6 attempts); unclaimed uploads are removed.
- Stale enrollments are deleted after an hour; a claimed credential that is never collected is
  deleted after 10 minutes.

## Logs and signals

`EXTEND_LOG_FORMAT=json`, `EXTEND_LOG=info`. Every response has `X-Request-ID`; errors carry it too,
so a CLI error message can be traced to its log lines. Telemetry events arrive at
`/api/v1/telemetry` and are stored per world in `telemetry` and logged under target `telemetry`
(forwarding to Space Station is the next step: set up the ingest key and exporter — not built yet).

## Revocation

IAM webhooks at `/webhook/` are verified by the official client, de-duplicated by event id, and
treated as prompts: Extend re-checks with IAM before ending access, and re-checks access on every
command regardless.
