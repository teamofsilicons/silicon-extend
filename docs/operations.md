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
- Self-destruct deletes a file as the Silicon that made it, with the Briefcase proof Extend keeps
  for that Silicon (its refresh token sealed with `EXTEND_DELEGATION_ENCRYPTION_KEY` in the
  database, so it survives restarts and the Silicon needn't be using Extend). Without a usable
  proof (a file stored before 4.0, or the Silicon signed out of Extend or removed its access) the
  file waits, and is trashed after the Silicon next uses Extend.
  A file's record is deleted only once Briefcase confirms (a 404 counts as gone); a failure is
  logged at `warn` with `file_id`, `tries`, `retry_in_s`, the error and a hint, and retried after
  1 minute, doubling to 1 hour. Up to 100 files per pass, not counting ones still backing off. A
  pending request gets 6 attempts, the first send included; the last marks it `failed` with
  `last_error` and logs `request_failed` on the device. Extend sends every Ting as itself (below),
  so no one's sign-in is needed for a retry.
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
  sent, backing off 30 s, then 1, 2, 4 and 8 minutes, then every 8 minutes. Extend sends every Ting
  as itself (an App verification proof), so no sign-in of anyone involved is needed. Requests give
  up after 6 counted attempts or 24 hours; a Carbon's wake Ting still pending when its request ends
  becomes `failed` ("the request ended before delivery"); woken and declined Tings give up 30 minutes
  after their request ended. A recipient who turned Extend off in Ting stops the retries until "Turn
  on". An `idempotency_conflict` answer counts as delivered. While Ting is off on the server
  (`EXTEND_TING_URL` unset), nothing is sent or retried: requests are recorded `failed` with why and
  stay visible on the website, in the CLI and on the device. Requests stored before Extend 4 (no
  frozen body) can't be sent and are marked failed with why.
- At start, after a rollback to 1.0.0, the service restores the grants the down step set aside
  (unless they were revoked or their pair ended since), logging each as `access_granted` with
  `{"restored_after_rollback": true}`.

## Settings

| Variable | Default | What it does |
|---|---|---|
| `EXTEND_MAX_PAIRS_PER_DEVICE` | 8 | Most Carbons one device may be paired to. A guard on sockets per device, not a product rule; the 9th "Pair with another Carbon" answers `409` with that number. |

Extend 3's membership and test settings (`EXTEND_MEMBERSHIP_SWEEP_HOURS`,
`EXTEND_OWNER_CHECK_CACHE_S`, `EXTEND_OWNER_CHECK_AT_USE`, `EXTEND_TEST_LINK_WINDOW_S`,
`EXTEND_LOCAL_IAM_READERS`) do nothing in Extend 4; the service logs any it finds as ignored.

## Waking a device

A Silicon's wake request is accepted whenever the device isn't known to be awake, including offline.
The limits are the defaults the Carbon accepted, fixed in `extend-protocol`:

- one open request per physical device and Silicon; asking again within 5 minutes is `429`, after
  that it refreshes the request; a request expires 30 minutes after the last ask;
- the device sounds for at most one request every 15 minutes (the others show silently);
- one Carbon Ting per pair every 15 minutes (later asks are "covered");
- at most 6 wake Tings per Carbon per hour: an ask over that is accepted and its Ting deferred, never
  refused;
- a Carbon can turn wake requests off for a device or for one Silicon on it.

Extend never wakes a device. Awake never refuses a command; a command that fails on a device that
isn't awake gets the wake hint.

## Extend's Ting types

Extend sends four notification types through Ting (when `EXTEND_TING_URL` is set):

| Type | Description it is registered with |
|---|---|
| `extend.device.requested` | A Silicon asks to use a device another Silicon is using |
| `extend.device.wake_requested` | A Silicon asks its Carbon to wake a device |
| `extend.device.woken` | A device a Silicon asked to wake is awake |
| `extend.device.wake_declined` | A Carbon turned down a request to wake a device |

Ting registers an app's types; Extend can't. A Ting refused because its type is missing is recorded
(`ting_types`) and shown, never retried silently: in Settings, in `extend ting status`, in the
delivery line of `request send` and `device wake`, and in `GET /api/v2/ting-registration`. It is
retried every 10 minutes, and the next successful Ting of that type clears it. Accounts are enrolled
to receive Extend's notifications with their own User verification proof: a Carbon at their first
pairing or grant, a Silicon at its sessions and wake requests, anyone with "Turn on".

## Logs and signals

`EXTEND_LOG_FORMAT=json`, `EXTEND_LOG=info`. The `credential` frame, credentials and raw hardware ids
are never logged. `connection_replaced` in a pair's activity log means another connection with that
pair's credential took over a live one: on a computer several Carbons paired, suspect a copied
credential (the next rotation, at the end of the next session there, makes the copy useless). Every response has `X-Request-ID`; errors carry it too,
so a CLI error message can be traced to its log lines. Telemetry events arrive at
`/api/v2/telemetry`, are stored in `telemetry` and logged under target `telemetry`, and name the
account by its uuid and current id. They are exported to Space Station when
`EXTEND_SPACE_STATION_KEY` is set; no ingest key has been available, so the export has not been seen
working against Space Station.

## Rate limits

Pairing-code claims are limited per Carbon (5 failed per 10 minutes; the per-address limit
`TECHNICAL.md` proposes was never built), bug reports to 10 per account per hour, and new
enrollments to 60 per hour per client address (read from `X-Forwarded-For` only when the peer is in `EXTEND_TRUSTED_PROXY_CIDRS`),
"Pair with another Carbon" included. 1.1 adds: at most 3 waiting "Pair with another Carbon" codes per
device, one setup retry per device every 5 s, and the wake limits above.
The counters live in the process's memory, so a restart resets them. Several end-to-end runs against one shared development service use up the
enrollment limit within minutes (HTTP 429 `rate_limited`, with a retry time); run test lanes against
their own service instance.

## Revocation

Silicon Accounts' webhooks at `POST /webhooks/accounts` are verified over the exact raw body
(`EXTEND_ACCOUNTS_WEBHOOK_SECRET`, then `EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET` during a rotation;
5 minutes of tolerance), de-duplicated by event id, and applied one at a time per account (an
advisory lock); an event is recorded only after its effects apply, and every effect is idempotent,
so a failure answers 5xx and Silicon Accounts' retry applies it again. Production refuses to start
without the secret. What each event does is in `docs/migration/contracts/TECHNICAL.md` §9 (the
review copy) and `crates/extend-service/src/lifecycle.rs`.

Without an event, a signed-out account is refused on the routes where a sign-out must take effect
at once as soon as Extend's cached introspection answer is gone (at most 30 s); a Silicon's running
session then ends when the event arrives or at its idle timeout.

- **Logout.** `POST /api/v2/auth/logout` by a Carbon (the website's Sign out, `extend logout`) ends
  the running sessions of the Silicons that Carbon gave access to, on that Carbon's side only; by a
  Silicon, its own sessions. When a client could only revoke its sign-in at Silicon Accounts
  directly, the `membership.signed_out` event that follows (reason `app_revoked`) ends the same
  sessions (those started before the logout). Either way the account's other sign-ins stay valid.
- **Custodian changes** end the grants the previous custodian gave the Silicon; other Carbons'
  grants stay, and their devices' logs say the custodian changed.

## Test environments

Extend 4 has none. A request that still sends `X-Testing-Application-Secret` is refused (`401
testing_secret_invalid`); the old `extend_test_*` schemas and `extend_global.test_environments` stay
until a manual cleanup drops them.
