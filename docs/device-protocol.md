# Device protocol

How an Extend app (Android, Android TV, Mac, Windows, Linux) talks to the Extend service. The Rust
types are in `crates/extend-protocol/src/frames.rs` and `model.rs`; this page is the same contract
for apps written in other languages (the Android app is Kotlin). What changed in 1.1.0, and what a
1.0 app still gets, is in section 5.

Service URL: production `https://backend.extend.teamofsilicons.com`, local development
`http://127.0.0.1:8480` (the Android emulator reaches it as `http://10.0.2.2:8480`). WebSocket URLs
are the same host with `ws://` / `wss://`.

Words used below:

- A **pair** is one Carbon's pairing of a device: its own `device_id`, credential, name, access list
  and lifetime. A device several Carbons paired has one pair each (a family TV paired by c:alice and
  c:bob has two).
- The **device engine** is what reads the screen and acts on Mac and Linux computers (and on a Mac
  carrying an iPhone or iPad). Apps name its version `engine_version`.
- A **side** is a Team plus the Carbon who gave access. Two Silicons are on the same side when they
  act in the same Team through the same Carbon's pair.

## 1. Before pairing: enrollment

```
POST /api/v1/enrollments
{"type":"enrollment","data":{"os":"android","os_version":"15","model":"Pixel 9","app_version":"1.1.0"}}

201 {"type":"enrollment","data":{
  "enrollment_id":"0192…","enrollment_secret":"ees_…","pairing_code":"4F9C2A",
  "code_expires_at":"2026-09-26T10:05:00.000Z","rotates_every_s":300}}
```

`os` is one of `android`, `android_tv`, `macos`, `windows`, `linux`. A computer also sends its
device engine's version as `engine_version` (1.0 apps send `agent_device_version`, which is still
read).

An app started for a test environment sends that environment's app secret in
`X-Testing-Application-Secret` when it creates the enrollment; its code then pairs the device only
into that environment (and a code made without the header only into production). Send the same
header, or none, when reading, discarding or connecting to the enrollment; another environment's
secret is `401 testing_secret_invalid`. An unknown, revoked or disabled environment's secret is
`401 testing_secret_invalid`, and one Honeycomb hasn't finished preparing is `503
testing_environment_not_ready`; neither falls back to production.

New enrollments are limited to 60 per hour per network address. A `429 rate_limited` carries
`details.retry_after_s`: wait that long before creating another enrollment (the Android app waits
at least 60 s without it and says so). Never create a new enrollment in a tight loop: pace every
attempt with the socket's backoff (below), and reset the backoff only once an enrollment socket has
actually opened.

Show `pairing_code` large (uppercase, 6 hexadecimal characters). Then open the enrollment socket:

```
GET /api/v1/enrollments/{enrollment_id}/connect   (WebSocket upgrade)
Authorization: Extend-Enrollment <enrollment_secret>
```

Frames from the service (JSON text messages):

```json
{"type":"code","pairing_code":"7B21E0","code_expires_at":"2026-09-26T10:10:00.000Z"}
{"type":"paired","device_id":"7c1e09ab","device_credential":"edc_…","environment":null}
{"type":"ping","nonce":17}
```

Answer every `ping` with `{"type":"pong","nonce":17}`. On `paired`, store `device_credential` in the
OS secret store (Android Keystore-backed storage, macOS Keychain, Windows DPAPI, libsecret), forget the
enrollment, and move to section 2. If the socket drops, reconnect; if the enrollment is gone (401/404),
start a new one. Polling alternative: `GET /api/v1/enrollments/{id}` with the same header returns
`{"state":"waiting",…}` or, exactly once, `{"state":"paired","device_id":…,"device_credential":…}`.

The pair made by this first enrollment is the app's **first pair**: it belongs to the Carbon who
installed Extend on the device. `GET /api/v1/device` says so (`first_pair: true`).

### Pair with another Carbon (1.1)

A paired app can show a new code so that another Carbon pairs the same device to their own account.
Start the enrollment with the credential of any live pair of this device:

```
POST /api/v1/device/enrollments          (no body, or {})
Authorization: Extend-Device <device_credential>

201 {"type":"enrollment","data":{
  "enrollment_id":"0192…","enrollment_secret":"ees_…","pairing_code":"9C04B7",
  "code_expires_at":"2026-09-27T10:05:00.000Z","rotates_every_s":300}}
```

Then follow it exactly as a first enrollment: the enrollment socket (or polling) gives rotating
codes and, once a Carbon enters the code on the website, `paired` with a new `device_id` and a new
credential. Keep every credential; each is a separate pair (section 2).

- The service ties the code to this physical device. The app never names the device itself: holding
  a live credential is the proof. The code pairs only into this device's world (production or its
  test environment), and all pairs of a device are in the same world.
- Before showing the code, warn the Carbon (section 4). A computer gets the shared-computer warning.
- A Carbon who already paired this device gets `409 conflict` when they enter the code ("You already
  paired this device: it's <their name for it> (<id>) in your devices."). The code stays valid for
  someone else until it expires.
- At most 3 such codes wait at once per device (`429 rate_limited`, `details.retry_after_s`), and a
  device can have at most `EXTEND_MAX_PAIRS_PER_DEVICE` pairs (8 by default): the 9th answers
  `409 conflict`, "This device is paired to 8 Carbons, the most Extend allows." Show the message.
- A 1.0 service answers `404`. Say "Extend on the service is too old for this" and offer nothing
  else.
- If every pair of the device ended while the code waited, the claim pairs it as a new device, and
  the app simply gets a working credential.
- A carried device (iPhone, iPad, Apple TV, Samsung or LG TV) has no credential of its own: the
  second Carbon pairs the computer that carries it, then adds the device through their own pair of
  that computer ("Carried devices" below).

## 2. Paired: the device socket

```
GET /api/v1/device/connect   (WebSocket upgrade)
Authorization: Extend-Device <device_credential>
```

Keep it open always. Reconnect with exponential backoff from 1 s to 60 s with full jitter. Close
codes: `4401` credential invalid or pair ended (forget that credential), `4409` superseded by a
newer connection (don't reconnect on your own), `4426` app too old, and `4503` the device's test
environment is closed for now (Honeycomb disabled it, or it is waiting for Honeycomb to confirm
every service is ready; the reason says which, for example "test environment disabled; still
paired, reconnect later"): **keep the credential** and reconnect with the usual backoff. A disable
never unpairs a device, and `restore` brings the same credential back. Treat any other code as a
dropped connection.

While the environment is closed, the upgrade request and every device HTTP route answer
`503 testing_environment_not_ready` instead of 401, with a hint that the device stays paired; keep
the credential and retry with backoff. Only `401` (or close `4401`) means the pair ended. A clean
of the environment does end every pair in it (`unpaired`, then `4401`).

### One connection per pair (1.1)

An app with several pairs keeps **one socket per pair**, each opened with that pair's credential.
Each socket is exactly a 1.0 device connection plus the 1.1 frames, so a 1.0 app is simply the
one-pair case. The rules per connection:

- `hello` and `setup_progress` update that pair. Send the same hello on every pair's connection
  (the service itself withholds the terminal from some pairs of a shared computer, below).
- `unpaired` (and close `4401`) ends that pair only. Delete that credential and keep the others.
  After the last one, show the pairing screen.
- `superseded` ends that pair's connection only. Show "Another connection took over <Carbon>'s pair
  on this device", with **Reconnect**. The service logs `connection_replaced` on that pair when the
  replaced socket had answered a ping within 30 s, so the Carbon sees it in the activity log.
- `refresh` means re-read `GET /api/v1/device` for that pair.
- `session_started` and `session_ended` arrive on the connection of the pair the session runs
  through. The device is in use when any pair's connection says so.
- Commands arrive on the pair's connection. Upload their files with that pair's credential.
- `stop` and `takeover_done` may go on any of the app's connections: the service applies them to
  the physical device, whichever pair the session runs through.
- `awake` may go on any connection; the service applies it to the physical device (section
  "Awake").
- `environment` is the same on every pair.
- A host computer reconciles carried devices per connection: when one pair reconnects, it considers
  only the devices carried through that pair, so one pair reconnecting never ends a session on a
  device carried through another pair.
- `GET /api/v1/device` gives `instance_id`: every credential of one app should name the same one.
  An app that finds two (a restored backup, for example) keeps each connection working and logs it.
  An app that lost all its credentials starts again with a new enrollment; its old pairs go offline
  and expire on their own.

### First frame: hello

```json
{"type":"hello","app_version":"1.1.0","os":"android","os_version":"15","model":"Pixel 9",
 "engine_version":null,
 "features":["setup_retry"],
 "capabilities":["screen.read","screen.capture","input.touch","input.text","nav.system","apps.launch","apps.list","takeover","notifications","links"],
 "missing":[{"capability":"adb","reason":"Wireless debugging is off. Turn it on in Developer options."}],
 "setup":{"state":"needs_carbon","steps":[
   {"key":"accessibility","title":"Allow Silicon Extend to control the screen","status":"done"},
   {"key":"wireless_debugging","title":"Turn on wireless debugging","status":"needs_carbon","help":"Settings › System › Developer options › Wireless debugging"}]}}
```

Send `hello` again whenever capabilities or setup change (or send `{"type":"setup_progress","setup":{…}}`
for setup-only changes). Capability names are in `crates/extend-protocol/src/capability.rs`; a name
the service doesn't know makes a 1.0 service drop the whole hello, so send only those.

- `engine_version`: the device engine's version on a computer (null on Android). 1.0 apps send it as
  `agent_device_version`; the service reads both.
- `features`: what this app can do beyond 1.0, from `extend_protocol::feature`. 1.1 apps send
  `"setup_retry"`. Leave it out, or send `[]`, when there are none. The service sends `setup_retry`
  only to an app that lists it.

### Frames the service sends

```json
{"type":"command","id":"<uuid>","session_id":"a3f","target":null,"command":"click","args":["@e2"],
 "attachments":[],"timeout_ms":30000,"upload_ids":["<uuid>","<uuid>"]}
{"type":"cancel","id":"<uuid>"}
{"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":"2026-09-26T10:00:00.000Z","side":"9f2c4b1a0d3e5f67"}
{"type":"session_ended","target":null,"session_id":"a3f","reason":"idle_timeout"}
{"type":"takeover","target":null,"session_id":"a3f","reason":"Please approve Face ID","expires_at":"…"}
{"type":"takeover_ended","target":null,"session_id":"a3f"}
{"type":"refresh"}
{"type":"environment","environment":{"environment_id":"…","name":"checkout-e2e","state":"ready","paired_devices":1,"device_limit":5}}
{"type":"unpaired","reason":"device_removed"}
{"type":"superseded"}
{"type":"ping","nonce":42}
{"type":"wake_request","target":null,"wake_id":"<uuid>","silicon_id":"si:chef","reason":"Check the order screen","side":"9f2c4b1a0d3e5f67","alert":true,"created_at":"…","expires_at":"…"}
{"type":"wake_request_ended","target":null,"wake_id":"<uuid>","reason":"woken"}
{"type":"setup_retry","target":null,"step":"wireless_debugging"}
{"type":"credential","device_credential":"edc_…"}
```

- `session_started` → show the in-use indicator naming `silicon_id`, with a Stop button. `side`
  (1.1) is the session's side tag: see "Wake requests on the device".
- `session_ended` → clear it.
- `takeover` → show the reason and a **Done** button; Done sends `{"type":"takeover_done"}`.
- `refresh` → re-read `GET /api/v1/device` (name, owner, environment).
- `environment` non-null → show a permanent test-environment banner with its name.
- `unpaired` → forget that pair's credential; with no pair left, return to the pairing screen.
- `attach` / `setup_code` are only sent to host computers (Mac, Windows, Linux).
- `wake_request`, `wake_request_ended` (1.1): see "Wake requests on the device".
- `setup_retry` (1.1): see "Setup steps and Retry".
- `credential` (1.1, computers only): see "Credentials on a computer several Carbons paired".

Ignore a frame type you don't know (log it and carry on): the service adds frames in minor
releases, and a 1.0 app already drops the 1.1 ones this way.

### Frames the device sends

```json
{"type":"result","id":"<same uuid>","ok":true,"output":{…},"text":"Tapped @e2 \"Continue\"","error":null,
 "files":[{"upload_id":"<uuid>","name":"screenshot.png","content_type":"image/png","kind":"screenshot","size_bytes":184223}]}
{"type":"stop"}                               (a host adds "target":"<device_id>" to stop a device it carries)
{"type":"takeover_done"}                      (same optional "target")
{"type":"pong","nonce":42}
{"type":"awake","awake":false,"sleep_state":"screen_off","run":"<uuid>","seq":41}
{"type":"awake","awake":true,"input_seen":true,"run":"<uuid>","seq":42}
{"type":"wake_request_shown","wake_id":"<uuid>","shown":false,"note":"Notifications are off for Silicon Extend on this phone."}
{"type":"credential_saved"}
```

A failed command still answers `result`, with `ok:false` and
`"error":{"code":"…","message":"…"}`. Use `"code":"unsupported_on_device"` for a command this device
can't do, `"code":"invalid_args"` for arguments it can't parse, and a precise message either way.

A command that is stopped still answers, unless the socket itself is gone:

| Why it stopped | `error.code` |
|---|---|
| The service sent `cancel` for it | `cancelled` |
| The Carbon pressed Stop on the device, or `session_ended` arrived, while it ran or waited | `session_ended` |
| It arrived for a session this device already ended (Mac, Windows and Linux app) | `session_ended`, without running it |
| Android: the Carbon disconnected Android debugging (only `adb`, `install`, `reinstall`, `record`, `logs`) | `device_not_ready` |
| A shell command exited non-zero (`terminal`, `adb shell`) | `command_failed`, with `details.exit_code` and the output |
| The device engine refused or failed it (Mac and Linux app) | the engine's error mapped to Extend's code, with the engine's own code in `details.engine_code` and its own details in `details.engine_details` (1.0 apps sent them as `agent_device_code` and `agent_device`) |
| A file it made could not be uploaded | `upload_failed` |

The service may already have answered the caller (for example `command_timeout`); a late `result`
is then dropped. Keep each `result` frame well under the socket's 16 MiB message limit: the Android
app replaces a result larger than 15 MiB with an `action_failed` result and puts long output in
files instead.

### Setup steps and Retry (1.1)

Each setup step has a `key`, a `title`, a `status` (`todo`, `in_progress`, `needs_carbon`, `done`,
`failed`), and optionally `help` (where to find it on this device), `input` (`"code"` for the PIN an
Apple TV shows) and `error`.

**`error` is for the Carbon.** One or two sentences: what is wrong and what to do. For example "The
iPhone is locked or not connected by cable. Unlock it and keep it plugged in." Never environment
variables, file paths, build or install commands, exit codes or stack traces: those go to the app's
log.

**Retry.** When a step failed, the Carbon can retry it: with **Retry** on the website, with
`extend device setup <id> --retry [--step <key>]`, or (on Android) with the Retry button on the
app's own setup screen. The service checks the request and sends:

```json
{"type":"setup_retry","target":null,"step":"wireless_debugging"}
```

- `step` names the step to run again; null (or absent) means every failed step.
- `target` is null for the app's own steps, or the id of a device this computer carries.
- Run the step again at once, then report as usual: `setup_progress` (or `hello`) for the app's own
  steps, `attached` for a carried device. Report the step `in_progress` while it runs, then `done`,
  or `failed` with a new `error`.
- If the step is no longer failed when the frame arrives (the Carbon fixed it on the device
  meanwhile), just report the setup as it is.
- The service only sends `setup_retry` to an app whose hello lists `"setup_retry"`. It changes no
  step itself; the website and CLI show progress from what the app reports. For an older app the
  caller gets `426 upgrade_required`: "<name> runs Silicon Extend <version>, which can't retry from
  here. Update it to 1.1, or tap Retry on the device."
- Retries are limited to one per device every 5 seconds (`429 rate_limited`).
- The Android app's steps that can fail are `wireless_debugging` or `network_debugging` (Android
  debugging the Carbon connected that the app can't reconnect to) and `accessibility` (turned on
  but never started by Android). It ignores a `setup_retry`, `wake_request` or `session_started`
  that carries a `target`, since it carries no devices, never sends `engine_version` (it runs no
  device engine) and ignores `credential` frames (its pairs are never rotated).

### Awake (1.1)

Awake means the device is ready for its screen to be used: a phone with its screen on and unlocked,
a computer awake and unlocked in the signed-in account, a TV on. Awake is information and the start
of a wake request. It never gates anything: the terminal, Android debugging and every other command
keep working while a device isn't awake, and a command that needs the screen fails with the
device's own error (the service adds how to ask the Carbon to wake it).

```json
{"type":"awake","awake":false,"sleep_state":"screen_off","run":"<uuid>","seq":41}
{"type":"awake","awake":true,"input_seen":true,"run":"<uuid>","seq":42}
```

- Send it right after every `hello`, on every change, on each pair's connection.
- `sleep_state` says why it isn't awake: `screen_off`, `locked`, `asleep`, `standby` or
  `other_session` (a computer showing another account). Leave it out when awake.
- `input_seen`: `true` when the device saw an unlock or real input with this change; `false` when it
  woke with no such sign; left out when it can't tell. After a wake with `false`, send `awake: true`
  again with `input_seen: true` at the first unlock or input. Only an awake that isn't
  `input_seen: false` answers wake requests, so a phone that lights up for a notification doesn't
  tell a Silicon "it's awake".
- `run` is a random UUID for each app process; `seq` increases across all the app's connections in
  that run. The service applies a frame when its `run` differs from the last one it applied (a new
  app process), or the `run` is the same and `seq` is larger, or it has no `run`. Stale frames and
  the same frame from sibling connections change nothing.
- When a socket connects and no other pair's socket of the device is connected, the service
  forgets the awake state until the app reports again.

What each app reports:

| Device | Awake | Not awake | `input_seen` |
|---|---|---|---|
| Android phone and tablet | After `USER_PRESENT` following the last screen-off, or at connect when the screen is on and the keyguard is not locked | `screen_off`; `locked` when the screen is on behind the lock screen | `true` on `USER_PRESENT` |
| Android TV, Google TV, Fire OS | The TV is on (a screensaver counts as awake) | `standby` | left out |
| Mac | Awake and unlocked in the signed-in account | `asleep`, `locked`, `other_session` | an unlock, or input in the last 10 s (HID idle time) |
| Windows | Unlocked (from the session's lock state; an admin prompt's secure desktop counts as awake) | `locked`, `other_session` | an unlock, or recent input (`GetLastInputInfo`) |
| Linux | The session is active and unlocked (logind); a computer without a screen is always awake | `locked`; `other_session` when the session isn't active | an unlock, or logind's idle hint clearing |
| Apple TV (carried) | Its attention state is awake | `standby` | left out |
| Samsung TV (carried) | On | `standby` (online, not awake) | left out |
| LG TV (carried) | Its power state is active | `screen_off` or `standby` | left out |
| iPhone, iPad (carried) | Not reported: Extend can't read it yet, so the service marks wake requests for them "can't tell" (`wake_detectable: false`) and the Carbon answers them | | |

A host reports a carried device's state in its `attached` frame (`awake`, `sleep_state`), with
`input_seen` left out.

**Extend never wakes a device.** No app turns a screen on, takes a wake lock that turns the screen
on, or sends a power-on key: the Apple TV's power button sends sleep only while it is awake and is
refused while it is asleep ("The Apple TV is asleep; only its Carbon can wake it (extend device
wake)"), a Samsung TV in standby gets no power key, Android TV's power key stays refused, and LG
power only turns the TV off.

### Wake requests on the device (1.1)

A Silicon asks its Carbon to wake a device with `extend device wake <id> --reason "..."`. The
service tells the Carbon through Ting and sends the device:

```json
{"type":"wake_request","target":null,"wake_id":"<uuid>","silicon_id":"si:chef","reason":"Check the order screen",
 "side":"9f2c4b1a0d3e5f67","alert":true,"created_at":"…","expires_at":"…"}
```

- It goes on the connection of the pair the Silicon asked through. For a carried device it goes to
  the host with `target`, and the host shows nothing: it checks that device every 5 s until the
  request ends, so it can report it awake quickly.
- It never names a Team or a Carbon. `side` is the request's side tag.
- **Redaction.** While the device (or anything in its lock group: a computer and the devices it
  carries) is used by a Silicon on another side than the request's, the service leaves out
  `silicon_id` and `reason`. Show "A Silicon asked to use this device; its Carbon was told through
  Ting" then. A frame without them replaces what you hold for that `wake_id`. The service sends the
  frame again on connect, on every `session_started` and `session_ended` in the lock group, and on
  each new ask, so the redaction follows who is using the device.
- **Redact before the first command.** On `session_started` with side `S`, before you run any
  command of that session, treat every wake request whose `side` isn't `S` (or has no side) as
  redacted: in memory, on your screen and in the OS notification. `session_started` arrives on the
  same connection as the session's commands, and before them, so this holds even when the service's
  redacted frames arrive later on other connections. Re-post OS notifications redacted, or remove
  them: Android notifies again with the same id; macOS removes the delivered notification and adds
  it again; Windows removes the toast from its history (tag and group) and shows it again; Linux
  sends `Notify` with `replaces_id`.
- **Show one notification per device**, listing every open request from all connections ("si:chef
  and 1 more"). `alert: true` means sound or vibrate for this one; the service allows that at most
  every 15 minutes per device, so show the others silently. On Android the lock screen shows only
  the Silicon's name unless the phone shows notification contents there (a private notification
  with a name-only public version). Show the reason as plain text only: escaped for Windows toasts
  (XML) and Linux notifications (markup), passed as an argument, never through a shell. Never use a
  full-screen intent or anything that turns the screen on.
- Answer `wake_request_shown` once per `wake_id`: `shown: true`, or `shown: false` with a `note` of
  at most 300 characters ("Notifications are off for Silicon Extend on this phone."; an Android TV
  answers "This TV can't show notifications; its Carbon was told through Ting.").
- Remove a request on `wake_request_ended` (whatever its `reason`: `woken`, `expired`, `withdrawn`,
  `declined`, or one you don't know), at `expires_at`, or when you report yourself awake.
- Keep wake requests in memory only: never in a status file or a log line. Log the `wake_id` if
  anything.

### Keeping the screen on (1.1)

While a Silicon uses a device that is awake, the app keeps the screen from turning off on its own,
so the Silicon doesn't lose it mid-task. It never turns a screen on, and the Carbon can still lock
the device. Hold it only while both are true (a session is running on any pair, and the device
reports itself awake), and release it on session end, Stop, a takeover, idle end, or when the
device stops being awake.

| Device | How |
|---|---|
| Android TV | `FLAG_KEEP_SCREEN_ON` on the in-use badge overlay |
| Android phone and tablet | A 0×0, not touchable, not focusable accessibility overlay with `FLAG_KEEP_SCREEN_ON` |
| Mac | An IOKit power assertion that prevents idle display sleep |
| Windows | `SetThreadExecutionState(ES_CONTINUOUS \| ES_DISPLAY_REQUIRED)`, held by one long-lived thread (the state belongs to the calling thread) |
| Linux | `org.freedesktop.ScreenSaver.Inhibit` on a D-Bus connection that lives as long as the session, falling back to the desktop portal's idle inhibit |

### Credentials on a computer several Carbons paired (1.1)

On a Mac, Windows or Linux computer, a Silicon's terminal runs as the computer's own account, which
can read what the app stores, including other pairs' credentials. So at the end of every session on
a computer whose live pairs belong to two or more Carbons (and whose app is 1.1 or later), the
service replaces each pair's credential over that pair's own connection:

```json
{"type":"credential","device_credential":"edc_…"}
```

Store the new credential in place of that pair's old one, then answer `{"type":"credential_saved"}`
on the same connection. The old credential keeps working until then (or until a connect with the
new one), so a crash in between loses nothing; keep the old one until the answer is sent. If the
pair isn't connected, the frame waits for its next connect. Never log either frame or the
credential. A copy of a credential taken during a session stops working once the app confirms.

Android pairs are never rotated: their credentials are sealed with the app's Android Keystore key
in app-private storage, which Android debugging can't read.

### The terminal on a computer several Carbons paired (1.1)

Only Silicons given access by the Carbon who installed Extend on the computer (its first pair) use
the terminal there. On a computer whose live pairs belong to two or more Carbons, every other pair
lists `terminal` in `missing` instead of in `capabilities`, with the reason:

"Several Carbons paired this computer. Only Silicons given access by the Carbon who installed
Silicon Extend on it can use its terminal. The screen, keyboard and apps work as usual."

The service applies this whatever the hello says: it removes `terminal` from those pairs'
capabilities, lists it in their `missing` with that reason, and refuses `terminal` commands through
them (`unsupported_on_device`). An app can tell which of its pairs is the first from `first_pair` in
`GET /api/v1/device`, to show the same on its own screen. When only one Carbon's pair
is left, that Carbon's Silicons get the terminal again. When the first pair itself has ended and
several Carbons remain, no pair has the terminal. This limits Extend's `terminal` command; a Silicon
that can use the screen and keyboard can still open a terminal app the computer has, which is why
the warning before pairing stays (section 4).

### Session process containment (1.1)

Every process a session's terminal starts ends with the session (`session_ended`, Stop, a pair
ending), whether or not the computer has several Carbons:

- Mac and Linux: each terminal command runs with `EXTEND_SESSION_MARK=<random 128-bit hex per
  session>`. At session end the app kills every process of this OS user that carries the mark
  (Linux reads `/proc/<pid>/environ`, macOS uses `sysctl KERN_PROCARGS2`), with `SIGKILL` to each
  match's process group, so `setsid` and `nohup` don't escape. macOS hides the environment of
  Apple's own programs (`sleep`, `perl`, the shells) even from the same user, so on a Mac the app
  also ends each command's process group and the processes forked from it. One case still escapes
  there: an Apple program that leaves its process group (`setsid`) in the instant its parent exits.
- Windows: one Job Object per session with kill-on-close and no breakaway. Each command starts
  suspended, joins the job, then resumes; closing the job at session end ends everything in it,
  `start` included.
- Not containable: jobs handed to the operating system's own schedulers (`launchd`, `schtasks`,
  `systemd-run`, `cron`, `at`). The shared-computer warning covers them.
- The app also wipes each command's work directory and a session's recordings at its end.

### Carried devices (iPhone, iPad, Apple TV, Samsung and LG TVs)

A computer carries devices the Carbon added through it. The service sends `attach` (and `attach`
with `removed: true`) on the connection of the Carbon's own pair of the computer, for that Carbon's
own carried devices; the host sets each one up and reports it with `attached`:

```json
{"type":"attached","device_id":"3a2b0c1d","online":true,"os_version":"18.0","model":"AppleTV14,1",
 "capabilities":["screen.capture","input.remote","nav.system","apps.launch","apps.list"],"missing":[],
 "setup":{"state":"complete","steps":[]},"awake":false,"sleep_state":"standby","hardware_key":"5f3a…"}
```

- `awake` and `sleep_state` (1.1): as in "Awake"; leave them out when the host can't tell.
- `hardware_key` (1.1): `hex(HMAC-SHA256(hardware_salt, driver + ":" + stable hardware id))`, with
  `hardware_salt` from `GET /api/v1/device` (computer pairs only; the same for every computer in
  the world). The stable ids are the iOS UDID, the Apple TV's Companion identifier, the Samsung TV's
  id (DUID) and the LG TV's device id or MAC address. Send no `hardware_key` until you have the salt
  (a 1.0 service gives none). The key is a pseudonym, not a secret: never send the raw id.
- **Linking across Carbons.** When c:bob pairs the family Mac (Pair with another Carbon) and adds
  the Apple TV that c:alice already carries through it, the host reports the same `hardware_key` on
  both pairs' connections, and the service links bob's pair of the TV to the same physical device.
  The host drives one physical device with one driver and one set of pairing and state, so the
  second Carbon enters no second TV code where the driver can identify the device before setup.
- **Duplicates are refused**, naming no other Carbon or computer:
  - the same Carbon added it twice: the later one reads not ready, with the step "This TV is already
    added as <name> (<id>) through <host name>; remove one of them.";
  - it is carried by another computer: "This TV is already added to Extend through another computer.
    Add it through that computer instead: on its Extend app choose Pair with another Carbon, then add
    the TV there." Only one computer carries a device, so one Silicon at a time holds.
- A computer and the devices it carries form one **lock group**: one side uses it at a time, so a
  terminal on the computer can't watch another side's iPhone or TV session.
- **Stop.** The Stop in the computer's own app stops everything on the computer and on every device
  it carries, whichever Carbon gave access. From the website or CLI, a carried device can be
  stopped only by a Carbon who paired it.
- A retry of a carried device's setup arrives as `setup_retry` with `target` (see "Setup steps and
  Retry"); answer with `attached`.

### Keeping a session through a dropped socket

The service pings every 15 s and marks the device offline after 45 s without a pong. It keeps a
session for 120 s after that and re-sends `session_started` when the device reconnects in time, so
the longest it can keep a session after losing contact is about 183 s (45 + 15 + 120 + its 2-second
scheduler pass). A device app should therefore keep a session's state (recordings, log captures,
snapshot refs) across a dropped socket, and drop it only on `session_ended`, the Carbon's Stop,
unpairing, a `GET /api/v1/device` after reconnecting that shows another session or none, or its own
grace period running out; the Android app waits 240 s (`SessionRetention.GRACE_MS`, checked against
`crates/extend-protocol/src/lib.rs` by a unit test). Commands that were running when the socket
dropped get no `result` from the Android app; the service answers the caller `device_offline`.

### Commands

`command` is the top-level name and `args` the remaining command-line tokens, exactly as the device
engine's command line takes them (see `understanding/cli.yaml`, `device_commands`). The list of
names is `COMMANDS` in `capability.rs`. Refs like `@e2` come from the latest `snapshot` in the same
session.

### Attachments (files the caller sends with a command)

A command may carry `attachments: [{"name","content_type","content_base64"}]` (a replay script, an
APK to install, an image or video to show on a TV): at most 8, and 8 MiB decoded in total. Write each one into the command's scratch
directory, then replace every argument of the form `attachment:<name>` with that file's local path
before running the command. Example: `display show --image attachment:cat.png` with an attachment
named `cat.png`.

### Files

For each file a command produces, upload it before sending the `result`, using the next unused id
from `upload_ids` and the credential of the pair the command came on:

```
PUT /api/v1/device/artifacts/{upload_id}
Authorization: Extend-Device <device_credential>
Content-Type: image/png
X-Content-SHA256: <64 lowercase hex of the bytes>
X-File-Name: screenshot.png
<bytes>
```

`X-File-Name` must be printable ASCII without slashes (HTTP headers can't carry more); replace
other characters there and put the real name, in any script, in `result.files[].name`. Then list
the file in `result.files`. Kinds: `screenshot`, `recording`, `log`, `replay_script`, `diff`, `other`.

## 3. Other device endpoints

```
GET    /api/v1/device              → {"type":"device_self","data":{device_id,name,owner:{type,id},team,os,in_use,takeover,setup,environment,
                                                                  instance_id,hardware_salt,first_pair}}
DELETE /api/v1/device              → 204. "Revoke pair" for this pair's Carbon (confirm with the Carbon first).
POST   /api/v1/device/stop         → 204. Same as the stop frame, for when the socket is down.
POST   /api/v1/device/enrollments  → 201 enrollment. "Pair with another Carbon" (section 1).
```

All with `Authorization: Extend-Device <device_credential>` of one pair.

- `team` is the Team the Carbon had selected when pairing. It authorizes nothing; show it nowhere.
- `in_use` is set only when the session runs through this pair. The device is in use when any of
  its pairs says so, or when `session_started` said so on any connection.
- `instance_id` (1.1): the physical device; every pair of it shares one.
- `hardware_salt` (1.1): computer pairs only; the key for `hardware_key`.
- `first_pair` (1.1): true for the pair made by the app's first enrollment.
- `DELETE` ends this pair only: its sessions, its access list and its wake requests. The other
  Carbons' pairs are untouched. Forget that credential and keep the others.
- `stop` (and the `stop` frame on any connection) ends the session on the physical device, whichever
  pair it runs through, and on every device the computer carries.

## 4. What every app shows

1. Unpaired: the pairing code, large, and never a login.
2. During setup: each setup step with where to find it on this device. A failed step shows its
   plain `error` and a **Retry** button (the Android app; on computers the website's Retry reaches
   the app).
3. Paired: the device's name for each Carbon it is paired to ("Paired to c:alice (Living room TV) ·
   c:bob (Family TV)"), and whether a Silicon is using it.
4. In use: which Silicon, and through which Carbon ("si:chef, through c:alice"), with Stop. Stop
   ends it whichever Carbon gave access.
5. Wake requests: the notification described above, wherever the device can show one, and the same
   list (with the same redaction) on the app's own screen.
6. **Pair with another Carbon.** First a warning, then the code (large on a TV), with Cancel:
   - on any device: "Silicons any Carbon gives access to can use this whole device, including what
     others leave on it."
   - on a computer: "Only Silicons given access by the Carbon who installed Silicon Extend here
     (<first Carbon>) use this computer's terminal. It runs as <os user> and can reach what
     <os user> can, including this app's other pairs. Silicons other Carbons give access to use its
     screen, keyboard and apps, as <os user> too. Share a computer only with Carbons you trust."
7. **Revoke pair**, for one Carbon at a time, with a confirmation naming that Carbon. It removes the
   device from that Carbon's account only.
8. For each pair whose connection was taken over: "Another connection took over <Carbon>'s pair on
   this device", with Reconnect.
9. A test-environment banner with its name whenever `environment` is set.

It starts when the device starts and keeps a socket open for every pair.

## 5. Versions

**A 1.0 app with a 1.1 service** has one pair and one connection. It never sends `awake`, so its
device reads "unknown" (never gated). It drops the 1.1 frames as unreadable (so wake requests read
"unsupported" on the device and reach the Carbon through Ting only), ignores `session_started.side`,
is never rotated (a 1.0 app has one Carbon), can't retry setup from the website (`426`), and can't
offer Pair with another Carbon. It keeps sending `agent_device_version`, which the service reads.

**A 1.1 app with a 1.0 service** gets its 1.1 frames logged as unreadable, `404` from
`POST /api/v1/device/enrollments`, and no `hardware_salt`, `instance_id` or `first_pair` (so no
`hardware_key` and no linking). Deploy the service first.

No capability, end reason or error code was added in 1.1: a 1.0 reader refuses values it doesn't
know (an unknown capability makes a 1.0 service drop the whole hello).

### In-use indicator (1.1 follow-up)

`GET /api/v1/device` and each carried-device `attach` frame now carry `in_use_indicator`:
`shown` (the default when absent) or `hidden`. `PATCH /api/v1/device` with a device credential
accepts `{"type":"device_self","data":{"in_use_indicator":"hidden"}}` and returns `device_self`.
Any live pair's credential can change its own physical device's setting. Every pair reads the
same value. The service sends `refresh` to the device's live pair connections; for carried devices,
it sends `attach` with the new setting to their hosts. A banner-only refresh must preserve a live
driver and its session/recording state. Carbons can also set it through the existing device PATCH;
Silicons cannot. Both APIs are covered by the client contract fixtures.

Android and desktop banner timers use a 10-second deadline without ending the session or the
screen hold. Hidden suppresses in-use announcements and desktop icon changes. Takeover requests
still show until answered; Stop remains in the app and website. The Android TV badge is positioned
at bottom centre. Test-environment disclosure is separate from this preference.
