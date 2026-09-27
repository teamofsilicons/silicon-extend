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
  files past their self-destruct time are deleted; requests Ting hasn't accepted are retried;
  unclaimed uploads are removed.
- Self-destruct and Ting retries act as the Silicon, with the latest login Extend holds for it in
  that team and world (its authorization cache, else a running session; no session is needed).
  A file's record is deleted only once Briefcase confirms (a 404 counts as gone); a failure is
  logged at `warn` with `file_id`, `tries`, `retry_in_s`, the error and a hint, and retried after
  1 minute, doubling to 1 hour. Up to 100 files per pass, not counting ones still backing off. A
  pending request gets 6 attempts, the first send included; each is counted even when no login is
  held, and the last marks it `failed` with `last_error` and logs `request_failed` on the device.
  Logins are held in memory: after a restart these wait for (or, for requests, fail without) the
  Silicon's next use of Extend (`TECHNICAL.md` open question C2).
- Each pass over a test world holds that world's fence and skips worlds that aren't open, so a
  clean, disable or purge never races the scheduler.
- Every 5 minutes each instance re-reads the API version lifecycle and sunsets a deprecated major
  that has gone 7 consecutive days without a request (`TECHNICAL.md` section 10). Requests are
  counted per major and UTC day in `extend_global.api_version_usage`.
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

Pairing-code claims are limited per Carbon (5 failed per 10 minutes; the per-address limit
`TECHNICAL.md` proposes was never built), answers about codes from another environment to 20 per
Carbon per 15 minutes, bug reports to 10 per member per hour, and new enrollments to 60 per hour per
client address (read from `X-Forwarded-For` only when the peer is in `EXTEND_TRUSTED_PROXY_CIDRS`).
Adds to one test environment take turns; one that waits more than 10 s (5 s on the database lock)
gets `429 rate_limited`. The counters live in the process's memory, so a restart resets them. Several end-to-end runs against one shared development service use up the
enrollment limit within minutes (HTTP 429 `rate_limited`, with a retry time); run test lanes against
their own service instance.

## Revocation

IAM webhooks at `/webhook/` are verified by the official client, de-duplicated by event id, and
treated as prompts: Extend re-checks with IAM before ending access, and re-checks access on every
command regardless. An event is recorded only after its effects apply (a failure answers 5xx and
IAM retries it), events about one aggregate apply in order, and deliveries for a test environment
that isn't open are dropped. Production refuses to start without `EXTEND_IAM_WEBHOOK_SECRET`.

IAM sends no logout events, so a Silicon that logs out elsewhere is noticed on its next call to a
session route: 15 s after IAM first refuses its login there, unless IAM accepts another login of
it, its running sessions end as `silicon_logged_out` (logged at `info`; "IAM could not say…" at
`warn` when IAM didn't answer, and nothing ends). A Silicon refused for a team where it has
running sessions has them ended as `left_team` unless IAM confirms it is still a member.

## Test environments

`clean`, `disable` and `purge` wait for requests and scheduler passes already running in that
environment, then keep new ones out until they finish. A disabled environment's devices keep
their pairs: their sockets close with code 4503 and they reconnect by themselves after `restore`.
A lifecycle instruction that failed leaves a `failed` receipt; retry the identical instruction.
