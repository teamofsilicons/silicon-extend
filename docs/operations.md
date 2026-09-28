# Operations

## One instance

Each pair keeps one WebSocket to the service (a device several Carbons paired keeps one per pair,
at most `EXTEND_MAX_PAIRS_PER_DEVICE`), and a command waits in the process that holds that socket. A second instance would accept sockets and commands that can't see each other.
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

1.1 adds, on the scheduler's slow pass:

- **Wake requests.** Open requests expire 30 minutes after their last ask. Carbon Tings deferred by
  the hourly limit are sent oldest first once the hour's window frees, if the request is still open
  and no other Ting covers it. Carbon, woken and declined Tings are retried (below).
- **Ting retries.** Requests, wake Tings and their answers are retried with the exact body first
  sent, backing off 30 s, then 1, 2, 4 and 8 minutes, then every 8 minutes. The sender is a member
  who took part (the asking Silicon, the answering Carbon, or the recipient Carbon to themselves),
  never the Silicon using the device; each Ting has a chain of such members, and an attempt with no
  login held for any of them doesn't count. A pending Ting also goes out when a member of its chain
  next signs in to Extend (the set of such members is rebuilt at every start). Requests give up after
  6 counted attempts or 24 hours; a Carbon's wake Ting still pending when its request ends becomes
  `failed` ("the request ended before delivery"); woken and declined Tings give up 30 minutes after
  their request ended. A recipient who turned Extend off in Ting stops the retries until "Turn on".
  An `idempotency_conflict` answer counts as delivered. Requests a 1.0.0 service stored are retried
  in the 1.0 shape.
- **Provisional carried devices.** In a test environment, a carried device accepted over the device
  limit while it waits to be recognised is removed if it isn't linked within
  `EXTEND_TEST_LINK_WINDOW_S`, with the limit message.
- **The membership sweep** every `EXTEND_MEMBERSHIP_SWEEP_HOURS` (below).
- At start, the service migrates every test schema that isn't retired before the scheduler starts,
  and, after a rollback to 1.0.0, restores the grants the down step set aside (unless they were
  revoked or their pair ended since), logging each as `access_granted` with
  `{"restored_after_rollback": true}`.

## Settings added in 1.1

| Variable | Default | What it does |
|---|---|---|
| `EXTEND_MAX_PAIRS_PER_DEVICE` | 8 | Most Carbons one device may be paired to. A guard on sockets per device, not a product rule; the 9th "Pair with another Carbon" answers `409` with that number. |
| `EXTEND_MEMBERSHIP_SWEEP_HOURS` | 6 | How often every grant's Silicon and granting Carbon are re-checked with IAM. |
| `EXTEND_OWNER_CHECK_CACHE_S` | 30 | How long IAM's "still a member" for the Carbon behind a grant is reused. Only positive answers are kept; an IAM event about the Carbon and a test clean clear it. |
| `EXTEND_OWNER_CHECK_AT_USE` | `true` | Whether every use checks that the Carbon who gave access is still in the Silicon's Team. Set `false` if IAM hides a Team's Carbons from its Silicons (the release gate catches that); the webhook and the sweep still work. |
| `EXTEND_TEST_LINK_WINDOW_S` | 120 | How long a carried device added over a test environment's device limit may wait to be recognised. |
| `EXTEND_LOCAL_IAM_READERS` | (unset) | Development only: `strict` makes the local IAM stand-in answer a Silicon reading a Carbon's entry with 403, as a real IAM may. |

## Waking a device

A Silicon's wake request is accepted whenever the device isn't known to be awake, including offline.
The limits are the defaults the Carbon accepted, fixed in `extend-protocol`:

- one open request per physical device, Team and Silicon; asking again within 5 minutes is `429`,
  after that it refreshes the request; a request expires 30 minutes after the last ask;
- the device sounds for at most one request every 15 minutes (the others show silently);
- one Carbon Ting per pair and Team every 15 minutes (later asks are "covered");
- at most 6 wake Tings per Carbon per hour: an ask over that is accepted and its Ting deferred, so
  one Team's asks never make another Team's refused;
- a Carbon can turn wake requests off for a device or for one Silicon.

Extend never wakes a device. Awake never refuses a command; a command that fails on a device that
isn't awake gets the wake hint.

## Extend's Ting types

Ting 0.1.9 resolves notification types by environment context and application, across delivery
Teams. Registration is managed by the app's owning Team; a delivery Team does not register another
copy. Extend sends four:

| Type | Description it is registered with |
|---|---|
| `extend.device.requested` | A Silicon asks to use a device another Silicon is using |
| `extend.device.wake_requested` | A Silicon asks its Carbon to wake a device |
| `extend.device.woken` | A device a Silicon asked to wake is awake |
| `extend.device.wake_declined` | A Carbon turned down a request to wake a device |

Runbook:

- **The app's owning Team** registers all four once in each used context. For production Extend,
  use a manager in `tos`; the delivery Team can be different:

  ```sh
  ting --org <app-owning-team> types register --type extend.device.requested --description 'A Silicon asks to use a device another Silicon is using'
  ting --org <app-owning-team> types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'
  ting --org <app-owning-team> types register --type extend.device.woken --description 'A device a Silicon asked to wake is awake'
  ting --org <app-owning-team> types register --type extend.device.wake_declined --description 'A Carbon turned down a request to wake a device'
  ```

  The current Ting OBO catalog has no `types.register` endpoint. Use these manager commands;
  Extend cannot perform this registration through the Carbon's delegated login.
- **After every clean of a test environment**, register them again once for the app in that
  context: a Ting clean removes types, grants and hooks. Silicons are registered with Ting again at their next
  session; Carbons at their next grant or signed-in call.
- **Finding what is missing.** A Ting refused because its type is missing is recorded per Team
  (`ting_type_status`) and shown, never retried silently: in Settings, on the device page, in
  `extend ting status` (with `--all-teams`), in the delivery line of `request send` and
  `device wake`, and in `GET /api/v1/ting-registration`. It is retried every 10 minutes, and the next
  successful Ting of that type in that Team clears it. To list the Teams Extend sends in, from the
  production database: `SELECT team FROM extend.devices UNION SELECT team FROM extend.device_access
  UNION SELECT team FROM extend.sessions UNION SELECT team FROM extend.requests`.
- `docs/requests/ting-app-level-types.md` is an earlier proposal. Its per-Team premise is obsolete;
  reconcile it with the verified app-global delivery semantics before sending it.

## Logs and signals

`EXTEND_LOG_FORMAT=json`, `EXTEND_LOG=info`. The `credential` frame, credentials and raw hardware ids
are never logged. `connection_replaced` in a pair's activity log means another connection with that
pair's credential took over a live one: on a computer several Carbons paired, suspect a copied
credential (the next rotation, at the end of the next session there, makes the copy useless). Every response has `X-Request-ID`; errors carry it too,
so a CLI error message can be traced to its log lines. Telemetry events arrive at
`/api/v1/telemetry` and are stored per world in `telemetry` and logged under target `telemetry`.
They are exported to Space Station when `EXTEND_SPACE_STATION_KEY` is set (per test environment:
`EXTEND_TEST_TELEMETRY_KEYS`); no ingest key has been available, so the export has not been seen
working against Space Station.

## Rate limits

Pairing-code claims are limited per Carbon (5 failed per 10 minutes; the per-address limit
`TECHNICAL.md` proposes was never built), answers about codes from another environment to 20 per
Carbon per 15 minutes, bug reports to 10 per member per hour, and new enrollments to 60 per hour per
client address (read from `X-Forwarded-For` only when the peer is in `EXTEND_TRUSTED_PROXY_CIDRS`),
"Pair with another Carbon" included. 1.1 adds: at most 3 waiting "Pair with another Carbon" codes per
device, one setup retry per device every 5 s, and the wake limits above.
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

1.1:

- **Logout.** `POST /api/v1/auth/logout` by a Carbon (the website's Sign out, `extend logout`) ends the
  running sessions of the Silicons that Carbon gave access to, on that Carbon's side only.
- **Membership acts only on definite answers.** IAM's answer about a member of a Team is active, gone
  (a 404 after the reader proved it can read the Team) or unknown (a 403, an error, no reader).
  Nothing is deleted on unknown. A gone Carbon behind a grant refuses the call and ends their
  Silicons' running sessions in that Team at once; the grants are deleted only when a second reader
  confirms. If the second reader contradicts, the grants stay and an error is logged naming both
  readers; for 10 minutes a Silicon reader's gone for that Carbon counts as unknown. The sweep deletes
  only after two gone answers at least 10 minutes apart. A Carbon who leaves a Team loses the grants
  they gave there; their devices stay paired.

## Test environments

`clean`, `disable` and `purge` wait for requests and scheduler passes already running in that
environment, then keep new ones out until they finish. 1.1: a clean also forgets which members
Extend registered with Ting there and its cached membership answers, and removes every pair of
every device; register Extend's Ting types again afterwards (above). A disabled environment's devices keep
their pairs: their sockets close with code 4503 and they reconnect by themselves after `restore`.
A lifecycle instruction that failed leaves a `failed` receipt; retry the identical instruction.
