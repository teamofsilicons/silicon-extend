# Verification record — 2026-09-26

What was run to check that Silicon Bridge works as `understanding/UNDERSTANDING.md` intends, on what,
and what has **not** been verified. Rerun the automated part with `e2e/run-all.sh`.

## Automated suites

| Suite | Where | Result |
|---|---|---|
| Protocol, service, client, CLI unit tests | `cargo test --workspace` | pass |
| Service end-to-end (`crates/bridge-service/tests/e2e.rs`) | real PostgreSQL, real HTTP/WS, scripted device | 4/4 suites: pairing, one-at-a-time, relay, files + self-destruct, takeover, stop, access removal mid-session, idle timeout, logout, pair expiry, supersede, session ids growing to 4 chars, rate limits, test environments (limit message, isolation, clean/disable/restore/purge), version negotiation |
| CLI end-to-end (`e2e/cli-e2e.sh`) | running service + `examples/fake_device` | 48/48 checks: help tree, login/status, pairing, device management, sessions, `--help` narrowing, exit codes, files, requests via Ting, takeover, activity redaction, reports, `--test` environments and isolation |
| Real Silicon IAM (`e2e/real-iam/realiam.py all`) | local IAM containers + Bridge in SDK mode | 51/51: real SLT login, refresh/reuse, revocation, directory, access grants, signed webhooks (removal ends access; forged 401; duplicates once), testing plane with member-id login |
| Website (`web`) | vitest, Playwright vs mock, Playwright vs real service | 78 unit, 23 mock e2e, 4 real-service e2e |
| Desktop agent (`crates/bridge-agent`) | macOS + Linux (Docker, arm64) | 107 unit + 7 integration (fake service) |
| Hosted drivers (`crates/bridge-hosted`) | mocks, iOS Simulator | 63 tests; Apple TV pairing crypto against pyatv's server (opt-in) |
| Android (`apps/android`) | JVM, phone emulator (API 36), Android TV emulator (API 34) | 58 unit; 83/83 phone and 36/36 TV checks vs fake service |

## Real devices, through the real service and the `bridge` CLI

| Device | What ran |
|---|---|
| This Mac (macOS 27) | paired via CLI; `terminal`, `open Calculator`, `snapshot -i` (33 nodes), `click @e5`, full-screen `screenshot` saved with `--out`; Stop, removal; banner and tray screenshots |
| Linux desktop (Docker, Xvfb, GNOME Calculator) | pairing, `snapshot -i`, clicks (history showed 7+5 = 12), `type`, screenshot, clipboard, terminal, Stop, removal; headless box reports only terminal/apps.launch/replay |
| Android phone emulator | pairing from the on-screen code, `snapshot`, `click`, `back`, `screenshot`, `replay` of a local script, takeover + Done on the device, Stop on the device |
| Android TV emulator | `display show` text and image, remote buttons, `screenshot`, corner badge |
| iOS Simulator (through the Mac's hosted driver) | `open Settings`, `snapshot -i`, `click`, `screenshot`, `record start/stop` |

## Not verified

- **Windows**: compiles for `x86_64-pc-windows-msvc`, pure logic unit-tested; never run on Windows.
- **Physical iPhone/iPad**: helper install (needs signing with an Apple development team), Trust and
  Developer Mode steps.
- **Real Samsung/LG TVs and Apple TV**: only mocks (and pyatv's pairing server for Apple TV crypto).
- **Physical Android phones, Fire TV, Android 11–12.**
- **Mac screen recording**: still needs UI Automation enabled once
  (`automationmodetool enable-automationmode-without-authentication`); reported as missing until then.
  Typing no longer uses that runner; see the native text follow-up below.
- **Briefcase and Ting through IAM OBO**: implemented to their documented contracts; tests used the
  local stand-ins (Briefcase's OBO upload lacks self-destruct and "make permanent" — TECHNICAL.md
  open questions 1–2).
- **Space Station export**: implemented; no ingest key was available to send real events.
- **Production**: nothing deployed; no DNS, no Vercel, no Honeycomb release upload.
- **Android `adb`, `install`, `logs`, screen recording**: not built in 1.0 (reported as missing, with why).

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
- `e2e/android-adb.sh` passed against the local Rust Bridge service and PostgreSQL with the
  Android emulator: CLI shell, install/reinstall, binary push/pull with artifact retrieval,
  logs/marker, downloaded MP4, Carbon Stop, refusal after Stop and cleanup before a new session.
  IAM, Briefcase and Ting in this run were the development stand-ins.
- Force-stopping the app during recording and reopening it exercised recovery of its saved
  recording directory. Instrumentation separately proves that transport closure stops the
  recorder; the recorder now uses a live PTY instead of a detached process.
- Existing phone/emulator regression suite: 84/84 checks pass, including local Stop, refusal
  of further commands from the ended session, reconnect, revoke confirmation and re-enrollment.
  The longer setup screen exposed stale Compose accessibility descendants; snapshots now clear
  the accessibility cache on Android 13+ before traversal.
- Inspected the actual setup UI: local pairing/connection controls and connected capabilities
  render correctly. Desktop, physical hardware and production were not tested in this batch.

Limits still requiring follow-up: 8 MiB inline APK/attachment limit, Briefcase file-id input,
180-second native recording cap, custom recording frame rate/app-only scope, and physical TV
compatibility. These checks do not establish completion of the entire product contract.

## macOS release packaging follow-up — 2026-09-26

- Built the optimized arm64 app with Developer ID Application: Shubham Gupta (LTBSK59BJ2).
  The app, helper and bundled Node are signed; hardened runtime and a secure timestamp are
  present. Deep/strict signature verification and a bundled Node JavaScript execution pass.
- The bundle is 127 MB and its zip is 43 MB. Node 22.23.3 is checksum verified against pinned
  official release hashes. The fork is rebuilt rather than reusing potentially stale output.
- A packaged `probe` with PATH limited to `/usr/bin:/bin` finds agent-device 0.21.15 and
  reports the new signed identity's missing Accessibility and Screen Recording permissions.
  Those grants must be made through macOS before input/capture validation can proceed.
- Notarization support is implemented but was not run: an existing notarytool Keychain
  profile is needed. The artifact is signed, not notarized or published. Recording still
  requires the UI Automation setup documented above.

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
- The packaged `bridge-agent exec` path passed `open`, `snapshot`, selector-based `fill`
  and `type` against the isolated fixture. Its dedicated daemon was stopped after the test.
  Run `python3 apps/desktop/macos/text-e2e.py --bridge 'target/desktop/macos/Silicon Bridge.app/Contents/MacOS/bridge-agent'`
  after building and granting Accessibility. The test opens only its own temporary app.
- 48 focused TypeScript tests and 7 Rust probe tests pass, as do workspace TypeScript
  checking and lint. The new dispatch regression was observed failing with the original
  implementation and passing with the native route. The Swift helper build/tests pass.
  The vendor's `check:affected` selector cannot run from this nested checkout because it
  resolves the parent Git root; its required checks were selected and run directly.
- System Settings shows both requested grants enabled. The running app's status reports
  Accessibility and Screen Recording setup complete; only the separate XCTest recording
  requirement remains. A subprocess launched by the development host can report different
  screen permission status from the actual running app.
- These checks establish native AppKit input and packaged local-driver behavior. They do
  not establish arbitrary third-party app compatibility, production relay behavior,
  recording, notarization or publication.

## Session close recovery — 2026-09-26

- Mac recording exercises exposed a retained device claim whose session had been deleted
  after failed cleanup. The agent-device close path now preserves the session and ownership
  until teardown succeeds, allowing the same close operation to be retried.
- The regression failed before the fix (four failures, one pass). After the fix, 29 lifecycle
  tests pass, including failed recording finish, successful retry, device claim release and
  retaining a provider lease until cleanup succeeds. These are daemon boundary tests, not
  evidence that the intermittent native MP4 finalization failure has been resolved.

- Bridge's driver setup and teardown now share a per-device queue with commands. Previously these
  hooks ran outside the command queue; the regression failed until the dispatcher performed cleanup
  before accepting the next session's commands. The fixed test verifies cleanup, new setup, then command execution. Separate
  devices still run concurrently and session end cancels pending commands. All 110 agent unit
  tests and seven fake-service tests pass. The rebuilt signed Mac app selected the expected app
  in a real recording run, but that single run does not establish that all foreground races are fixed.
- The stuck live test session `bridge-c67` subsequently closed successfully through the same
  daemon, confirming cleanup retry releases a real retained claim. Bridge now retries the typed
  `session_cleanup_incomplete` result once on session end and preserves state/artifacts if cleanup
  still fails. The regression failed before this driver change and passes for recovery, persistent
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
  `target/desktop/macos/recording-verification/` (ignored build output).
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
  `target/desktop/macos/app-target-verification/`.
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
  are intentionally reused without comparing filesystem fingerprints. Bridge's rebuilt fork
  previously retained upstream version `0.21.15` even when its behavior changed.
- Mac/Linux staging now appends `+bridge.<sha256>` to the fork's package version. The digest
  covers the staged code/assets, canonical package metadata, Node platform/architecture/version,
  and the Mac native helper before signing. Relocation and filesystem timestamps do not affect it.
  The vendor source manifest and Bridge's public release version remain unchanged.
- Four focused tests pass: identical relocated content, same-size/same-mtime code changes, native
  helper changes and rejection of incomplete staging. The real-daemon harness first reproduced
  stale reuse by unstamped A/B artifacts, then verified a changed PID and execution of build B
  after stamping. An identical relocated B reused that daemon. Cleanup used the runtime's own
  stop command against its isolated state directory; no devices or active sessions were opened.
- This proves refresh of an idle packaged daemon. Active-session update behavior, Windows release
  handling, notarization and production publication remain separate gates. Windows uses Bridge's
  native driver and does not bundle this Node daemon. Linux's packaging hook is added, but its
  full package build was not rerun in this check.
- Two successive Developer ID Mac builds retained identical runtime build identities. Deep/strict
  signature verification, bundled Node execution and archive-manifest equality passed. The final
  app contains no local-test environment overrides. Output is the signed, not-notarized
  `target/desktop/macos/Silicon Bridge.app` and its 43-MB zip.


### 2026-09-26 — Mac recording limits and owner loss

- GUI-owned session `105` passed native manual stop, a 1.5-second duration limit, a
  16-MiB file limit and abrupt recorder-owner exit. Each result was H.264 MP4 and passed
  full ffmpeg decoding. The file-limit recording stopped with `size-limit` at 12,666,951
  bytes and 52.58 seconds; owner loss stopped with `owner-exited` at 1.11 seconds.
- The same run passed named-app capture with an overlapping foreground peer and a public
  recording start/stop through the local service, upload and CLI download. Saved artifacts
  are in ignored `target/desktop/macos/record-stress-verification/`.
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
  artifacts. Results: `/tmp/bridge-linux-recording.log`; saved videos under ignored
  `target/desktop/linux-recording/recording-s1_mt3aa/`. Python syntax and shell syntax checks pass.
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
- The first public stop exposed a double-signal bug: generic cleanup signaled both supervisor
  and encoder, then the supervisor signaled ffmpeg again, leaving MP4 without a moov atom.
  Linux now stops its supervisor first and only then cleans a surviving identity-matched encoder.
  Shared process cleanup keeps its original tree behavior unless the supervisor owner opts out.
- Real unprivileged Debian/Xvfb tests passed public start/stop and full decode; daemon SIGKILL
  followed by a new public stop recovered the video and reported unavailable touch events.
  Both device and system scopes ran. Native artifacts were retired only after successful export.
- Built the Linux agent and exercised Bridge's driver probe and public `exec record` path.
  This exposed two existing wrapper bugs: `outPath` was not recognized, and moving the export
  invalidated the runtime's durable path. Both regressions failed before correction. Bridge now
  recognizes `outPath`, copies for upload, retains the committed source and reports missing/copy
  errors instead of success with no file. Unit checks cover repeated exports and copy refusal.
- Final live run: `/tmp/bridge-linux-driver-record-final.log`, artifacts in ignored
  `target/desktop/linux-recording/runtime-recording-l2sf3ujf/`. The driver returned one recording
  artifact, full ffmpeg decoding passed, and the manifest's source remained present. Test daemons
  and fixtures were stopped; all screen activity was inside the owned Xvfb container.
- Checks: 70 Linux/shared-recording TypeScript tests, full workspace typechecking/lint, runtime
  build, 115 agent unit tests and 7 integration tests pass. A stale clipboard test expected the
  old direct-xclip call; it now checks the existing detached-output command and stdin contract.
  The Linux probe's screenshot-independence regression also failed before correction.
- `check:affected --run` still fails before selecting gates because it resolves this nested
  workspace's root as Silicon Bridge and looks for its nonexistent package.json. The depgraph
  likewise does not recognize nested tracked paths. These remain unpassed gates.
- CLI help and Linux package dependency guidance now describe the implemented whole-screen
  path. No new Linux release package or public deployment was produced. App identity/isolation,
  Wayland portal/PipeWire support, live gesture-overlay rendering, full-size/full-duration caps,
  service upload/download and current physical desktops remain open.


### 2026-09-26 — isolated X11 app recording

- Reproduced black app video with ffmpeg x11grab when an opaque peer covered the target.
  The new XComposite source captures the named window's off-screen pixels. Pixmap XImages
  report zero RGB masks, so pixel interpretation uses the window's TrueColor visual.
- Public `open <app>` now retains a named app identity; URLs do not acquire one. App recording
  resolves exactly one mapped WM_CLASS matching the executable or desktop-file basename.
  Missing and ambiguous targets fail without recording the desktop. The identity survives
  durable-resource recovery. Additional app/window-manager coverage remains necessary.
- The unprivileged Xvfb public lane passed with the target covered before recording began:
  bound identity, unchanged foreground, target-colored output, full MP4 decode, native-file
  retirement, and missing/ambiguous app refusal. Evidence: `/tmp/bridge-linux-public-app-final.log`,
  artifacts `target/desktop/linux-recording/public-app-recording-sfz4714y/`.
- Native isolation lanes passed target resize and unmap: both ended with `source-ended` and
  playable target-only video (`/tmp/bridge-linux-isolation-resize.log` and
  `/tmp/bridge-linux-isolation-unmap.log`). The native lifecycle suite also passed manual stop,
  reduced duration/file limits, owner exit, supervisor SIGKILL and root-screen capture with the
  new source (`/tmp/bridge-linux-composite-lifecycle.log`). These are manual container lanes.
- All 64 focused Linux/host TypeScript tests, full workspace typechecking, lint and runtime build
  pass. Python syntax, package-script syntax and diff whitespace checks pass. CLI help, desktop
  instructions and Linux package dependency guidance describe the supported single-window path.
- Wayland, multiwindow apps, minimized-window starts, resize continuation, live gesture-overlay
  rendering, physical desktops, full recording caps, service transfer and release packaging remain
  open. The nested-workspace affected gate remains unavailable; no release or deployment is claimed.


### 2026-09-26 — installed Linux package and local-service recording

- Rebuilt the Linux arm64 package with the current runtime and native XComposite helper.
  The Docker packaging wrapper now rebuilds agent-device first, preventing stale dist reuse.
  Node 22.23.3 is pinned with official SHA-256 entries for Linux arm64/x64; cached and offline
  tarballs are checked. A deliberately corrupt archive was refused before assembly.
- Dependency inspection found that the old `.deb` declared no libc minimum despite this build
  needing glibc 2.39. Packaging now runs dpkg-shlibdeps over both the agent and bundled Node,
  recording the actual library packages and minimum versions. This does not establish older
  distro compatibility; build on the oldest intended distribution before widening that claim.
- Built the optimized tray-enabled agent on Debian trixie arm64, installed the `.deb`, and ran
  version, headless capability probe and terminal execution using the installed runtime. Final
  build log: `/tmp/bridge-linux-package-release-final.log`. Tarball: 51,414,281 bytes; `.deb`:
  33,279,808 bytes. All 595 packaged regular files match between tarball, staging and `.deb`
  staging. Artifact hashes and runtime identity are in ignored `target/desktop/linux/verification.json`.
- New manual lane `apps/desktop/linux-e2e/record-service-e2e.py` installs the package in a fresh
  test container, runs the agent as Carbon (unprivileged), and mounts only harness/output/package,
  never the source runtime. It pairs a new owned device to the existing local development service
  and uses separate CLI homes for the development Carbon and Silicon accounts.
- Both debug and optimized packages passed app-only and whole-screen recording via the real
  Bridge CLI/service/agent. Each video was uploaded, downloaded, fully decoded, checked for
  expected geometry and changing frames, then downloaded again with identical SHA-256. The
  optimized evidence is `/tmp/bridge-linux-service-record-release.log` and ignored artifacts
  `target/desktop/linux-recording/service-recording-rh6ph038/`. Test sessions ended, test devices
  were removed and their containers stopped successfully. The existing service remained running.
- Shell/Python syntax and diff checks pass. These checks exercise Xvfb, headless agent operation,
  local IAM and local file storage. Production IAM/OBO/Briefcase, graphical Linux tray interaction,
  physical desktops, Wayland, x64 packages, other distributions and public publication remain open.


### 2026-09-26 — Android recordings beyond the native segment limit

- The on-device Android app previously ran one `screenrecord --time-limit 180` process. New
  `RecordingTest#beyondNativeLimit` exercised an animated test fixture for 187 seconds and failed
  against the installed old implementation: its MP4 stopped at 180,436 ms
  (`/tmp/bridge-android-long-before.log`).
- Added a session-owned native supervisor that sequences up to 180-second segments, stops at a
  monotonic 30-minute deadline or a reserved file-size threshold, and fences rollover before Stop.
  Child/supervisor signals require a command line containing the capture's unique directory.
  The PTY HUP trap ends capture on transport loss; source directories remain tracked for recovery.
- `RecordingMuxer` streams finalized H.264 samples through Android MediaExtractor/MediaMuxer
  into one MP4 with increasing timestamps. It checks dimensions/codec configuration, bounds sample
  buffers and final output size, observes cancellation and retains remote source segments after
  finalization failure. The stop text discloses brief gaps between native recorder restarts.
- The real long test passed with two segments, 187,824.544 ms, 2,278,619 bytes and 418 encoded
  packets, including 14 packets after 181 seconds. Full host ffmpeg decoding passed using the
  source time base; timestamps strictly increase. Evidence: `/tmp/bridge-android-long-after.log`,
  ignored `target/android-recording/long.mp4`. This long run preceded the final interruptible-wait
  cleanup fix; final-code rollover/finalization was rerun in the short lane below.
- The broader lifecycle test exposed an existing logs-start readiness race: the following marker
  could be sent before logcat was reading. Start now waits for stream data; the test uses a unique
  marker so historical log lines cannot create a false pass. The test then exposed delayed HUP
  handling while the supervisor slept. Its timer now waits interruptibly, allowing prompt cleanup.
- Final installed-code `LocalAdbTest#realLocalDaemon` passed shell, APK install/uninstall, binary
  transfer, cancellation, log marker capture, recording, session Stop, disconnect and owned-file
  cleanup (`/tmp/bridge-android-segments-lifecycle-final.log`). The new manual
  `e2e/android-recording.sh` short lane passed reduced-duration stop, multiple segments, full decode
  and timestamp checks (`/tmp/bridge-android-final-recording-lane.log`, ignored
  `target/android-recording/run-yPc6h1oR/`). No capture processes or remote directories remained.
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
  Evidence: `/tmp/bridge-android-service-long-final.log`, ignored
  `target/android-recording/service-ghqrk9z3/verification.json` and `recording.mp4`.
- All 64 JVM tests, app/test APK builds and the strengthened reduced-duration native timing test
  pass (`/tmp/bridge-android-service-final-build.log`, `/tmp/bridge-android-sparse-bounded.log`).
  After cleanup there were no capture processes or capture directories. Instrumentation/reinstall
  had left previously granted accessibility/notification listeners unbound; only the emulator's
  existing Bridge grants were rebound. The physical Pixel 8 was not modified.
- This closes local long-recording relay/upload/download verification on Android 16. Production
  IAM/OBO/Briefcase, full 30-minute/1-GiB tests, physical phones/TVs and release signing remain open.
