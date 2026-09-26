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
   debugging are listed with their real status (`done` or `todo`) and marked optional, because
   nothing in this version uses them; `setup.state` is `complete` once the required steps are done.
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

- `screen.record` (`record`): MediaProjection needs the Carbon's consent prompt for every recording;
  no way around it without Android debugging.
- `adb`, `apps.install` (`install`/`reinstall`), `logs`: the wireless-debugging bridge (an on-device
  ADB client with TLS pairing) is not built in this version. This is what Developer options and
  wireless/network debugging will be for.
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
