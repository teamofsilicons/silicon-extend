# Silicon Bridge for Android (phones, tablets, Android TV, Google TV, Fire OS)

One APK, package `com.teamofsilicons.bridge`, minSdk 30 (Android 11), target/compile SDK 36. It
pairs the device with Bridge, keeps it connected, shows who is using it with a Stop button, and runs
a Silicon's agent-device commands on the device. It speaks exactly `docs/device-protocol.md`.

- **Phone/tablet** (`os: "android"`): pairing code → setup steps → paired screen; in-use
  notification with **Stop**.
- **TV** (`os: "android_tv"`, chosen at runtime when `UiModeManager` reports a television, or the
  device has `leanback`/`television`/`amazon.hardware.fire_tv`): very large pairing code, corner
  badge while a Silicon uses the TV, remote buttons, full-screen display. Listed in the TV launcher
  (`LEANBACK_LAUNCHER`, banner); touchscreen and leanback are not required features.

## Build

There is no system Java on the build Mac; use any JDK 17+ (the Homebrew `openjdk@17` keg works):

```sh
cd apps/android
export JAVA_HOME=/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home
./gradlew :app:testDebugUnitTest          # JVM unit tests
./gradlew :app:assembleDebug              # app/build/outputs/apk/debug/app-debug.apk
./gradlew :app:assembleRelease            # app/build/outputs/apk/release/app-release.apk (debug-signed, not minified)
```

`local.properties` points at `~/Library/Android/sdk` (not committed). Toolchain: AGP 8.13.2,
Gradle 8.14.3 (wrapper included), Kotlin 2.2.20, Compose BOM 2025.09.00, OkHttp 4.12,
kotlinx.serialization 1.9.

Service URL: release builds default to `https://backend.bridge.teamofsilicons.com`. Debug builds
default to the same unless built with `-PbridgeServiceUrl=http://10.0.2.2:8480`. Cleartext HTTP is
allowed only for `10.0.2.2`, `localhost` and `127.0.0.1` (`res/xml/network_security_config.xml`).

## Install and run

```sh
ADB=~/Library/Android/sdk/platform-tools/adb
$ADB install -r -g app/build/outputs/apk/debug/app-debug.apk
# Debug builds only: point at a local service / behave as a TV / start unpaired.
$ADB shell am start -n com.teamofsilicons.bridge/.ui.MainActivity \
  --es service_url http://10.0.2.2:8480 --ez force_tv false --ez forget_pair true
```

Release builds ignore those extras: an exported intent that could move a paired device to another
service would let any installed app take the device over. In every build, the **hidden developer
settings** open after tapping the version line at the bottom of the app 7 times (service URL,
"behave as a TV"). Changing the URL forgets the pair locally.

What a Carbon does on a real device: open the app, enter the code on the website, then follow the
setup cards (each has a button to the exact settings page and the path written out). On emulators
the same can be done with adb:

```sh
$ADB shell settings put secure enabled_accessibility_services com.teamofsilicons.bridge/com.teamofsilicons.bridge.a11y.BridgeAccessibilityService
$ADB shell settings put secure accessibility_enabled 1
$ADB shell cmd notification allow_listener com.teamofsilicons.bridge/com.teamofsilicons.bridge.notif.BridgeNotificationListener
$ADB shell dumpsys deviceidle whitelist +com.teamofsilicons.bridge
```

Sideloaded APKs on Android 13+ must first get **Allow restricted settings** (Settings › Apps ›
Silicon Bridge › ⋮) before Android lets the Carbon turn on the accessibility service or notification
access; the setup help says so.

## Tests

- **JVM unit tests** (`app/src/test`, 58 tests): every frame example in `docs/device-protocol.md`
  decoded/encoded (plus the `{"type","data"}` envelope form and unknown frames), argument parsing for
  every command (and the refusals), selector parsing/matching, snapshot filtering/ref assignment/
  text/JSON/diff, alert detection, `.ad`/batch parsing, reconnect backoff.
- **Fake service** `tools/fake-service/fake_bridge.py` (Python standard library only: HTTP + a
  minimal WebSocket). It implements the device half of the protocol, checks upload digests, and
  runs scenarios. `tools/fake-service/run-emulator-test.sh phone|tv` does everything on a running
  emulator: install, launch pointed at the fake, check the code on screen equals the service's, grant
  access with adb, claim the code, run the scenario (83 checks on a phone, 36 on a TV).
  `tools/fake-service/bcmd <command> [args…]` sends one command to a device paired with a running
  fake (`/_test/command`); `/_test/frame` and `/_test/close` send any frame or close code.
- **Real service** (`crates/bridge-service` on `http://127.0.0.1:8480`, emulator `10.0.2.2:8480`,
  local IAM mode) with the `bridge` CLI: the code read off the device's screen claimed with
  `POST /api/v1/pairings` as `c:alice`; the device came online `ready` with the reported
  capabilities/missing; as `si:chef`: `session new --connect`, `open`, `snapshot -i`, `click @e7`,
  `back`, `screenshot` (stored), `replay <local .ad>` (sent as an attachment), `notifications`,
  `takeover --reason` → Done on the device resumes the session, Stop on the device ends it
  (`stopped_by_carbon`); on the TV emulator: `android_tv`, `display show --text|--image <local png>`,
  `tv-remote press back|down`, `screenshot`.

## Decisions

1. **An AccessibilityService, not ADB over loopback, reads and acts on the screen.** An app can't
   drive its own device's ADB without the wireless-debugging pairing dance, it breaks on every
   reboot, and needs Wi-Fi. The accessibility tree gives the same element list agent-device's
   Android helper reads (it is itself built on `UiAutomation`), `dispatchGesture` taps/swipes,
   `performGlobalAction` presses back/home/recents/D-pad, and `takeScreenshot` (API 30+) captures
   the screen. The service is also what Android allows to start activities from the background,
   which `open`, `display` and `clipboard read` need.
2. **agent-device semantics, re-implemented in Kotlin.** The snapshot follows agent-device's Android
   presentation (`ui-hierarchy-inclusion.ts`, `snapshot-lines.ts`): label = text or content
   description, value = text, identifier = resource id; `-i` keeps touch/focus targets and their
   labelled proxies; unlabelled groups fold away; refs `@e1…` in document order, held per session
   until the next snapshot; capped at 5000 nodes (`truncated`). Selectors are agent-device's grammar
   (`role="button" label="Continue" || text=Next`, boolean keys, case/space-insensitive equality);
   `role` matches the Android class (`button`, `edittext`) or the display role (`text-field`); `id`
   also matches the bare entry name. `find` scores exact over contains and rejects ambiguity unless
   `--first/--last`. `agent_device_version` is sent as null because agent-device itself isn't
   embedded.
3. **Capabilities are exactly what works now.** Accessibility-backed capabilities appear only while
   the service is connected; notifications only while notification access is connected. The app
   sends `hello` again when capabilities change and `setup_progress` when only setup changes (polled
   every 3 s and on service connect/disconnect). A command whose capabilities the device doesn't
   report returns `unsupported_on_device` with the `missing` reason.
4. **Setup state.** Required steps: accessibility, notifications (phones, API 33+), background use
   (battery optimisation), notification access (phones). Developer options and wireless/network
   debugging are listed with their real status (`done` or `todo`). Core accessibility control
   remains available without debugging; installation, logs and recording require its connection.
5. **TV in-use badge** is a `TYPE_ACCESSIBILITY_OVERLAY` window (no "display over other apps"
   permission), not focusable or touchable, left out of snapshots.
6. **Credential storage**: AES-256-GCM key in the Android Keystore; only ciphertext on disk
   (EncryptedSharedPreferences is deprecated). The enrollment secret stays in memory; an abandoned
   enrollment is discarded with `DELETE /api/v1/enrollments/{id}`.
7. **Foreground service type `specialUse`** (allowed to start from `BOOT_COMPLETED` on Android 15+),
   started at boot and after app updates when paired.
8. **Attachments** (`docs/device-protocol.md`, "Attachments") are written to a per-command scratch
   directory and `attachment:<name>` arguments replaced by the path; the directory is deleted after
   the command (the display keeps its own copy).
9. **Cancel** cancels the running command and sends no `result` (the service already answered
   `command_timeout`). Commands run one at a time. The command budget is `timeout_ms − 750 ms`.

## What works (verified on emulators)

Phone (Android 16 emulator) and TV (Android TV 14 emulator):

- Pairing: `POST /enrollments`, code shown large (TV: very large), rotations and pings on the
  enrollment socket, polling fallback, new enrollment on 401/404, `paired` → Keystore → device
  socket. Never asks for a login.
- Device socket: hello, ping/pong, reconnect with 1–60 s full-jitter backoff (and immediately when
  the network returns), close codes 4401 (forget → pairing), 4409 (stop, show Reconnect), 4426
  (stop, "Update Silicon Bridge"), `superseded`, `unpaired`, `refresh` → `GET /device`,
  `environment` → permanent banner, start at boot and after updates.
- Paired screen: device name, owner, team, connection, who is using it, Stop, takeover reason + Done
  (`takeover_done`), setup cards with deep links, Revoke pair with confirmation (`DELETE /device`).
  Phone notification "si:chef is using this device" with Stop (tested from the shade); TV badge.
- Commands: `snapshot` (`-i -d -s --raw --diff`), `diff snapshot`, `get text|attrs`, `find` (all
  locators/actions), `is` (all predicates), `wait` (ms, text, ref, selector, absent), `screenshot`
  (`--scale`, `--overlay-refs`, `--crop-on`; uploaded with SHA-256), `click`/`press` (ref, selector,
  x y, `--count/--hold-ms/--interval-ms`), `longpress`, `fill` (verified read-back), `type`,
  `focus`, `scroll` (up/down/left/right/top/bottom, fraction, `--pixels`, keyboard-aware),
  `swipe` (count, pause, ping-pong), `gesture pan|fling|pinch|rotate|drag`, `back`, `home`,
  `app-switcher`, `open` (package, app label, URL, app+URL), `close`, `apps [--all]`, `appstate`,
  `alert get|wait|accept|dismiss` (permission prompts, AlertDialogs, small dialog windows),
  `keyboard status|dismiss`, `clipboard read|write`, `notifications`, `batch --steps`, `replay`,
  `test`; TV: `tv-remote press up|down|left|right|select|back|home|play-pause|volume-up|volume-down|mute`,
  `tv-remote longpress select`, `display show --url|--image|--video|--text`, `display clear`, Back
  clears the display.

## What doesn't (reported in `missing` or as `unsupported_on_device`, with the reason)

- `tv-remote press menu|power` and long-presses other than select: an accessibility service can't
  send those keys (power is also refused on purpose: nothing could turn the TV back on).
- `hover`, `click --button secondary`, `open --surface` (computer-only), `gesture transform`,
  `replay --from/--plan-digest`, Maestro flows, `close --save-script`, `diff screenshot`,
  `snapshot --actions` (iOS-only in agent-device too), batch steps in agent-device's structured
  `input` form (use `{"command","args"}`).
- `clipboard read` briefly takes window focus (Android 10+ only lets the focused app read the
  clipboard), which can close the keyboard or a menu in the app underneath.
- `close` goes home and ends background processes; force-stop needs Android debugging.
- Not verified: physical devices, physical Fire TV (`amazon.hardware.fire_tv` detection and the
  Fire OS settings paths are from documentation), Android 11–12 devices (minSdk 30; D-pad buttons
  need Android 13+, and older TVs report `input.remote` missing), video playback in `display`.

## Android debugging (2026-09-26 follow-up)

The paired app now has an **Android debugging** setup card. On Android 11+, enable Wireless
Debugging, keep Android's pairing-code dialog beside Bridge in split screen, and enter Android's
pairing port and six-digit code. These are separate from the Bridge enrollment code. After pairing,
Bridge discovers this device's connection port; it also accepts the port shown on the main Wireless
Debugging screen. TVs with TCP debugging can connect to their local port (commonly 5555) and approve
Android's RSA prompt. Connections are restricted to loopback, and discovery only accepts this
host's own addresses. Credentials are encrypted with the Android Keystore. Reconnect runs while
paired; after a reboot the owner may still need to enable Wireless Debugging again.

Connected debugging enables `adb`, `install`/`reinstall`, `logs`, and phone `record` commands.
Accessibility remains the semantic screen/input driver. Capabilities are withdrawn when debugging
is disconnected. The app never opens a debugging connection in response to a remote command.

- `bridge adb shell <command>` preserves exit failures; `exec-out` returns a binary artifact.
- `bridge adb push <local file> <device path>` and `pull <device path> --out <local file>` use ADB
  sync and Bridge's artifact uploads. The CLI attaches local inputs for ADB push/install only.
- `bridge install <package.name> <local.apk>` checks APK package identity before installation;
  `reinstall` requests replacement. `adb install [-r] <local.apk>` and `adb uninstall <package>`
  are also supported. The existing service limit of 8 MiB total inline attachments still applies;
  large APKs and Briefcase file-id inputs remain follow-up work.
- `bridge logs start`, `mark <label>`, `stop --out <file>`, `clear` stream up to 16 MiB of device
  logs. The app closes the live stream on session end or disconnect.
- `bridge record start [name] [--scope device] [--quality normal|high]` and
  `record stop --out <file.mp4>` use supervised screenrecord segments (up to 180 seconds each),
  combined into one MP4 on stop. The supervisor bounds the overall capture to 30 minutes or
  1 GiB; native restarts can leave brief capture gaps, disclosed in the stop result. Encoder or
  dimension changes between segments fail finalization and retain source segments for retry.
  App-only scope and custom frame rates are explicitly unsupported by this backend.
  Recordings and pulled files are streamed from disk during upload. Stop, revoke and connection
  loss clean up session-owned capture processes. Recorder PID checks include its unique output
  directory, so cleanup cannot signal a recycled PID. Interrupted recording directories are
  tracked for cleanup on the next debugging connection.

The ADB implementation uses the patched libadb-android 3.1.1 source in `vendor/libadb`
(Apache-2.0 option; upstream BSD notices retained), Conscrypt 2.5.3 and
BouncyCastle 1.81. APKs remain development-signed; release signing is a separate gate.

Verification commands:

```sh
./gradlew testDebugUnitTest assembleDebug assembleDebugAndroidTest
# On the dedicated test emulator, enable TCP debugging once and approve the test app's RSA key.
adb tcpip 5555
adb shell am instrument -w -e class com.teamofsilicons.bridge.LocalAdbTest#realLocalDaemon \
  com.teamofsilicons.bridge.test/androidx.test.runner.AndroidJUnitRunner
# With this device paired to the local Bridge backend and debugging connected:
../../e2e/android-adb.sh <device-id>
```

`LocalAdbTest#wirelessPairing` additionally accepts `adb_pairing_port`, `adb_pairing_code`, and
optionally `adb_connect_port` instrumentation arguments. `#wirelessReconnect -e adb_tls true`
verifies mDNS discovery and reuse of the saved identity in a new app process. The test-only APK
fixture has no executable code or runtime permissions and is uninstalled after the test.


Long Android recording verification (dedicated emulator only): build/install the app and
instrumentation APK, then run `RUN_LONG=1 bash e2e/android-recording.sh emulator-5554` from
the repository root. The underlying instrumentation uses `RecordingTest#nativeDurationLimit` for reduced automatic-stop
and segment-rollover coverage. `RecordingTest#beyondNativeLimit` with `-e long_recording true`
runs for about 190 seconds and requires real frames after the native 180-second boundary.
Both use an animated fixture in the test APK. Proof videos are saved under the target app's
external files directory. Decode variable-rate Android video with its source time base, e.g.
`ffmpeg -v error -i proof.mp4 -enc_time_base demux -fps_mode passthrough -f null -`.
These are manual instrumentation lanes; full 30-minute/1-GiB and physical-device recording
coverage remain separate gates.
