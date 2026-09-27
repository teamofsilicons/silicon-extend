# Verification record

What was run to check that Silicon Extend works as `understanding/UNDERSTANDING.md` intends, on what,
and what has **not** been verified. Newest first: the 1.1.0 integration section, the 2026-09-27
round-2 section, the earlier 2026-09-27 section, then the 2026-09-26 record, corrected in place where
later work showed it wrong (marked *Corrected 2026-09-27*). Rerun the automated part with
`e2e/run-all.sh`. The device engine is named as it is from 1.1 (it lived in another directory when
the 1.0 checks ran). Only the 1.1.0 section covers 1.1.

## Where the evidence is

- **Nothing here is committed evidence.** Each result was observed by the agent that ran it; its
  logs, videos and screenshots stayed on this Mac, outside version control. The record itself is
  the only durable account.
- **Gone.** The 2026-09-26 record cites logs under `/tmp/extend-*.log` and ignored folders under
  `target/`. On 2026-09-27 no `/tmp/extend-*` file exists any more, and neither do
  `target/desktop/macos/`, `target/desktop/linux/verification.json`,
  `target/android-recording/service-ghqrk9z3/` or the other folders marked *gone* below. Those
  claims can be checked only by rerunning their lanes.
- **Kept on this Mac on 2026-09-27**, all ignored by git, so they survive a reboot but not
  `cargo clean` or a fresh checkout:
  - `target/desktop/linux-recording/`: `cover-recording-*`, `isolated-recording-*`,
    `public-app-recording-*`, `runtime-recording-*`, and `service-recording-6snwvqkw/` and
    `service-recording-nyiexg15/`, each with `summary.txt` and the package install logs;
  - `target/android-recording/run-*/`: duration-limit proof videos and logs;
  - `web/test-results/restyle/`: website screenshots;
  - `apps/android/build-screens/`: Android phone and TV screenshots;
  - `e2e/real-iam/last-run/`: Briefcase, Ting and Extend logs of the last real-services run.
- The agents' own scripts, logs and scratch builds from 2026-09-26/27 were in a temporary session
  directory and are not kept.
- Lanes that now write their evidence under `target/`: `apps/desktop/linux-e2e/record-service-e2e.py`
  (`summary.txt`), `RECORD_LANE=record-hung-e2e.py` (`cover-recording-*`), `e2e/android-recording.sh`
  and `e2e/android-recording-service.py`.

## 2026-09-28 — native Mac banner and focus preservation

The isolated `apps/desktop/macos/banner-native-e2e.py` fixture runs the production UI with a fake
agent and a process-local AppKit probe. It exposed a real focus bug: Tao's `set_visible(true)`
made the banner key on every show despite `focused=false`. Banner display now uses AppKit
`orderFront` on macOS; a sentinel window keeps focus when a session announces itself.

The rebuilt fixture passed 13 native scenarios: expanded/collapsed sizes, restored position,
screen-edge clamping, new-session expansion, hide/reappear, ten-second timeout and focus retention.
Three Stop actions reached the correct fake targets. CUA clicks also verified collapse, Stop and
restore with the native frame retained at (160,220). Rust UI tests passed 17/17, DOM tests 2/2,
strict Clippy and Python compilation passed. Owned fixture processes exited; installed apps,
TCC, real sessions and unrelated windows were untouched. Evidence: `target/desktop/banner-native-e2e/`.

Physical dragging remains unverified: CUA's app-targeted drag could move neither the banner nor
the fixture's standard AppKit title bar, and macOS reported no pressed mouse button. This
establishes a tool limitation, not a banner drag defect. Multi-monitor movement, Windows/Linux
native behavior and metadata refresh during recording remain separate checks.

## 2026-09-28 — released 1.0 CLI against service 1.1

The installed, released 1.0.0 CLI (SHA256
`0f450055fb96ffe5c45f9f2700c1b55c4cb394198eaaff6c1330ccadfa2038fd`) passed the new isolated
`e2e/released-cli-compat.py` lane against the current 1.1 service. It covers negotiation, login,
pairing/grants, additive device-response decoding, sessions, commands, screenshots and downloads.
Switching between actual 1.0 and current CLI binaries preserves the same saved login and connected
session. Mixed-version takeover/release and Carbon Stop work; the old CLI can resume a session
saved by 1.1, end it and remove the device. The current full CLI suite then passed all 119 checks
on the same owned service. Eight lane groups passed; logs/report are in `target/released-cli-compat/`.

The fixture used local IAM/file/Ting implementations and a scripted 1.0 device, not an actual 1.0
Android/desktop app or website. The owned database/processes were removed. The existing service on
8480 and installed user logins were untouched. Actual device-app and website upgrade rehearsal
remains open.

## 2026-09-28 — repeated rollback and old-service banner compatibility

Four migration tests pass on owned local PostgreSQL databases. The rehearsal runs three down/up
cycles across production-shaped schema5 and schema4/schema5 test worlds, repeating each step twice.
Instance identities, first-pair markers, salts, confirmed credentials, hidden-banner choices and
grant metadata survive. Removed and revoked grants stay absent, and 1.0-style writes still work.
A controlled overlapping-startup test reproduced a missing rollback-stash relation; restoration
now holds the migration advisory lock and rechecks the catalog inside its transaction. Both
concurrent restores pass and restore/log each grant once. Owned databases are cleaned on success
and assertion failure; retained logs were checked for leftovers.

The desktop's internal service reader now distinguishes an omitted old-service banner field from
an explicit `shown`. A real HTTP/WebSocket regression covers synchronized hidden choices for the
computer and a carried device, old refresh/attach responses, app restart and reconnect, then an
explicit 1.1 change back to shown. Public protocol defaults and the public client API are unchanged.
The full agent suite passes 231 tests with one existing ignored; agent/service Clippy with warnings
denied, formatting and diff checks pass. Evidence: `target/rollback-verification/`.

This is synthetic database and simulated old-service proof. The production-schema-copy and actual
1.0 application rehearsal remain separate release gates.

## 2026-09-28 — native TV display through real Briefcase

The repeatable `e2e/real-iam/native_display.py` lane passed 12 checks in 27.24 seconds with local
real IAM, Briefcase and MinIO and the installed debug app on dedicated Android TV emulator-5640.
It stored a native screenshot as `si:chef`, verified its Briefcase ownership and Carbon share,
then displayed it by `file:<UUID>`, bare UUID and private Briefcase URL. Before each replay the TV
showed a distinct text screen; all four sampled color quadrants then matched exactly at 1920×1080.
Missing files and another Silicon's private-file access were refused. Damaged image bytes reached
the native decoder and returned `action_failed`; the next stored-image display recovered.

Evidence is in `target/real-briefcase-native-tv/`: `report.json`, before/after PNGs, command results,
service logs, logcat and a memory dump. Owned fixture services were removed; the dedicated emulator
was left unpaired at its prior debug URL. Physical devices and production were untouched. The
Carbon signs into Briefcase first to satisfy its existing recipient-projection limitation. This
closes the real-service/native-emulator handoff gate, not the physical-TV or production gate.

## 2026-09-28 — real IAM, Briefcase and Ting 1.1 checks

- The combined local real IAM + Briefcase + Ting run passed 75 checks with zero failures. A fresh
  IAM + Ting run after the registration fix passed 62 with zero failures. These use the services'
  actual implementations in owned fixtures, not production accounts or deployment.
- Verified holder versus Carbon request routing, all wake events in acme/globex, a third Team
  using globally registered app types, and genuine missing-type refusals after removing the types
  only in the stopped owned fixture. Ting accepted self-send with HTTP 202. The Silicon's directory
  lookup of c:bob returned 200, then 404 after real IAM removal; its own lookup stayed 200. In the
  combined lane c:bob owned a second device granted to that Silicon, so this was a granting Carbon.
- Ting 0.1.9 resolves types by context and app, independently of delivery Team, and its OBO catalog
  has no `types.register`. Removed Extend's unsupported calls and corrected service, CLI and web
  guidance to use the app's owning Team (an explicit quoted placeholder when unknown). Recipient
  "Turn on" does not falsely clear missing-type errors. No additional OBO scope is required.
- Focused validation: 121 Rust tests, 170 web unit tests, two Chromium cases, TypeScript, Clippy with
  warnings denied and formatting passed. All fixture processes and containers were cleaned up.
- Evidence: `target/realiam-1.1-verification/pre-guidance/` (combined run) and `final/` (fresh run),
  including report JSON, service logs and focused check logs. Briefcase's unseen-recipient
  projection remains a recorded dependency gap; the native-TV stored-file handoff is a separate
  check. Production `tos` still needs the three wake types registered by its app manager.

## 2026-09-28 — desktop carried banners and offline choices

Per-carried-device switches, atomic offline preference persistence and delayed-response fencing
are implemented. Known aliases of one carried device update together and use the correct host
credential. Setting synchronization cannot block Stop. Restarting the app no longer announces an
old session for another ten seconds. The new carried-settings endpoint authenticates the exact
carrying pair and rechecks that relationship under instance locks before changing shared settings.

- Agent: 230 passing tests (204 unit, 26 integration), one existing ignored. Tests cover offline
  save/restart, slow HTTP versus Stop, remote refresh, two-Carbon aliases and retained live drivers.
- Service indicator suite: five passing tests, covering authorization, world isolation, invalid
  inputs, removed/unrelated targets and shared settings.
- Actual desktop HTML in headless Chromium: two passing tests, covering carried controls, offline
  messaging, drag IPC, collapse/restore, Stop, takeover Done and escaped names. Clippy and formatting
  pass. Evidence: `target/desktop-banner-verification/extend-desktop-banner-*.log`.
- These do not prove actual native window movement, multi-monitor position retention or a banner
  change during a real native recording. Installed apps and production were not changed.

## 2026-09-28 — direct Android TV element clicks

Android TV apps 1.1+ can receive direct `click` through their existing `screen.read` accessibility
capability. The service's command list and command gate, Android's local gate and CLI help now
agree. The TV's advertised capabilities are unchanged: pointer/touch/hover/gesture support is not
implied. Apps 1.0.0/1.0.2 retain their prior direct-click refusal and `find … click` fallback.

- Two HTTP/WebSocket tests pass, including decoding the actual response with frozen 1.0 models,
  refusing old TV apps and withdrawing click when accessibility disappears during a session.
- Protocol 32 tests, CLI 28 existing tests plus one help regression, and 50 focused Android tests
  passed; scoped Clippy with warnings denied passed. The subsequent full Android app suite has
  235 passing tests, with lint and APK build passing.
- Native Android TV 14 emulator-5640: `click @e8` opened Device Preferences using
  `accessibility_click`; the next snapshot confirmed the destination. Hover, gesture and scroll
  remained refused. Coordinate, repeated and held clicks still depend on gesture injection;
  the native proof here is an element click, not those variants or a physical-TV check.
- Evidence: `target/tv-click-verification/{native-click.json,service-tests.log,android-tests.log,android-final-build.log}`.

## 2026-09-28 — lower screenshot memory on Android TVs

Plain ADB screenshots now stream Android's original PNG from disk, reading only the dimensions
instead of decoding and recompressing the full TV frame. Cropping, scaling and reference overlays
still render at the requested resolution; each replaced bitmap is recycled, and the final image
is encoded to disk and recycled before upload. Uploads use the existing cancellable file stream,
so PNG compression no longer creates a growing byte buffer plus a second full byte array. The
accessibility capture releases its hardware bitmap wrapper after copying and recycles screenshots
that arrive after command cancellation. Allocation failures on Android's callback thread are
forwarded to the command's error handler. Screenshot scratch directories are deleted in `finally`.

- Android TV 14 emulator-5640, five sequential 1920x1080 screenshots through the fake service:
  sampled PSS before the change ranged 54,648–64,949 KiB, after 56,143–58,827 KiB. Both measurements
  began after a background-only app restart. This is one emulator debug-build workload, not a
  physical-TV baseline or a claim about all app memory. The actual captured resolution remained
  1920x1080 even with a logical 3840x2160 `wm` override.
- Real ADB PNG uploads and accessibility plain, 0.5-scale and annotated 0.5-scale uploads all
  succeed and decode completely with ffmpeg. A missing crop selector still returns
  `element_not_found`. No screenshot scratch directories remain after success or that failure.
  Android 9 emulator-5642 also passed real ADB plain and 0.5-scale captures and complete PNG decoding.
- Android app unit tests, lint and debug build pass. The final accessibility-wrapper change was
  checked again with native plain, scaled and annotated captures. Final app unit total: 235.
  Review also corrected reference-overlay coordinates after cropping. The native 74x38 crop of
  Settings' About label at screen position (1328,210) contains zero annotation-colour pixels without
  overlays and 2,149 with overlays, proving the reference is drawn within the cropped image.
- Evidence: `target/adb-reconnect-verification/screenshot-{before,after,variants,final-native,legacy-native,crop-native}.json`,
  `screenshot-final-build.log` and the PNGs under `service/`. Existing file-upload tests cover the
  streamed checksum/bytes and cancellation behavior. Further TV memory profiling and physical
  device verification remain open; no screenshot resolution was reduced by default.

## 2026-09-28 — Android debugging recovery after process death

The existing reconnect implementation passed on two fresh, isolated emulators. After the one-time
debugging authorization and pairing with an isolated fake service, the lane sends the app Home,
kills its exact PID with `run-as`, and waits for a real remote `adb shell id -u` result of 2000.
It does not reopen the activity or press Connect. The foreground service and debugging connection
return automatically. Restarting that emulator's adbd also restores commands automatically.

| Emulator | Process death | adbd restart | Background PSS after process death |
| --- | --- | --- | --- |
| Android TV 14, API 34, `ExtendReconnectVerification`, emulator-5640 | 2.111 s | 14.284 s | 55,967 KiB |
| Android 9 Google APIs, API 28, TV override, `ExtendReconnectLegacyVerification`, emulator-5642 | 2.119 s | 14.337 s | 29,622 KiB |

API 28's emulator adbd exposes only its emulator pipe. As in the earlier legacy lane, the test
uses `reverse tcp:5555 tcp:5643` and restores that route after adbd restarts. This verifies the
app's legacy connection recovery, not a physical TV's daemon or networking. API 34 uses its real
on-device TCP listener. Both runs used the debug APK and a local fake service, not production.
The API 28 setup activity retained before process death measured 60,949 KiB PSS in the background;
this is a profiling baseline, not proof of a memory optimization or a leak.

Added `apps/android/tools/adb-reconnect-lane.py` with explicit emulator selection, matching the
remote AVD name before process mutation, real shell checks, captured meminfo and logcat. The
initial instrumentation helper now uses the same 60-second authorization wait as a manual Connect
and reports the error after the connection attempt. Its native Android 9 run and the instrumentation
APK build pass. The earlier 8-second helper timed out during first authorization on API 34; that
was test setup, not evidence of a production reconnect failure.

Evidence: `target/adb-reconnect-verification/{api34,api28}/results.json`, `memory-*.txt`,
`logcat.txt`, service logs and `build.log`. Neither pre-existing emulator (5620/5622) was changed.
The reported physical-TV disconnect, manufacturer process restrictions, TLS recovery on that TV,
reboot behavior and overall memory reduction remain open. No runtime reconnect code was changed.

## 2026-09-28 — display stored Extend files

`display show --image/--video` accepts a bare file UUID, `file:<file_id>`, its stored Briefcase
link or the service's file-content URL. The service resolves it in the caller's world, reuses the
normal file visibility check (requesting Silicon, Team and self-destruct time), reads Briefcase as
that caller, and relays an ordinary attachment. It sends no caller token or OBO proof to the device.
The same 8-file/8-MiB command attachment budget applies, including existing attachments. Store reads
enforce the actual byte limit, not only recorded size or Content-Length. File resolution happens
under the session's command lock, shares the command deadline, and rechecks the session before
relay so a concurrent Stop/takeover cannot be followed by display. Public media URLs still go to
the device unchanged. CLI help and implementation docs describe the forms and limit.

- Four real HTTP/WebSocket/PostgreSQL display tests pass: a previous session's own image reaches
  a 1.0 fake TV app in the existing attachment format; private links and file IDs resolve;
  other Silicons, other Teams, expired files, wrong types and exceeded budgets are refused before
  relay. A deliberately oversized local file with small recorded metadata is refused too.
  A controlled read exercises Stop and command timeout before relay. Log:
  `/tmp/extend-display-files-service.log`.
- A fake Briefcase HTTP endpoint verifies the delegated reader/Team and chunked response limit.
  This is part of the four tests, not live IAM/Briefcase evidence.
- Service suite: 147 pass, including existing contract/1.0 consumer replay; log:
  `/tmp/extend-display-files-all-service.log`. Opt-in real-service tests remain disabled.
- Client/CLI tests: 81 pass across nine result groups, including 1.0 source compatibility;
  log: `/tmp/extend-display-files-client-cli.log`. Explicit `file:` references stay remote instead
  of being mistaken for local paths.
- Workspace Clippy with `-D warnings` passes: `/tmp/extend-display-files-clippy.log`.

The combined real Briefcase → installed native TV emulator path subsequently passed the lane
recorded above. The physical-TV path and production release remain unverified.

## 2026-09-28 — TV image readiness and memory

Image display now acknowledges decoded content in the foreground, rather than activity startup.
Corrupt images, HTTP failures and video preparation errors reach the command's failure result.
Each display request has its own identifier, so an older completion cannot satisfy a newer one.
Downloads stream to disk with a 32 MiB limit even without Content-Length; decoding runs off the
main thread and samples to at most 2,073,600 pixels. Replacement, clear, cancellation and activity
destruction cancel downloads and release video/WebView resources.

- Android unit tests: 234 pass; lint, debug APK and instrumentation APK build pass. Six new JVM
  tests cover successful download, HTTP rejection, advertised and chunked size limits, sampling,
  and stale request completion. Log: `/tmp/extend-display-android.log`.
- Four native `DisplayTest` cases pass on a fresh, dedicated Android TV API 34 emulator
  (`ExtendDisplayVerification`, port 5640). Actual BitmapFactory decoding rejects a corrupt file;
  the full `CommandExecutor` returns `action_failed` for it. A delayed local HTTP 404 cannot report
  success while loading. A valid 3840 × 2160 PNG displays correctly with at most 8,294,400 allocated
  bitmap bytes. Log: `/tmp/extend-display-native.log`.
- The instrumentation build caught an older recording test still calling `connectionLost()`
  without its 1.1 pair identifier. Updated that call to the same empty pair identifier its session
  uses; the recording runtime lane was not rerun here.
- The initial command-path test lacked a bound accessibility service after instrumentation
  restarted the app. The dedicated-emulator test setup now rebinds it and disables UiAutomation's
  normal suppression of real accessibility services. The final four-case run passes.

These checks do not prove physical TV behavior, overall app memory consumption, debugging
reconnect, or authenticated display of a Silicon's stored Extend files. Those remain open.

## 2026-09-28 — iPad first screenshot

The hosted iOS driver now responds to the engine's typed `SESSION_NOT_FOUND` on screenshot or
screenshot-diff by attaching the same session with a bare `open`, then retrying the capture once.
It never supplies an app target. Existing sessions retain their binding. Attachment failures are
returned, and waiting for the device, stale-session cleanup, attachment and capture share the
original command deadline and cancellation token.

- Hosted-driver suite: 78 pass, 4 opt-in native tests ignored; the new regressions exercise first
  capture, repeated capture, lost daemon state, abandoned setup recovery, attachment failure,
  cancellation and timeout. Log: `/tmp/extend-ios-attach-tests.log`.
- `cargo clippy -p extend-hosted --all-targets --locked -- -D warnings` passes; log:
  `/tmp/extend-ios-attach-clippy.log`.
- The opt-in `simulator_first_screenshot` test passes through the built engine on a newly created
  iPad Air 11-inch (M2), iOS 18.4 simulator. Settings was already open via `simctl` before the
  Extend session. The engine event log records screenshot → `SESSION_NOT_FOUND` → targetless
  open → successful screenshot → close. The PNG was visually inspected: Settings/General
  remains visible. No XCTest runner existed before or after. Evidence is under
  `target/ios-first-capture/` (`before.png`, `capture/screenshot.png`, `events.ndjson`, `test.log`).
  The test used its own daemon, claims and leases; the daemon was stopped and the temporary
  simulator was shut down and deleted afterward.

This proves the driver and simulator path, not physical iPad/CoreDevice capture, annotated
screenshots requiring app accessibility, or an installed/released desktop build. Physical-device
and release checks remain in `completion-work.md`.

## 2026-09-28 — resumed banner follow-up

Recovered the interrupted Claude working tree on `release/1.1.0` at `505a695` and completed the
shared banner setting through the service, Rust client/CLI, website, Android notification/badge,
and desktop banner/icon. Preserved the 1.0 Rust `DevicePatch` struct-literal API by introducing
`DeviceSettingsPatch`; added consumer fixtures for both new calls and replayed them alongside
frozen 1.0 consumers. Device settings and the shared indicator now change in one transaction,
including the version check. Concurrent conditional edits are covered by a service test.

Observed on this Mac, with the existing local PostgreSQL and stand-in IAM; no physical device,
installed app or production service was changed:

- Android offline Gradle unit tests, lint and debug APK: 228 app + 10 libadb tests pass, lint and
  assembly pass. The new virtual-time tests cover the exact 10-second boundary, hiding, takeover
  persistence, session identity across pairs and process-restart announcement suppression. They
  also assert hiding the badge keeps the session and screen hold alive. Log:
  `/tmp/extend-resume-android.log`.
- Real HTTP/WebSocket/PostgreSQL banner tests: 4 pass, including two Carbons sharing the setting,
  Silicon refusal, carried-device refresh/reconnect, test isolation and competing If-Match edits.
  Existing device gap tests: 7 pass. Log: `/tmp/extend-banner-service-tests.log`.
- Client contract generation: 3 pass; real service contract replay: 9 pass, including frozen 1.0.
  Logs: `/tmp/extend-banner-contract-write.log`, `/tmp/extend-banner-contract-replay.log`.
- Protocol/client tests and old request-body source compatibility pass:
  `/tmp/extend-banner-client-tests.log`. CLI behavior tests include exact banner PATCH bodies and
  argument rejection; desktop tests include independent per-device timers, hidden icons,
  takeover persistence and Stop availability. `/tmp/extend-banner-rust-tests.log` records those
  passes before its missing-fixture gate failed; the separate contract run above closes that gate.
- Website typecheck, 170 unit tests and production build pass. Browser tests exercise the extra
  setup step and persist the banner setting across reload without stopping the running session.
  The first run caught outdated setup-step expectations. After updating the successful pairing
  paths, 57 browser cases passed; the remaining visual-tour case passed separately after changing
  its expected mobile progress from step 4/6 to step 5/7. Logs:
  `/tmp/extend-resume-web-e2e.log`, `/tmp/extend-resume-web-restyle.log`. Phone-width device settings
  screenshot was visually inspected (`web/test-results/screenshots/12-device-page-phone.png`).
- Workspace Clippy with `-D warnings` passes: `/tmp/extend-resume-clippy.log`.
- Final `cargo test --workspace --locked`: 559 pass, 4 ignored, 0 failures across 36 test/doc-test
  result groups (`/tmp/extend-resume-workspace.log`). The migration suite verifies schema 5,
  default-shown indicators on pre-existing rows, and rollback/roll-forward behavior. Opt-in
  real-service/native lanes were not enabled; a green harness result does not establish those.

This is local evidence, not proof of Windows/Linux native windows, a physical TV's placement,
Android's notification rendering, production interoperability or a release. The remaining final
fixes and runtime/release gates stay in `completion-work.md`.

## 2026-09-27 — 1.1.0 integration

1.1.0 was built by ten groups, each testing its own part, on top of `8da8e2d`; an integration pass
then ran every suite together on the whole tree (not committed). Same Mac (macOS 27, arm64),
PostgreSQL in `silicon-extend-postgres`, the local IAM, Briefcase and Ting stand-ins, and no device,
emulator or simulator. The service ran as its own instance on `:8580`/`:8581` with its own database.

| Command | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | clean |
| `cargo test --locked -p silicon-extend-client --test contract_fixtures` and `-p extend-service --test contracts` | 3 and 9 passed, including the frozen `v1/client-1.0.0` replay |
| `cargo test --workspace --locked` | 550 passed, 4 ignored (a sleeper helper in extend-agent; three extend-hosted tests that need a Simulator or pyatv), 0 failed |
| `bash e2e/cli-e2e.sh http://127.0.0.1:8581` | all 119 checks |
| `cd web && pnpm typecheck && pnpm test && pnpm test:e2e && pnpm test:e2e:compat && pnpm build` | 161 unit tests; 57 Playwright tests against the mock; the released 1.0.0 website against the 1.1 mock (1 test); build ok |
| `pnpm test:e2e:real` (`EXTEND_REAL_URL` at the 1.1 service) | 5 passed |
| `cd vendor/extend-engine && pnpm typecheck && pnpm lint && pnpm format:check && pnpm build && pnpm test:unit` | 1,398 files: 10,692 tests passed, 1 skipped; no test near the slow-test gate |
| The same `pnpm test:unit` in the `silicon-extend-linux-e2e` image (Debian, arm64, Node 22, as a non-root user with zip) | 10,640 passed, 53 skipped (platform-specific), 0 failed; no test near the slow-test gate |
| `node --test apps/desktop/*.test.mjs` | 65 passed |
| `JAVA_HOME=… ./gradlew --offline --rerun-tasks lint testDebugUnitTest assembleDebug` in `apps/android` | lint 0 errors (73 warnings); 226 app and 10 libadb JVM tests; debug APK built |
| `OUT=<scratch> apps/desktop/macos/build-app.sh` (ad hoc signed) | `Silicon Extend.app` 1.1.0 with `MacOS/Silicon Extend Helper` (identifier `com.teamofsilicons.extend.macos-helper`) and the engine entry `Resources/engine/bin/extend-engine.mjs`; not installed or launched |
| `npx @redocly/cli@2.49.0 lint understanding/api.yaml --skip-rule no-path-trailing-slash` | valid, 12 warnings (the same 12 as 1.0.2) |

Not covered here: anything on a physical device, emulator or Simulator, real IAM and Ting, and the
release gates in `completion-work.md`.

## 2026-09-27, round 2 — audit fixes

After the first round was committed (`5b3c578`), an audit checked the build against every
requirement in `UNDERSTANDING.md` and listed what was unmet. A second round fixed those items in
eight groups (service core, service devices, service test environments, versioning, CLI, website,
Android, desktop and fork), between about 02:30 and 04:55. Each group's work was checked by a
separate verifier agent that re-ran its tests, read the code, and tried to break it (often by
reverting a fix in a scratch copy and checking that a test failed). Same Mac (macOS 27, arm64),
the headless emulator `Medium_Phone_API_36.0` (emulator-5554), Docker, and the local IAM, Briefcase
and Ting stand-ins. **Nothing of round 2 is committed:** it is the working tree on top of
`5b3c578`. Nobody drove the Carbon's desktop.

The development service on `:8480` was restarted once, at 04:07:51, with a round-2 build; no file
under `crates/extend-service/src` or `crates/extend-protocol/src` changed after 04:04, so it serves
the round-2 service. Most service verifiers ran their own instances on other ports with their own
databases.

One incident: between about 02:56 and 03:00 a service-core agent built mutated scratch copies into
the shared `target/` directory, and cargo reused those artifacts for the real tree. An
`extend-service` test run by anyone in that window may have seen false results. The agent forced a
rebuild at about 03:00; every run cited below is later.

### Run by the documentation pass (05:00–05:15)

| Command | Result |
|---|---|
| `cargo fmt --all --check` | **exit 1**: 32 hunks in 9 files, all in `crates/extend-agent` (`autostart.rs`, `config.rs`, `drivers/agent_device.rs`, `drivers/probe_linux.rs`, `drivers/probe_macos.rs`, `drivers/screen_lock.rs`, `main.rs`, `ui/mod.rs`, `tests/fake_service.rs`). CI's first check fails until they are formatted. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 (macOS host) |
| `EXTEND_TEST_ADMIN_URL=postgres://extend:extend@127.0.0.1:5440/postgres cargo test --workspace --locked` | exit 0. extend-agent 164 unit + 10 `fake_service`; extend-cli 28 unit + 16 `cli_behaviour` + 8 `device_args` + 2 `json_consumers`; extend-hosted 63 (2 ignored); extend-protocol 9; extend-service 34 unit + 8 `contracts` + 16 `core_gaps` + 7 `devices_gaps` + 4 `e2e` + 6 `obo_requests` + 17 `testenv_gaps` + 1 `real_services` (its body runs only with `EXTEND_REALIAM_STATE` set, so here it returned at once); silicon-extend-client 6 unit + 3 `contract_fixtures` + 2 doc tests; silicon-iam-client 47 + 22 + 2 + 3 doc tests. |
| `npx -y @redocly/cli@2.49.0 lint understanding/api.yaml --skip-rule no-path-trailing-slash` | valid, 0 errors, 12 warnings (missing 4xx/2xx responses on `/live`, `/ready`, the WebSocket routes and some list routes, and no `license` in `info`) |
| `pnpm exec vitest run --project unit-core <the 23 fork test files Extend touched>` in the device engine (now `vendor/extend-engine`; the list is in `ci.yml`) | 23 files, 278 tests passed (macOS host) |
| `node --test apps/desktop/*.test.mjs` | 60/60 (the fork's `dist` was present, so the real-dist completeness case ran) |
| `JAVA_HOME=… ./gradlew :app:testDebugUnitTest --rerun :libadb:testDebugUnitTest --rerun` in `apps/android` | 138 app + 10 libadb JVM tests, 0 failures |
| `pnpm install --frozen-lockfile --lockfile-only` on a copy of the fork's manifests | the lockfile matches (checks the `fork` CI job's install) |
| Black-box calls to `:8480` | `GET /api/version` with no header → 200, version 1; `/api/v2/devices` → 400 `api_version_unsupported`; a pin of 2 on a v1 path → 400 `api_version_mismatch`; a blank or unknown test secret on `/api/v1/iam`, `/api/v1/contracts` and `POST /api/v1/enrollments` → 401 `testing_secret_invalid`; `/files/{id}/content` without a token → 401; as c:alice, `include_removed=maybe` and `scope=team&include_removed=true` → 422 with hints, `scope=mine&include_removed=true` listed 32 removed devices with `removed_at`/`removed_reason`; a removed device's detail (empty capabilities, the `missing[0]` reason) and activity → 200; `PATCH` on it → 404 with when, why, the hint and `details`; `os=beos` → 422; `POST /auth/logout` with a blank token → 422; `GET /api/v1/testing-environment` without a secret → 400 `test_only`; an unknown lifecycle receipt → 404 `request_not_found`; the contracts matrix has every field `api.yaml` now lists. |
| `adb shell pm list packages` on emulator-5554 | only `com.teamofsilicons.extend` and its test package; the old `com.teamofsilicons.bridge` is gone |

Not run by this pass: `e2e/cli-e2e.sh`, the website suites, the real-IAM harness, the Linux
container lanes, the Swift helper tests and anything on the emulator. Those results below are the
groups' and verifiers'. This pass also found that `.github/workflows/release.yml` now builds
`-p silicon-extend-cli`, a package that doesn't exist (the CLI package is `extend-cli`), so a tagged
release would fail; that file belongs to another group and was not changed here.

### Integration pass (05:20–06:00)

Every suite run again on the whole round-2 tree, after these fixes:

- `cargo fmt --all` formatted the 9 `crates/extend-agent` files (the vendored
  `silicon-iam-client` came out byte-identical: its own `rustfmt.toml` applies).
- The local IAM stand-in's logout revoked the Carbon's logins in **every** world, so leaving a test
  environment on the website signed the Carbon out of production too (the real-service lane's
  "exit back to production" failure). It now revokes only the logins of the world signed out of;
  regression test `iam::tests::local_logout_in_a_test_environment_leaves_the_production_login_alone`.
- `release.yml` builds `-p extend-cli` again; `crates/extend-cli/tests/build_scripts.rs` checks that
  every `cargo … -p` in the workflows, the Dockerfile and the desktop build scripts names a
  workspace package.
- `scripts/package-cli.py` staged `licences/` beside `targets/`, which `honeycomb pack` drops
  silently, so every release failed the script's own archive check. The licences now go in each
  target's root; `scripts/test_package_cli.py` packs stand-in executables with the real
  `honeycomb` 0.5.0 and failed before the change.
- `e2e/clean-test-dbs.sh` drops all four prefixes the suites create (it had dropped 1,307 left
  over).

| Command | Result |
|---|---|
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace --no-fail-fast` | 480 passed, 0 failed, 2 ignored (the 478 above plus the two new tests) |
| `cargo check -p extend-agent -p extend-hosted --target x86_64-pc-windows-msvc` | exit 0 |
| `pnpm build`, `pnpm typecheck` and the 23 CI vitest files in the device engine (now `vendor/extend-engine`) | built; 23 files, 278 tests passed |
| `node --test apps/desktop/*.test.mjs` | 60/60 against the rebuilt dist |
| `bash e2e/cli-e2e.sh` against `:8480` (restarted at 05:35 and 05:38 with this build) | 75/75 |
| `web`: `pnpm test`, `pnpm build`, `pnpm test:e2e`, `pnpm test:e2e:real` | 112/112; built; 34/34; 5/5 (1 of 5 before the logout fix) |
| `./gradlew :app:testDebugUnitTest --rerun :app:assembleDebug`, `:libadb:testDebugUnitTest --rerun :app:assembleDebugAndroidTest` | 138 + 10 passed; APK and test APK built |
| `bash e2e/android-adb.sh 1cdee418` (emulator-5554, the installed 03:40 build) | 6/6 PASS |
| `python3 e2e/real-iam/realiam.py all --briefcase --ting` | 67 passed, 0 failed, 1 known Briefcase gap |
| `realiam.py up --briefcase --ting` + `EXTEND_REALIAM_STATE=… cargo test -p extend-service --test real_services` | 1/1 |
| `bash e2e/run-all.sh` | 9/9 lanes (Redocly: valid, 12 warnings) |

Not run: the emulator instrumentation tests (`connectedDebugAndroidTest` uninstalls the paired app
afterwards), the Linux container lanes, the Swift helper tests.

### Service: sessions, files, requests, Ting (service core)

Verified (verifier's own runs; `core_gaps` 16/16 in three runs of about 19 s):

- **Idle timer:** with the idle window cut to 1 s, a 6 s command keeps the session active, and
  `idle_ends_at` is more than 300 s out during it and 290–300 s after it. A command queued behind
  one whose session ended is refused, not relayed (one `command` frame reaches the device).
- **Ending mid-command:** device removed, pair revoked on the device, and access removed while a
  command runs each answer `session_ended` within seconds (not at the 120 s deadline), with
  `end_reason`, `command_id`, `may_have_run: true` and a hint; the device gets `session_ended`
  before `unpaired`, or `cancel` without `unpaired` when only access was removed. The `stop` frame
  and `POST /api/v1/device/stop` (in flight, idle no-op, 401 for an unpaired credential).
- **Refused logins:** against **real IAM** (the `e2e/real-iam` harness), after revoking si:chef's
  refresh family directly at IAM (no webhook), a session call got `token_expired` 31 s later and
  15.2 s after that the session was `ended|silicon_logged_out`, and the device received
  `session_ended`. A refreshed token keeps the session (test).
- **Ting:** `realiam.py up --briefcase --ting` then `check`: check #1, "Extend registered si:chef as
  a Ting recipient before delivering — first request delivered", passes against real Ting 0.1.9, and
  `real_services.rs` passes. A test-world session registered with Ting's test plane, which refused it
  (503 "Import this testing environment through Honeycomb"); Extend logged that and went on.
  Pending requests are retried with no session anywhere and fail with a reason after 6 attempts
  (test driving scheduler passes directly). The reason with quotes, ünïcode and `<tags>` arrived
  through real Ting exactly as sent.
- **Self-destruct against real Briefcase:** with si:chef logged out and a backdated file, the pass
  logged "could not delete the file yet; its record stays … tries=1 retry_in_s=60" and kept the row;
  after si:chef signed in again (no session), the next pass 60 s later deleted it and Briefcase
  answered 404 for c:alice with the entry in si:chef's bin.
- **Download route against real Briefcase** (called directly): c:alice got 200 `image/png`, 67
  bytes whose SHA-256 matches Briefcase's copy; `Range: bytes=0-7` gave 206 with
  `content-range: bytes 0-7/67`; c:bob read the file of his own device; c:alice got 404 for bob's.
- **Warnings:** a double file store produced four of the five cases; "stored but not recorded" is
  covered only by reading the code.

Not verified: the CLI's downloads through the new route against real Briefcase (the harness's own
"briefcase: download" check was still failing when it ran, because `extend file get` still used
the Briefcase URL; the CLI switched afterwards and the harness was not run again); anything after a
service restart (logins are held in memory).

### Service: devices (service devices)

Verified: the full `extend-service` suite, clippy and rustfmt clean at the time; `devices_gaps`
passed 3 more times (54 claims in 0.58–0.60 s, slowest production read 63–76 ms). Mutations in a
scratch copy with its own target directory: the round-1 design (no in-memory turn) made the burst
test fail with a 500 after 30.8 s; removing only the turn made the connection test fail (32
advisory waiters instead of 1); removing the lock let 8 of 8 claims succeed in 2 of 3 runs. Two
service processes on one database with 20–24 concurrent claims, some with an outside connection
holding the lock: every run ended with exactly 5 devices, the rest `409 test_device_limit`, and
production reads answered in ≤78 ms. With the lock held for 20 s, claims answered `429 rate_limited`
at 5.06 s and 10.06 s. The removed-device reads, the hosted-device path end to end (attach, the
`attach` frame, the setup code, `attached`, commands with `target`, a targeted Stop, removal) and
full pages with the online filter (also a scratch test with 460 offline devices spanning several
batches) were checked.

Found and still open: retrying the pairing that filled a test environment, with the same
`Idempotency-Key` and body, answers `409 test_device_limit` instead of replaying its 201, because
the early limit check runs before the idempotency lookup (also true before round 2).

### Service: test environments (service test environments)

Verified by the verifier on a private instance (`:8591`): with a live code from the other world and
with an unknown code, every caller that isn't a signed-in Carbon with a team (not signed in, a bad
token, a Silicon, a Carbon naming another team, no `X-Org-ID`) got the same status and code for
both; only a signed-in Carbon heard the precise `404 pairing_code_invalid`, in both directions.
Reverting the per-member limit, or moving the check before sign-in, made `testenv_gaps` fail at the
expected lines. A restore of a purged environment, and the retry of a restore accepted before the
purge, both got 409; restoring the old retry exemption made
`removed_and_superseded_operations_never_run_again` fail. `POST /api/v1/pairings` with an unknown,
blank or disabled environment's secret got 401 before any sign-in check. Readiness: `activate` at the
same revision opens a `preparing` environment, an older one is 409, and IAM accepting the secret
opens it. `e2e/cli-e2e.sh` (the pre-round-2 CLI, 48 checks) passed against that instance and its
cleanup purge left the environment `removed`. The verifier's notes on the other test-environment
items (receipts, the clean fence, webhooks, logout, the production default, test-plane login) did
not reach this record; `testenv_gaps` (17 tests) covers them and passes in this pass's run.

Not verified: a real IAM or Honeycomb run of the lifecycle (including IAM introspection of refresh
tokens on logout), a Docker image build, and how the Android app behaves on close code 4503 (its
code treats unknown close codes as temporary and reconnects).

### Versioning

The verifier's `cargo test -p extend-service` (03:10) passed, `contracts` 8/8, and clippy was clean;
this pass's run agrees. The lifecycle (deprecation headers, sunset after 7 quiet days with an
injected clock and usage rows, `410` afterwards, negotiation steering around a sunset major, two
majors side by side each checking its own pin), the matrix, and the replay of every fixture in
`contracts/` against a real service are what `contracts.rs` tests. The device fixtures are derived
by hand from `docs/device-protocol.md` and the apps' code; the apps don't dump their own frames yet.

### CLI

The verifier (04:43–04:51): `cargo test -p extend-cli` 28 unit + 16 `cli_behaviour` + 8
`device_args` + 2 `json_consumers`, the client crate's tests, clippy clean, and `bash
e2e/cli-e2e.sh` against `:8480`, 75/75 twice. The checks include `--json` printing the data itself
and `{"error": …}` on stderr, `login status` at exit 0 with `authenticated: false`, the test line on
stderr after early failures, `EXTEND_TEST_SECRET`, paging in `device ls`, `config home` moving the
login, and downloads through the new route (local file store). Not verified: downloads against real
Briefcase, and `extend version` against a deprecated or sunset major on a live service (unit and
fake-service tests only).

### Website

The group's and the verifier's runs: `pnpm test` 112/112 (7 files), `pnpm build`, and `pnpm
test:e2e` against the mock 34/34 (04:42 and 04:50). `pnpm test:e2e:real`: 5/5 against the web
agent's own service on `:8487` (04:39); against `:8480`, 4 of 5 in three runs (04:20, 04:37, 04:51).
The failing test is "test environment: banner, test member login, device limit, exit back to
production": after leaving the test environment, the signed-in member id is not shown within 10 s.
Its cause was not found. The removed-device test ("the Remove dialog says what happens, then its log
stays readable under Removed") passed against `:8480`. Screenshots are in
`web/test-results/restyle/`. Not verified: sign-up through a real IAM (the "Create an account" link
guesses IAM's `/signup` address), and the deployed website.

### Android

The group and the verifier: `:app:testDebugUnitTest` 138 tests (21 classes; this pass reran them
with libadb's 10); with each fix reverted in a scratch copy, `DebuggingAfterRestartTest` (3),
`EnrollmentLoopTest` and `TvRemoteKeysTest` fail. On emulator-5554 against `:8480`: after `adb
reboot` the device came back `ready` with setup `complete` and the Wireless-debugging step
`needs_carbon`; a session and `snapshot -i` worked; `extend adb shell` was refused
(`unsupported_on_device`) with the after-restart reason; the notification was shown, and the
verifier's screenshots show the system Wireless debugging page open after the notification; turning
Wireless debugging on reconnected within 3 s and the step returned to `done` (as the group reports).
The TV emulator, against a stand-in answering 429, showed "Silicon Extend TV" and "Extend is limiting
new pairing codes from this network … asks again by itself in 12 min 34 s (at 03:40)" (seen in the
verifier's screenshot). The group reports that at 360 dp and 1.3× font scale the labels wrap
instead of being cut (screenshots under `apps/android/build-screens/`, not committed).
Not verified: physical phones and TVs, `input keyevent` remote buttons on a TV (unit tests only),
TalkBack, the TV D-pad on the licences screen.

### Desktop agent and fork

The verifier (03:41–03:55): `cargo test -p extend-agent` 164 unit + 10 integration on macOS and
again inside the Linux container (with the Linux e2e passing); `record-hung-e2e.py` passed every
case with no window manager and with openbox; the verifier's own probe refused all four
background-`None` cases, the two-recorder one included, with no frame written (before round 2 two
of them leaked the cover); `record-e2e.py` three times, the isolation, app and runtime lanes, and
the runtime driver lane including "`--quality high` records on Linux, encoded at 20 Mbit/s";
`runtime-update-e2e.mjs` 5 PASS and 2 REPRODUCED lines, as before. This pass: the 23 fork vitest
files and `node --test apps/desktop/*.test.mjs` 60/60. Not verified: anything on a real desktop
(start at login after a real login, a real lock screen, the Stop rows for carried devices in a
real tray and banner, "Download the update" opening a browser), macOS recording quality in a real
recording, and Windows at all.

### Not verified after round 2

- A production deployment, the deployed website, and a production IAM, Briefcase, Ting or
  Honeycomb (the real-service harness runs them locally).
- IAM logout or revocation webhooks: IAM sends none to applications, so logout elsewhere is ended by
  the 15 s check (`TECHNICAL.md` open question C1).
- Everything after a service restart that depends on logins held in memory (self-destruct, Ting
  retries, identifying a refused Silicon).
- Physical devices of every kind; a real Mac, Linux or Windows desktop.
- The new CI jobs (`fork`, `android`, the named contract step) have not run on GitHub.

## 2026-09-27 — rename, review fixes, restyle, Briefcase and Ting

The work ran from the evening of 2026-09-26 into the early hours of 2026-09-27, on this Mac
(macOS 27, arm64) with the headless Android emulator `Medium_Phone_API_36.0` (emulator-5554,
Android 16), a headless Android TV emulator (API 34) for the restyle, and Docker. Every fix was made
by one agent and then checked by a separate verifier agent that re-ran the tests and tried to break
it; where a verifier found a problem, a second round fixed it and a second verifier checked again.
None of this work is committed yet: it is the working tree on top of `7af7fd5`. Nobody drove the
Carbon's desktop; the shared development service on `:8480` was never restarted. Its enrollment
limit (60 per hour per address, in memory) was used up by concurrent runs, so most lanes ran against
their own service instance on another port with its own database, dropped afterwards.

### Run by the documentation pass

| Command | Result |
|---|---|
| `cargo fmt --all --check` | exit 1: 1,492 hunks in 78 files. The review found the same failure on the earlier base `fcf9a90`; formatting is left to the integration step. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 (macOS host) |
| `EXTEND_TEST_ADMIN_URL=postgres://extend:extend@127.0.0.1:5440/postgres cargo test --workspace --locked` | exit 0. extend-agent 142 unit + 9 fake-service; extend-cli 15 unit + 8 `device_args`; extend-hosted 63 (2 ignored); extend-protocol 9; extend-service 13 unit + 4 `e2e` + 6 `obo_requests` + 1 `real_services` (its body runs only with `EXTEND_REALIAM_STATE` set, so here it returned at once); silicon-iam-client 47 + 22 + 2; 4 doc tests. The `e2e` suites left four throwaway `extend_e2e_*` databases (`e2e/clean-test-dbs.sh` drops them). |
| `npx @redocly/cli@2.49.0 lint understanding/api.yaml --skip-rule no-path-trailing-slash` | valid, 15 warnings: the same 15 as the committed file |
| `cargo metadata --format-version 1 --locked --offline` | 581 third-party crates, listed in `THIRD_PARTY_NOTICES.md` |
| read-only `gh repo view` / `gh api` on `teamofsilicons/silicon-extend` and `teamofsilicons/silicon-bridge` | both exist and are other products (`completion-work.md`) |

Not run by this pass: `e2e/cli-e2e.sh`, the Android, website, Swift and fork suites, and any device
lane. Those results below are the other agents'.

### Rename (commit `7af7fd5`)

Silicon Bridge became Silicon Extend: crates `extend-*` and `silicon-extend-client`, the `extend`
CLI, `EXTEND_*` settings, the `Extend-Device` and `Extend-Enrollment` auth schemes,
`Silicon-Extend-API-Version`, the `ees_` (enrollment secret) and `edc_` (device credential) prefixes
(were `bes_`/`bdc_`), `extend.teamofsilicons.com` and `backend.extend.teamofsilicons.com`, the
Android package `com.teamofsilicons.extend`, and the `extend`, `extend_global` and `extend_test_*`
database schemas. Upstream names that contain the word (`snapshot-bridge`, `atspi-bridge`, "Android
Debug Bridge") were kept.

- The same commit rewrote the Carbon-owned `understanding/UNDERSTANDING.md` (128 changed lines,
  Bridge → Extend and the domains), although the file says agents must not edit it. The Carbon
  should review that diff.
- Rename damage found and fixed later: GTK's own `NO_AT_BRIDGE` variable had become
  `NO_AT_EXTEND` in `apps/desktop/linux-e2e/Dockerfile` and `e2e.sh`; FORK.md still said "Bridge".
- On the emulator the old `com.teamofsilicons.bridge` app is still installed beside the new
  package; it is not part of this product any more.

### Mark and restyle

- **Mark:** a sibling of Silicon Interface's mark on the same grid, one square of the ring reaching
  out to another square (`web/public/brand/mark.svg`). Used for the favicon, the website, the
  Android launcher icon (adaptive, with a monochrome layer) and TV banner, the Mac app icon
  (`apps/desktop/macos/icon/`, built by `make-icns.sh`) and the tray icon. Checked by rendering at
  16–1024 px, `cargo test -p extend-agent --lib ui::` (2 new icon tests), a headless Chromium
  screenshot of a website build, `./gradlew :app:processDebugResources`, and `iconutil` round-trips
  of the `.icns`. Not seen in Finder or a real menu bar.
- **Website:** restyled as a sibling of Silicon Interface (tokens copied from Interface, IBM Plex
  Sans and Mono, Source Serif 4 titles ending in a period, a dithered print on sign-in, empty states
  and the pairing ticket, a ⌘K menu, a phone layout with a bottom bar). `pnpm check`, `pnpm test`
  (79 tests), `pnpm build` and `pnpm test:e2e` (27/27 against the mock) pass; `pnpm test:e2e:real`
  passed 4/4 against the agent's own service on `:8483` and 2–3 of 4 against `:8480`, where the
  others stopped at the enrollment limit (HTTP 429). The design critic accepted it with only
  low-severity notes left.
- **Desktop window and banner** (`crates/extend-agent/src/ui/page.html`): the same system, fonts
  inlined. Rendered offscreen in headless Chromium and WebKit (37 states each, no page errors, no
  overflow) with a 32-case banner fit check in both engines; `cargo test -p extend-agent` and clippy
  pass. Not seen in a real window (WKWebView, WebKitGTK or WebView2). Accepted by the critic with
  low-severity notes.
- **Android app:** Interface's system in Compose (fonts bundled), a redrawn TV corner badge
  (`ui/InUseBadge.kt`), and a styled Open-source licences screen. `./gradlew :app:testDebugUnitTest
  :app:assembleDebug` (104 JVM tests) passes; phone (API 36) and TV (API 34) emulators were
  captured in every state against `tools/fake-service/fake_extend.py`, because `:8480` answered
  429. TalkBack itself was not run. Accepted by the critic with low-severity notes.

### Android debugging

Checked by the second verifier with the current code:

- `./gradlew :app:testDebugUnitTest :libadb:test --rerun-tasks`: all JVM suites pass, including
  `SessionRetentionTest` (8, its constants check against `crates/extend-protocol` not skipped),
  `CommandJobsTest` (5), `RecordingTimelineTest` (7), `AdbWireTest` (7), `FramesTest` (12),
  `ArtifactNameTest`, `LicencesTest`, `AdbReconnectPolicyTest`, and libadb's `ExtendTransportTest`
  (7/7) and `AndroidPubkeyTest` (3/3).
- On emulator-5554, `am instrument -e local_daemon true` for `LocalAdbTest` and `RecordingTest`:
  19 tests, 15 ran and passed, 4 skipped by assumption (`connectLocalForService`,
  `wirelessPairing`, `wirelessReconnect`, `beyondNativeLimit`), 114.7 s.
- Mutation checks: restoring upstream libadb's acknowledge-on-arrival makes both flow-control tests
  fail; changing the pinned spake2-android checksum makes the build fail.
- Against the verifier's own service (`:8494`): a service restart during `record start` and
  `logs start` still gave an 82.8 s MP4 and a log with both markers; a 158 s network outage kept the
  session and gave a 167.97 s MP4 (the old 150 s device grace would have deleted it); a 181 s outage
  ended the session at +171 s, the device then removed its capture and the CLI reported
  `session_ended`.
- Through the CLI: `adb shell "head -c 270000000 /dev/zero"` returned `action_failed` with the
  256 MiB hint in 2.25 s; 40 MB on stdout plus 3 MB on stderr returned 256 KiB inline per stream and
  both full files; the app process stayed the same throughout. `adb shell sleep 60` ended 4 s after
  the Carbon's `extend device stop` with `session_ended` (exit 1; exit 6 is CLI work still open).
  A non-zero exit gives `command_failed` with `details.exit_code`; `adb pull` of
  `Café résumé 写真.txt` kept its name.
- `e2e/android-adb.sh` against `:8494`: all 6 PASS lines (shell, install/reinstall, push/pull, logs,
  recording, Carbon Stop with exit 6 and cleanup).
- `tools/wireless-debugging-lane.py` (TLS): pairing passed; `wirelessReconnect` failed three times,
  because discovery picked a dead mDNS advertisement the emulator kept publishing (open).
- Emulator UI: "Open-source licences" opens the licences screen.
- Emulator state afterwards, as the agents reported it: the Extend debug app and its test APK are
  installed beside the old `com.teamofsilicons.bridge` app, pointed at `http://10.0.2.2:8480` and
  unpaired; the emulator's adbd trusts the app's key (legacy port 5555 and a Wireless debugging
  pairing); proof videos are in the app's external files directory.

### Desktop agent

- `cargo test -p extend-agent` (142 unit + 9 integration, three runs of the dispatch and driver
  tests), clippy with `-D warnings`, and `cargo check --target x86_64-pc-windows-msvc` pass.
- The real `extend-agent exec` against a logging fake device engine: 28 `open`/`close` cases with
  named Mac apps, `--save-script` in every position, and the Spotlight fallback for apps outside
  `/Applications`.
- Live against the verifier's own service (`:8498`) with a headless agent and a fake device engine
  whose cleanup fails: the held-computer report (setup complete, the engine's capabilities under
  `missing` with one reason), a new session and `terminal` while held, `snapshot` refused with exit
  10, background retries forcing only with no session in use, `kill -9` mid-session and restart
  without disturbing the session, the forced release at session end, the hold shown 0.6 s after
  `session end`, and a session that ended while the agent was down closed at the next start.
- Not run: anything on a real desktop (named apps opened by the real helper, the banner in a real
  window, a real stuck recording).

### CLI

- `cargo test -p extend-cli` (15 unit + 8 integration) and clippy pass. Every integration test also
  failed against the pre-fix binary.
- `bash e2e/cli-e2e.sh` against the verifier's own service (`:8493`): 48/48. Against `:8480` it
  stopped at check 9 on the enrollment limit.
- About 60 adversarial commands through that service with a fake Android device: `adb` arguments
  arrive verbatim, Extend flags before the first `adb` argument, the three `adb pull` forms and their
  refusals, file ids, links, missing paths, directories and `.aab` refused before sending (exit 2),
  an exactly 8 MiB push sent and 8 MiB + 1 byte refused, a 40 GiB sparse file refused with about
  9.9 MB peak memory.
- The parts recipe for a 9 MiB APK (`split -b 8m`, push, `cat`, `pm install -r`) worked on the
  emulator with host adb; not through Extend's own ADB connection.
- Still open after the CLI rounds: `record start --quality normal`, which `cli.yaml` now documents
  for every platform, fails with `invalid_args` on Mac and Linux (a desktop-agent change, not made).

### The device engine (fork) — macOS

- Targeted vitest suites 43/43; the wider run 4,842 passed and 5 failed, all 5 attributed to code
  older than this work. An in-memory mutation harness showed the new tests fail against the old
  logic for each fix. `swift test` for the helper: 21/21 and 6/6 (no test posts events or activates
  apps); a clean release build at the macOS 13 deployment target; `pnpm typecheck`, lint and build.
- Not run, because they drive the real desktop: `apps/desktop/macos/text-e2e.py` (frontmost-app
  typing including `find … type`, colour checks of screenshots and recordings, key-release on
  cancel), hiding or quitting an app mid-recording, revoked Screen Recording, a context menu in an
  app screenshot, macOS 13–15.1, two displays.

### The device engine (fork) — Linux

- In the `silicon-extend-linux-e2e` container: `record-hung-e2e.py` 18/18 with no window manager and
  18/18 with openbox (the previous worker fails 6 of them), `record-e2e.py` all pass, the isolation
  lane with no edge, unmap, resize and remap 4/4, `record-app-e2e.py` and `record-runtime-e2e.py`
  pass; 42/42 vitest; a 3000x2000, 60 fps capture kept real time (5.23 s of video for 5 s).
- `package-install-check.sh` in a clean `debian:trixie`: the Depends-only and the Recommends
  installs pass.
- The installed package through the local service (`record-service-e2e.py`, app and device
  recording, upload, full decode, identical repeat download) passed with the first-round worker:
  `service-recording-6snwvqkw/summary.txt` and `service-recording-nyiexg15/summary.txt` (package
  SHA-256 `eac81e1a…`). The second-round worker was not run through this lane.
- An adversarial probe found two remaining leaks (a plain Xlib window with background `None` and no
  `_NET_WM_PING`, left by a cover while hung; a second recorder on an already-redirected window);
  they are open (`vendor/extend-engine/FORK.md`). A frozen Java Swing window did not leak.
- Not run: any real Linux desktop.

### Packaging

- `node --test apps/desktop/stamp-runtime.test.mjs apps/desktop/runtime-entry.test.mjs`: 37/37.
- `apps/desktop/macos/build-app.sh` in a scratch copy of the repository reached through symlinks,
  with pnpm and cargo stubbed, a real Node download and ad-hoc signing: exit 0,
  `codesign --verify --deep --strict` passes, only the `-adhoc.zip` is produced. The same build with
  the old stamp script fails as intended. A corrupted cached Node tarball is downloaded again; a wrong
  pinned hash caches nothing.
- The notarization paths ran with `codesign`, `xcrun` and `spctl` stubbed (submit error, `Invalid`,
  staple failure, `spctl` rejection each stop with no zip; success gives the plain zip). Real
  notarization was not run.
- `runtime-update-e2e.mjs` against the packaged Mac runtime and the Linux tarball runtime, each under
  its bundled Node 22.23.3: 5 PASS and 2 REPRODUCED lines. With the real fork, a moved install
  replaced the stale daemon and four concurrent commands shared one new daemon.
- The verifier found a regression that is still open: the new entry reads `--state-dir` past `--`,
  so typed text can choose where it writes its record (`completion-work.md`).

### Briefcase and Ting through real services

- Services: Briefcase 2.1.0 (image `briefcase-backend:candidate`, rev `2e6ffef`) with its migrate,
  API and worker; MinIO `RELEASE.2025-07-23T15-54-02Z` built from source by `realiam.py build-minio`
  (the images are no longer published); Silicon IAM `silicon-iam:release-433665db`; the public Ting
  0.1.9 server binary, checked against its SHA-256 (`069e71b1…`), on SQLite.
- `python3 e2e/real-iam/realiam.py all --briefcase --ting` from scratch: 66 passed, 2 failed,
  1 known Briefcase gap (exit 1). `realiam.py all` without the new lanes: 51/51. The
  `real_services` test against the live fixture passed on both runs.
- Confirmed through Briefcase's own API: a Silicon's screenshot is stored in
  `apps/extend/private/si:chef/` with the right owner, size and bytes; the returned URL is
  Briefcase's permanent URL; the owner's share is exactly read + update (a plain member cannot
  delete); `extend file keep` leaves the entry alone; a self-destruct trashes it through
  `entries.trash`. Three defects were found and fixed: the share asked for `write` (Briefcase
  refused it), a repeated file name crashed the second screenshot, and the trash request lacked its
  `operation_id`.
- Ting accepted Extend's `tings.send` with its OBO proof and delivered the request with the exact
  reason.
- Still failing: Extend never registers a Silicon as a Ting recipient, so a real Ting refuses the
  request (`recipient_not_registered`) and it stays pending; `extend file get` and
  `screenshot --out` fail in Briefcase mode. Known Briefcase gap: a delegated invitation to a Carbon
  Briefcase has never seen fails with `invalid_principal`.
- Not exercised: Briefcase's and Ting's test planes, large files through the one-shot upload, a live
  IAM refusal of an OBO exchange.

---

The rest of this file is the 2026-09-26 record.

## Automated suites — 2026-09-26

| Suite | Where | Result |
|---|---|---|
| Protocol, service, client, CLI unit tests | `cargo test --workspace` | pass |
| Service end-to-end (`crates/extend-service/tests/e2e.rs`) | real PostgreSQL, real HTTP/WS, scripted device | 4/4 suites: pairing, one-at-a-time, relay, files + self-destruct, takeover, stop, access removal mid-session, idle timeout, logout, pair expiry, supersede, session ids growing to 4 chars, rate limits, test environments (limit message, isolation, clean/disable/restore/purge), version negotiation |
| CLI end-to-end (`e2e/cli-e2e.sh`) | running service + `examples/fake_device` | 48/48 checks: help tree, login/status, pairing, device management, sessions, `--help` narrowing, exit codes, files, requests via Ting, takeover, activity redaction, reports, `--test` environments and isolation |
| Real Silicon IAM (`e2e/real-iam/realiam.py all`) | local IAM containers + Extend in SDK mode | 51/51: real SLT login, refresh/reuse, revocation, directory, access grants, signed webhooks (removal ends access; forged 401; duplicates once), testing plane with member-id login |
| Website (`web`) | vitest, Playwright vs mock, Playwright vs real service | 78 unit, 23 mock e2e, 4 real-service e2e |
| Desktop agent (`crates/extend-agent`) | macOS + Linux (Docker, arm64) | 107 unit + 7 integration (fake service) |
| Hosted drivers (`crates/extend-hosted`) | mocks, iOS Simulator | 63 tests; Apple TV pairing crypto against pyatv's server (opt-in) |
| Android (`apps/android`) | JVM, phone emulator (API 36), Android TV emulator (API 34) | 58 unit; 83/83 phone and 36/36 TV checks vs fake service |

## Real devices, through the real service and the `extend` CLI — 2026-09-26

| Device | What ran |
|---|---|
| This Mac (macOS 27) | paired via CLI; `terminal`, `open Calculator`, `snapshot -i` (33 nodes), `click @e5`, full-screen `screenshot` saved with `--out`; Stop, removal; banner and tray screenshots |
| Linux desktop (Docker, Xvfb, GNOME Calculator) | pairing, `snapshot -i`, clicks (history showed 7+5 = 12), `type`, screenshot, clipboard, terminal, Stop, removal; headless box reports only terminal/apps.launch/replay |
| Android phone emulator | pairing from the on-screen code, `snapshot`, `click`, `back`, `screenshot`, `replay` of a local script, takeover + Done on the device, Stop on the device |
| Android TV emulator | `display show` text and image, remote buttons, `screenshot`, corner badge |
| iOS Simulator (through the Mac's hosted driver) | `open Settings`, `snapshot -i`, `click`, `screenshot`, `record start/stop` |

## Not verified — as of 2026-09-26 (current list: `completion-work.md`)

- **Windows**: compiles for `x86_64-pc-windows-msvc`, pure logic unit-tested; never run on Windows.
- **Physical iPhone/iPad**: helper install (needs signing with an Apple development team), Trust and
  Developer Mode steps.
- **Real Samsung/LG TVs and Apple TV**: only mocks (and pyatv's pairing server for Apple TV crypto).
- **Physical Android phones, Fire TV, Android 11–12.**
- ~~**Mac screen recording**: still needs UI Automation enabled once
  (`automationmodetool enable-automationmode-without-authentication`); reported as missing until then.~~
  *Corrected 2026-09-27:* superseded the same day. Recording uses a native ScreenCaptureKit helper and typing the
  Accessibility helper; neither needs Xcode or UI Automation. The probe reports `screen.record` once
  Screen Recording is granted (see "Native Mac recording" below).
- **Briefcase and Ting through IAM OBO**: implemented to their documented contracts; tests used the
  local stand-ins (Briefcase's OBO upload lacks self-destruct and "make permanent" — TECHNICAL.md
  open questions 1–2). *Corrected 2026-09-27:* exercised against real local Briefcase, Ting and IAM services on
  2026-09-26/27; see the 2026-09-27 section for what passed and what still fails.
- **Space Station export**: implemented; no ingest key was available to send real events.
- **Production**: nothing deployed; no DNS, no Vercel, no Honeycomb release upload.
- ~~**Android `adb`, `install`, `logs`, screen recording**: not built in 1.0 (reported as missing, with why).~~
  *Corrected 2026-09-27:* built the same day; see the Android debugging follow-up below.

## Android debugging follow-up — 2026-09-26

The earlier “not built” entry for Android ADB, installation, logs and recording is superseded
by this follow-up. Release and physical-device gates above remain open.

- 64 Android JVM tests pass, including binary shell framing, malformed/oversized frames,
  command parsing, streaming upload bytes/checksum and upload cancellation.
- CLI unit tests (4) and CLI clippy pass. Local-input attachment handling now includes ADB
  push/install while leaving remote shell/pull paths untouched.
- Android 16 phone emulator: real local ADB shell (40 rapid commands), nonzero exit/stderr,
  140 KB binary sync round-trip, APK install/reinstall/uninstall using a generated test fixture,
  cancellation of a blocked command, logs with marker, MP4 capture, session cleanup and
  termination of the recorder when the debugging transport closes.
- Real Android TLS pairing succeeded using the pairing code/port from Settings. A separate
  app process reused the saved identity and discovered the current port with mDNS.
- Final wireless reconnect testing exposed asynchronous preference writes lost at process
  exit; connection preferences now commit on the IO dispatcher. The vendored libadb source
  also fixes a lost OPEN acknowledgement and allocates stream IDs atomically. With these
  changes, 100 rapid TLS shell commands and the complete real-service CLI suite pass.
- `e2e/android-adb.sh` passed against the local Rust Extend service and PostgreSQL with the
  Android emulator: CLI shell, install/reinstall, binary push/pull with artifact retrieval,
  logs/marker, downloaded MP4, Carbon Stop, refusal after Stop and cleanup before a new session.
  IAM, Briefcase and Ting in this run were the development stand-ins.
- Force-stopping the app during recording and reopening it exercised the cleanup of its
  recording directory. *Corrected 2026-09-27:* this is cleanup, not recovery: after an app restart the app stops the
  interrupted recorder and deletes its directory; nothing is uploaded or offered. Instrumentation
  separately proves that transport closure stops the recorder; the recorder now uses a live PTY
  instead of a detached process.
- Existing phone/emulator regression suite: 84/84 checks pass, including local Stop, refusal
  of further commands from the ended session, reconnect, revoke confirmation and re-enrollment.
  The longer setup screen exposed stale Compose accessibility descendants; snapshots now clear
  the accessibility cache on Android 13+ before traversal.
- Inspected the actual setup UI: local pairing/connection controls and connected capabilities
  render correctly. Desktop, physical hardware and production were not tested in this batch.

Limits still requiring follow-up: 8 MiB inline APK/attachment limit, Briefcase file-id input,
180-second native recording cap (lifted later that day by segmenting, below), custom recording
frame rate/app-only scope, and physical TV compatibility. These checks do not establish completion of the entire product contract.

## macOS release packaging follow-up — 2026-09-26

- Built the optimized arm64 app signed with a Developer ID Application identity from this Mac's
  keychain (which identity signs releases is a Carbon decision; see `completion-work.md`).
  The app, helper and bundled Node are signed; hardened runtime and a secure timestamp are
  present. Deep/strict signature verification and a bundled Node JavaScript execution pass.
- The bundle is 127 MB and its zip is 43 MB. Node 22.23.3 is checksum verified against pinned
  official release hashes. The fork is rebuilt rather than reusing potentially stale output.
- A packaged `probe` with PATH limited to `/usr/bin:/bin` finds the device engine 0.21.15 and
  reports the new signed identity's missing Accessibility and Screen Recording permissions.
  Those grants must be made through macOS before input/capture validation can proceed.
- Notarization support is implemented but was not run: an existing notarytool Keychain
  profile is needed. The artifact is signed, not notarized or published. ~~Recording still
  requires the UI Automation setup documented above.~~ *Corrected 2026-09-27:* it does not; see "Native Mac
  recording" below.

## macOS native text follow-up — 2026-09-26

- `fill`, `type` and `focus` now use the signed Accessibility helper without Xcode or an
  XCTest authentication prompt. Text travels over stdin; the helper verifies the target app
  and field focus before sending input. Cancellation releases any held key and exits.
- Real AppKit tests on this Mac passed Unicode (including joined emoji and Devanagari),
  long text, exact whitespace/punctuation, replacement, append, empty clearing, field
  isolation, secure-field replacement and refusal of invalid coordinates. Cancelling a
  delayed entry stopped further typing; the next entry succeeded.
- Selection uses the Accessibility text range, so clearing does not depend on an Edit menu
  implementing Command-A. Default text events are grouped to avoid macOS double-space
  punctuation substitution. Non-secure `fill` verifies the final exact value and reports a
  failure if the application transforms or rejects it. Explicit per-character delay still
  uses individual character events.
- The packaged `extend-agent exec` path passed `open`, `snapshot`, selector-based `fill`
  and `type` against the isolated fixture. Its dedicated daemon was stopped after the test.
  Run `python3 apps/desktop/macos/text-e2e.py --extend 'target/desktop/macos/Silicon Extend.app/Contents/MacOS/extend-agent'`
  after building and granting Accessibility. The test opens only its own temporary app.
- 48 focused TypeScript tests and 7 Rust probe tests pass, as do workspace TypeScript
  checking and lint. The new dispatch regression was observed failing with the original
  implementation and passing with the native route. The Swift helper build/tests pass.
  The vendor's `check:affected` selector cannot run from this nested checkout because it
  resolves the parent Git root; its required checks were selected and run directly.
- System Settings shows both requested grants enabled. The running app's status reports
  Accessibility and Screen Recording setup complete; only the separate XCTest recording
  requirement remains (*Corrected 2026-09-27:* removed later that day by native recording). A subprocess launched by the development host can report different
  screen permission status from the actual running app.
- These checks establish native AppKit input and packaged local-driver behavior. They do
  not establish arbitrary third-party app compatibility, production relay behavior,
  recording, notarization or publication.

## Session close recovery — 2026-09-26

- Mac recording exercises exposed a retained device claim whose session had been deleted
  after failed cleanup. The device engine's close path now preserves the session and ownership
  until teardown succeeds, allowing the same close operation to be retried.
- The regression failed before the fix (four failures, one pass). After the fix, 29 lifecycle
  tests pass, including failed recording finish, successful retry, device claim release and
  retaining a provider lease until cleanup succeeds. These are daemon boundary tests, not
  evidence that the intermittent native MP4 finalization failure has been resolved.

- Extend's driver setup and teardown now share a per-device queue with commands. Previously these
  hooks ran outside the command queue; the regression failed until the dispatcher performed cleanup
  before accepting the next session's commands. The fixed test verifies cleanup, new setup, then command execution. Separate
  devices still run concurrently and session end cancels pending commands. All 110 agent unit
  tests and seven fake-service tests pass. The rebuilt signed Mac app selected the expected app
  in a real recording run, but that single run does not establish that all foreground races are fixed.
- The stuck live test session `extend-c67` subsequently closed successfully through the same
  daemon, confirming cleanup retry releases a real retained claim. Extend now retries the typed
  `session_cleanup_incomplete` result once on session end and preserves state/artifacts if cleanup
  still fails (*Corrected 2026-09-27:* since 2026-09-27 a failed cleanup is followed by a forced release, and if that
  fails the computer reports its device engine capabilities as missing until a retry succeeds; see
  the 2026-09-27 section). The regression failed before this driver change and passes for recovery, persistent
  cleanup failure and a same-message error with a different reason. All 111 agent unit tests and
  seven fake-service tests pass after this change.

## Native Mac recording — 2026-09-26

- ScreenCaptureKit now captures to H.264 MP4 through the signed GUI app, without Xcode or an
  XCTest permission prompt. The runtime waits for the first frame and durable process identity;
  cancellation, recovery and export use the existing owned-process lifecycle. A failed native
  finalization is reported as failure, not uploaded as a successful video.
- The fragmented MP4 writer failed during a real changing-screen capture with AVFoundation
  `-11800` / underlying `-16341`. Ordinary MP4 output subsequently passed repeated animated
  capture and full decode checks. This is observed evidence, not proof against every encoder failure.
- Display selection formerly used the first app window, which could be empty or off-screen.
  It now chooses the largest positive window/display intersection. The new native regressions
  failed under the first-window strategy before the correction.
- Live GUI-owned tests passed manual stop and a 1.5-second duration cap. Sessions `518`, `f7e`
  and `4b3` passed app capture, real local service relay, upload, CLI download and full ffmpeg
  decode. The final run also verified changing frames and a decoded image showing only the
  fixture app against a black background. Artifacts are under
  `target/desktop/macos/recording-verification/` (ignored build output; gone by 2026-09-27).
- The local service uses development IAM/Briefcase/Ting stand-ins. These results do not prove
  production integrations, every Mac app, multi-monitor live behavior, 30-minute/1-GiB stress,
  owner-loss recovery, default touch-overlay rendering, notarization or publication. Explicit
  app targeting still uses the frontmost-app route and remains a hardening requirement.
- Validation: 39 recording runtime/ownership/recovery tests, five native helper tests, workspace
  lint/typechecking and the signed app build pass. The production bundle contains no local-test
  environment overrides. The vendor layering gate cannot complete in this nested checkout:
  249 tooling tests pass, six fail because tracked-source enumeration resolves the parent Git
  tree and `origin/main` is absent. Its final production scan therefore did not run; this gate
  remains open rather than being counted as passed.

### 2026-09-26 — named macOS app capture

- Named app opens now bind the native `app` surface. A prior frontmost-app session no longer
  overrides the named target. Explicit `.app` paths resolve bundle metadata outside standard
  installation folders; mixed-case bundle identifiers are preserved. URL-only opens still use
  the foreground route and are not covered by this fix.
- App snapshots carry the session identity to Accessibility; missing bound applications fail
  rather than substituting the foreground app. App screenshots select that app's AX windows
  from ScreenCaptureKit, excluding larger invisible backing windows. AppKit initialization
  fixes the observed `CGS_REQUIRE_INIT` crash in the independent-window filter. Explicit
  fullscreen capture remains a whole-display request; app-window crop policy remains unchanged.
- Session `e3a` passed the real local-service path after a second owned app took focus and
  overlapped the target: bound AX nodes, a nonblank screenshot, app-scoped H.264 recording,
  upload/download, changing frame hashes, and full ffmpeg decode. Both the downloaded screenshot
  and decoded video frame were visually inspected. Artifacts: ignored
  `target/desktop/macos/app-target-verification/` (gone by 2026-09-27).
- The first visual audit caught a black backing-window image despite command success. The
  harness now rejects blank screenshots. Both fixtures use Apple's `canJoinAllApplications`
  window behavior so the overlap test remains an overlap when Stage Manager is enabled.
  Earlier hidden-stage capture showed a thumbnail; full-size recording of hidden Stage Manager
  windows remains unverified and is not claimed here.
- Checks: 109 focused TypeScript tests, workspace lint/typechecking, 112 agent unit tests and
  nine helper tests pass. Native app-identity regressions failed before the fix. Secondary and
  middle click transport is preserved; those physical clicks were not part of this live run.
- A persistent test daemon retained old code across one rebuild. Its subsequent terminal
  shutdown and a fresh daemon exposed the updated behavior. Release/update handling must
  explicitly refresh idle daemons; repeatedly replacing an app bundle is not proof of refresh.
  Native recording stress/recovery, hidden-stage/multi-display coverage, URL targeting,
  notarization, other platforms, physical hardware and production integrations remain open.


### 2026-09-26 — packaged runtime build identity

- The stale-daemon cause was the installed-runtime identity contract: identical package versions
  are intentionally reused without comparing filesystem fingerprints. Extend's rebuilt fork
  previously retained upstream version `0.21.15` even when its behavior changed.
- Mac/Linux staging now appends `+extend.<sha256>` to the fork's package version. The digest
  covers the staged code/assets, canonical package metadata, Node platform/architecture/version,
  and the Mac native helper before signing. Relocation and filesystem timestamps do not affect it.
  The vendor source manifest and Extend's public release version remain unchanged.
- Four focused tests pass: identical relocated content, same-size/same-mtime code changes, native
  helper changes and rejection of incomplete staging. The real-daemon harness first reproduced
  stale reuse by unstamped A/B artifacts, then verified a changed PID and execution of build B
  after stamping. An identical relocated B reused that daemon (*Corrected 2026-09-27:* since 2026-09-27 the packaged
  entry replaces a daemon started from another install path; a relocated copy now gets its own). Cleanup used the runtime's own
  stop command against its isolated state directory; no devices or active sessions were opened.
- This proves refresh of an idle daemon for a stamped copy of the packaged runtime. *Corrected 2026-09-27:* the
  harness at the time ran under the host's Node against the source runtime; since 2026-09-27 it
  runs the packaged runtime under its bundled Node with extend-agent's environment, and it still
  does not start the real `extend-agent` or install the `.deb`. Active-session update behavior, Windows release
  handling, notarization and production publication remain separate gates. Windows uses Extend's
  native driver and does not bundle this Node daemon. Linux's packaging hook is added, but its
  full package build was not rerun in this check.
- Two successive Developer ID Mac builds retained identical runtime build identities. Deep/strict
  signature verification, bundled Node execution and archive-manifest equality passed. The final
  app contains no local-test environment overrides. Output is the signed, not-notarized
  `target/desktop/macos/Silicon Extend.app` and its 43-MB zip (*Corrected 2026-09-27:* under today's naming that zip is
  `Silicon-Extend-<version>-macos-<arch>-unnotarized.zip`).


### 2026-09-26 — Mac recording limits and owner loss

- GUI-owned session `105` passed native manual stop, a 1.5-second duration limit, a
  16-MiB file limit and abrupt recorder-owner exit. Each result was H.264 MP4 and passed
  full ffmpeg decoding. The file-limit recording stopped with `size-limit` at 12,666,951
  bytes and 52.58 seconds; owner loss stopped with `owner-exited` at 1.11 seconds.
- The same run passed named-app capture with an overlapping foreground peer and a public
  recording start/stop through the local service, upload and CLI download. Saved artifacts
  were in ignored `target/desktop/macos/record-stress-verification/` (gone by 2026-09-27).
- `text-e2e.py --record-stress --device <id>` reproduces these checks. The animated fixture
  supplies changing image content; its owner-loss case exits without signaling the recorder.
  Fixture cleanup runs even when session cleanup fails.
- A separate `--record-duration-limit` run in session `d7a` was interrupted at about
  100 seconds with service end reason `stopped_by_carbon`. The 30-minute cap is NOT verified,
  and this run is not counted as a recorder failure. No automatic restart was made.
- The full 1-GiB cap, hidden-stage/multi-display capture and default touch overlays remain
  open. Reduced limits do not close those gates. After the interruption the session was
  inactive, the test daemon was stopped through its CLI, and the app's production Info.plist
  was restored. Developer ID signature, bundled Node execution and archive byte equality
  passed; neither the app nor its zip contains local-test environment overrides.


### 2026-09-26 — native X11 recording worker

- Added the Linux native worker; it is not yet wired to public recording admission or commands.
  It captures an explicit XID or the root display with ffmpeg, encodes H.264 MP4, publishes
  atomic readiness/finalization status, limits duration/file size, and finalizes on signals or
  owning-parent exit. Linux parent-death signaling prevents a killed supervisor leaving its
  encoder running. The native artifact is retained for later runtime export.
- Live tests ran as unprivileged `carbon` in Debian trixie arm64 with Xvfb 1280x800, GTK3,
  ffmpeg 7.1.5 and an animated 641x481 fixture. Window recordings pad to 642x482; device recording
  covers 1280x800. Manual stop, 1.8-second duration limit, 1-MiB file limit and abrupt owner
  exit passed full decoding and nonblank frame checks. The reduced file cap stopped at
  793,075 bytes. Killing the supervisor with SIGKILL stopped its encoder and left a decodable MP4.
- Invalid fps/size, existing outputs, missing DISPLAY and Wayland were rejected without changing
  artifacts. Results: `/tmp/extend-linux-recording.log` (gone; see "Where the evidence is"); saved
  videos under ignored `target/desktop/linux-recording/recording-s1_mt3aa/` (also gone). Python syntax and shell syntax checks pass.
- This is a manual native-worker lane. Public start/stop, app identity binding, durable daemon
  recovery, overlay/export/transfer, obscured/hidden windows, full 30-minute/1-GiB limits, current
  Wayland portal support and release packaging remain open. No Linux capability was enabled
  solely from these native-worker results.


### 2026-09-26 — public X11 recording and export recovery

- Connected the native worker to Linux recording admission, the host process authority and
  durable runtime resources. Whole-screen `--scope device/system` supports fps, export quality
  and hide-touches. App scope is explicitly refused until isolated app capture is implemented;
  Wayland/XWayland and headless hosts do not advertise this X11 recorder. Recovery remains
  callable when start dependencies are unavailable. Worker and encoder identities are persisted.
  *Corrected 2026-09-27:* since the 2026-09-26 fork fix, Linux refuses `--quality` (Linux exports the recorder's own
  H.264 unchanged); `--fps` and `--hide-touches` remain.
- The first public stop exposed a double-signal bug: generic cleanup signaled both supervisor
  and encoder, then the supervisor signaled ffmpeg again, leaving MP4 without a moov atom.
  Linux now stops its supervisor first and only then cleans a surviving identity-matched encoder.
  Shared process cleanup keeps its original tree behavior unless the supervisor owner opts out.
- Real unprivileged Debian/Xvfb tests passed public start/stop and full decode; daemon SIGKILL
  followed by a new public stop recovered the video and reported unavailable touch events.
  Both device and system scopes ran. Native artifacts were retired only after successful export.
- Built the Linux agent and exercised Extend's driver probe and public `exec record` path.
  This exposed two existing wrapper bugs: `outPath` was not recognized, and moving the export
  invalidated the runtime's durable path. Both regressions failed before correction. Extend now
  recognizes `outPath`, copies for upload, retains the committed source and reports missing/copy
  errors instead of success with no file. Unit checks cover repeated exports and copy refusal.
- Final live run: `/tmp/extend-linux-driver-record-final.log` (gone), artifacts in ignored
  `target/desktop/linux-recording/runtime-recording-l2sf3ujf/` (gone). The driver returned one recording
  artifact, full ffmpeg decoding passed, and the manifest's source remained present. Test daemons
  and fixtures were stopped; all screen activity was inside the owned Xvfb container.
- Checks: 70 Linux/shared-recording TypeScript tests, full workspace typechecking/lint, runtime
  build, 115 agent unit tests and 7 integration tests pass. A stale clipboard test expected the
  old direct-xclip call; it now checks the existing detached-output command and stdin contract.
  The Linux probe's screenshot-independence regression also failed before correction.
- `check:affected --run` still fails before selecting gates because it resolves this nested
  workspace's root as Silicon Extend and looks for its nonexistent package.json. The depgraph
  likewise does not recognize nested tracked paths. These remain unpassed gates.
- CLI help and Linux package dependency guidance now describe the implemented whole-screen
  path. No new Linux release package or public deployment was produced. App identity/isolation,
  Wayland portal/PipeWire support, live gesture-overlay rendering, full-size/full-duration caps,
  service upload/download and current physical desktops remain open.


### 2026-09-26 — isolated X11 app recording

- Reproduced black app video with ffmpeg x11grab when an opaque peer covered the target.
  The new XComposite source captures the named window's off-screen pixels. *Corrected 2026-09-27:* a review found
  that a covered or hung app's first frames (or all frames) could show the covering window; since
  2026-09-27 the recorder waits for the app to redraw what was hidden and refuses apps that do not
  respond (2026-09-27 section). Pixmap XImages
  report zero RGB masks, so pixel interpretation uses the window's TrueColor visual.
- Public `open <app>` now retains a named app identity; URLs do not acquire one. App recording
  resolves exactly one mapped WM_CLASS matching the executable or desktop-file basename.
  Missing and ambiguous targets fail without recording the desktop. The identity survives
  durable-resource recovery. Additional app/window-manager coverage remains necessary.
- The unprivileged Xvfb public lane passed with the target covered before recording began:
  bound identity, unchanged foreground, target-colored output, full MP4 decode, native-file
  retirement, and missing/ambiguous app refusal. Evidence: `/tmp/extend-linux-public-app-final.log`
  and `target/desktop/linux-recording/public-app-recording-sfz4714y/` (both gone).
- Native isolation lanes passed target resize and unmap: both ended with `source-ended` and
  playable target-only video (`/tmp/extend-linux-isolation-resize.log` and
  `/tmp/extend-linux-isolation-unmap.log`, gone). The native lifecycle suite also passed manual stop,
  reduced duration/file limits, owner exit, supervisor SIGKILL and root-screen capture with the
  new source (`/tmp/extend-linux-composite-lifecycle.log`, gone). These are manual container lanes.
- All 64 focused Linux/host TypeScript tests, full workspace typechecking, lint and runtime build
  pass. Python syntax, package-script syntax and diff whitespace checks pass. CLI help, desktop
  instructions and Linux package dependency guidance describe the supported single-window path.
- Wayland, multiwindow apps, minimized-window starts, resize continuation, live gesture-overlay
  rendering, physical desktops, full recording caps, service transfer and release packaging remain
  open. The nested-workspace affected gate remains unavailable; no release or deployment is claimed.


### 2026-09-26 — installed Linux package and local-service recording

- Rebuilt the Linux arm64 package with the current runtime and native XComposite helper.
  The Docker packaging wrapper now rebuilds the device engine first, preventing stale dist reuse.
  Node 22.23.3 is pinned with official SHA-256 entries for Linux arm64/x64; cached and offline
  tarballs are checked. A deliberately corrupt archive was refused before assembly.
- Dependency inspection found that the old `.deb` declared no libc minimum despite this build
  needing glibc 2.39. Packaging now runs dpkg-shlibdeps over both the agent and bundled Node,
  recording the actual library packages and minimum versions. This does not establish older
  distro compatibility; build on the oldest intended distribution before widening that claim.
- Built the optimized tray-enabled agent on Debian trixie arm64, installed the `.deb`, and ran
  version, headless capability probe and terminal execution using the installed runtime. The
  final build log was kept only in `/tmp` and is gone; the 2026-09-27 reruns of this lane write
  `summary.txt` under `target/desktop/linux-recording/service-recording-*/`. Tarball: 51,414,281 bytes; `.deb`:
  33,279,808 bytes. All 595 packaged regular files match between tarball, staging and `.deb`
  staging. Artifact hashes and runtime identity were in ignored `target/desktop/linux/verification.json`
  (gone).
- New manual lane `apps/desktop/linux-e2e/record-service-e2e.py` installs the package in a fresh
  test container, runs the agent as Carbon (unprivileged), and mounts only harness/output/package,
  never the source runtime. It pairs a new owned device to the existing local development service
  and uses separate CLI homes for the development Carbon and Silicon accounts.
- Both debug and optimized packages passed app-only and whole-screen recording via the real
  Extend CLI/service/agent. Each video was uploaded, downloaded, fully decoded, checked for
  expected geometry and changing frames, then downloaded again with identical SHA-256. The
  optimized run's log (`/tmp`) and artifacts (`service-recording-rh6ph038/`) are gone; the
  2026-09-27 reruns are `target/desktop/linux-recording/service-recording-6snwvqkw/summary.txt` and
  `service-recording-nyiexg15/summary.txt`. Test sessions ended, test devices
  were removed and their containers stopped successfully. The existing service remained running.
- Shell/Python syntax and diff checks pass. These checks exercise Xvfb, headless agent operation,
  local IAM and local file storage. Production IAM/OBO/Briefcase, graphical Linux tray interaction,
  physical desktops, Wayland, x64 packages, other distributions and public publication remain open.


### 2026-09-26 — Android recordings beyond the native segment limit

- The on-device Android app previously ran one `screenrecord --time-limit 180` process. New
  `RecordingTest#beyondNativeLimit` exercised an animated test fixture for 187 seconds and failed
  against the installed old implementation: its MP4 stopped at 180,436 ms
  (`/tmp/extend-android-long-before.log`, gone).
- Added a session-owned native supervisor that sequences up to 180-second segments, stops at a
  monotonic 30-minute deadline or a reserved file-size threshold, and fences rollover before Stop.
  Child/supervisor signals require a command line containing the capture's unique directory.
  The PTY HUP trap ends capture on transport loss; source directories remain tracked for cleanup
  (an interrupted capture is stopped and deleted, not recovered).
- `RecordingMuxer` streams finalized H.264 samples through Android MediaExtractor/MediaMuxer
  into one MP4 with increasing timestamps. It checks dimensions/codec configuration, bounds sample
  buffers and final output size, observes cancellation and retains remote source segments after
  finalization failure. The stop text discloses brief gaps between native recorder restarts.
- The real long test passed with two segments, 187,824.544 ms, 2,278,619 bytes and 418 encoded
  packets, including 14 packets after 181 seconds. Full host ffmpeg decoding passed using the
  source time base; timestamps strictly increase. Evidence: `/tmp/extend-android-long-after.log`
  and `target/android-recording/long.mp4` (both gone). This long run preceded the final interruptible-wait
  cleanup fix; final-code rollover/finalization was rerun in the short lane below.
- The broader lifecycle test exposed an existing logs-start readiness race: the following marker
  could be sent before logcat was reading. Start now waits for stream data; the test uses a unique
  marker so historical log lines cannot create a false pass. The test then exposed delayed HUP
  handling while the supervisor slept. Its timer now waits interruptibly, allowing prompt cleanup.
- Final installed-code `LocalAdbTest#realLocalDaemon` passed shell, APK install/uninstall, binary
  transfer, cancellation, log marker capture, recording, session Stop, disconnect and owned-file
  cleanup (`/tmp/extend-android-segments-lifecycle-final.log`, gone). The new manual
  `e2e/android-recording.sh` short lane passed reduced-duration stop, multiple segments, full decode
  and timestamp checks (`/tmp/extend-android-final-recording-lane.log` and
  `target/android-recording/run-yPc6h1oR/`, both gone). No capture processes or remote directories remained.
- All 64 Android JVM tests and debug/app-test APK builds pass. Only emulator-5554 (Android 16)
  was modified. A connected Pixel 8 was inventoried, not exercised. Full 30-minute/1-GiB limits,
  large APKs, long-video service/Briefcase transfer, physical recording and release signing remain
  unverified. No production deployment or release is claimed.


### 2026-09-26 — Android long recording through the local service and timing correction

- Added `e2e/android-recording-service.py`, using the existing paired emulator, isolated CLI homes
  and local IAM users. It opens the test fixture, records for 187 seconds, runs public record stop
  with download, decodes the full MP4, checks late animated frames and duration, then verifies
  identical additional downloads as the Silicon and device-owning Carbon. Sessions and fixtures
  are cleaned up; the existing pairing is retained.
- The first relay/upload/download run succeeded but was not accepted as complete recording proof:
  a 187-second request exported a 206.369589-second MP4. The fixture process had also crashed with
  `NoClassDefFoundError: kotlin/jvm/internal/Intrinsics`. The test APK launches it separately from
  the target app, so Kotlin classes supplied only by the target APK were unavailable. Earlier tests
  did not check that the requested animated fixture stayed alive; they therefore do not establish
  the intended animated scene even where video export/decode passed.
- Replaced that fixture with a standalone Android/Java activity. The service lane now checks its
  process, more than 500 encoded frames, multiple frames after 181 seconds and a bounded overall
  duration. The final run's foreground activity was verified as RecordingFixtureActivity.
- Fixed sparse-video timeline inflation. Each native segment writes monotonic start/end timing
  and its requested native limit. The muxer bounds samples and durations by that evidence,
  preserves the declared duration within those bounds, and writes an explicit end-of-stream
  timestamp so the last frame gap is not extrapolated. Android's
  [MediaMuxer contract](https://developer.android.com/reference/android/media/MediaMuxer)
  documents explicit final-sample duration using this timestamp. Invalid timing evidence fails
  finalization while retaining source files.
- Final result: 186.9295 seconds, 43,286,430 bytes, 2,104 packets, final frame at 186.830433 seconds;
  full decoding and strictly increasing timestamps pass. Creator and Carbon downloads have SHA-256
  `0e98634cfbc0fd74b897c42fd775772c215ddfb6c693170c328acb3f40b552a5`.
  Evidence: `/tmp/extend-android-service-long-final.log` and
  `target/android-recording/service-ghqrk9z3/` (both gone; the SHA-256 above is the record).
- All 64 JVM tests, app/test APK builds and the strengthened reduced-duration native timing test
  pass (`/tmp/extend-android-service-final-build.log`, `/tmp/extend-android-sparse-bounded.log`, gone).
  After cleanup there were no capture processes or capture directories. Instrumentation/reinstall
  had left previously granted accessibility/notification listeners unbound; only the emulator's
  existing Extend grants were rebound. The physical Pixel 8 was not modified.
- This closes local long-recording relay/upload/download verification on Android 16. Production
  IAM/OBO/Briefcase, full 30-minute/1-GiB tests, physical phones/TVs and release signing remain open.
