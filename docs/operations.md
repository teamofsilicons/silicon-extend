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
- Known gaps (2026-09-27): a pending Ting request is retried only while its sender still has a
  running session, which a sender normally no longer has; and a self-destruct due after the
  Silicon's session ended can't act for it in Briefcase, yet Extend drops its record, so the file
  stays in Briefcase. Both need the Silicon's latest authorized principal (`completion-work.md`).
- Stale enrollments are deleted after an hour; a claimed credential that is never collected is
  deleted after 10 minutes.

## Logs and signals

`EXTEND_LOG_FORMAT=json`, `EXTEND_LOG=info`. Every response has `X-Request-ID`; errors carry it too,
so a CLI error message can be traced to its log lines. Telemetry events arrive at
`/api/v1/telemetry` and are stored per world in `telemetry` and logged under target `telemetry`.
They are exported to Space Station when `EXTEND_SPACE_STATION_KEY` is set (per test environment:
`EXTEND_TEST_TELEMETRY_KEYS`); no ingest key has been available, so the export has not been seen
working against Space Station.

## Rate limits

Pairing-code claims are limited per Carbon (5 failed per 10 minutes) and per address (30 per hour),
and new enrollments to 60 per hour per address. The counters live in the process's memory, so a
restart resets them. Several end-to-end runs against one shared development service use up the
enrollment limit within minutes (HTTP 429 `rate_limited`, with a retry time); run test lanes against
their own service instance.

## Revocation

IAM webhooks at `/webhook/` are verified by the official client, de-duplicated by event id, and
treated as prompts: Extend re-checks with IAM before ending access, and re-checks access on every
command regardless.
