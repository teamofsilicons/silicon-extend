# Silicon Extend fork of agent-device

Forked from https://github.com/callstack/agent-device at commit `bce6f52` (2026-09-25, v0.21.15), MIT licensed.

Silicon Extend runs this fork, which it calls the device engine (`vendor/extend-engine`, package
`silicon-extend-engine`), on Mac and Linux computers (and on a Mac hosting an iPhone or iPad) to
read the screen and act on it. Every change made for Extend is listed below, newest first, so it can
be offered upstream or carried across an upstream sync. Each entry names its files. Entries marked
*uncommitted* were in the working tree on 2026-09-27 and not yet in a commit.

Silicon Extend's CI (`.github/workflows/ci.yml`, job `fork`) runs the engine's whole unit suite
(`pnpm test:unit`: the unit-core and fuzz-worker projects), so every test an entry adds or changes
runs there.

Outside this tree: Extend's packaging installs its own entry, `apps/desktop/runtime-entry.mjs`, as
the packaged runtime's `bin/extend-engine.mjs` and ships this fork's entry beside it as
`bin/extend-engine-cli.mjs`; it also appends `+extend.<sha256>` to the packaged `package.json`
version. Neither changes the fork's source; see `apps/desktop/README.md`.

## 2026-09-27 — 1.1.0 integration: the unit suite passes in full (*uncommitted*)

The rename left upstream tests reading files the rename removed, and six tests still pinned upstream
behaviour this fork deliberately changed in 1.0. The whole unit suite (`pnpm test:unit`) now passes
on macOS and Linux, and CI runs all of it instead of a list of files.

- **Removed: tests of upstream tooling and material Silicon Extend doesn't ship.** Nothing here
  tested engine behaviour:
  - reading the removed `website/` docs: `src/__tests__/client-api-examples-drift.test.ts`,
    `src/__tests__/command-doc-coverage.test.ts` (and its `check:command-docs` script),
    `scripts/__tests__/agent-setup-startup-contract.test.ts`, and the doc-only cases in
    `cli-agent-cdp.test.ts` (2), `cli-react-devtools.test.ts`, `maestro-support-matrix.test.ts` and
    `is-argument-surface-parity.test.ts` (its predicate-list and schema assertions stay);
  - reading the removed `skills/`: `scripts/__tests__/simulator-skills-contract.test.ts` and one
    case of `help-conformance-bench.test.ts`;
  - reading the removed upstream `.github/` workflows and actions: `test/ci/root-docs-paths-ignore`,
    `size-workflow`, `upload-agent-device-artifacts` and `upload-artifact-hidden-paths`,
    `scripts/__tests__/apple-ci-impact.test.ts` with `scripts/apple-ci-impact.ts`,
    `scripts/__tests__/xctest-selection.test.ts` (its `check:xctest-selection` script too; the
    module stays for `xctest-run-summary.ts`), one case of `size-report-package.test.ts`, and
    `src/__tests__/npm-package-scripts.test.ts` (it pinned upstream's release and npm scripts);
  - the removed `server.json`, Fallow and Stryker: `scripts/__tests__/mcp-metadata.test.ts`,
    `scripts/__tests__/fallow-fixture-policy.test.ts`, `scripts/sync-mcp-metadata.mjs`,
    `scripts/release-mark-dev.mjs` and `scripts/mutation/`;
  - upstream's git-history ratchets, `scripts/__tests__/eager-closure-budgets.test.ts` and
    `scripts/__tests__/test-file-size-ratchet.test.ts`. They compare against the merge-base with
    `origin/main` from the engine's own root; in Silicon Extend's repository the engine is a
    subdirectory that was `vendor/agent-device` at that merge-base, so every entry read as new.
    `eager-closure-budgets.ts` and `committed-source-tree.ts` stay: the layering scripts use them.
- **Updated to this fork's behaviour** (each failed already at 1.0.2):
  - `src/platform-runtime-operation-host.test.ts`: the macOS `app` surface (and an absent one) is
    forwarded to the helper as `app`, not refused ("Bound macOS app capture" below);
  - `packages/platform-apple/src/__tests__/screenshot-backend.test.ts`: a screenshot carries the
    session's bound app id (same entry);
  - `src/daemon/session-lifecycle/internal/__tests__/session-teardown-resources.test.ts`: a failed
    recording finalization keeps the session for a retried `close` ("Retryable session cleanup");
  - `src/daemon/__tests__/request-router-record-runtime-lock.test.ts`: Linux records here, so a
    Linux session's record start is refused by the host (pinned to one that isn't Linux) with
    `owner-capability-missing`; Vega keeps upstream's refusal;
  - `src/daemon/__tests__/application-lifecycle-runtime-fixture.ts` gains the Linux recording host
    the fork's Linux facts read, which `request-router-typed-error.test.ts` reaches.
- **Rename fallout in tests:** `cli-network.test.ts` and `cli-doctor-progress.test.ts` use the
  doctor's `engine` check; `cli-session-state-dir.test.ts` expects `~/.silicon-extend/engine/dev`.
- **Text a Silicon can see:** the snapshot presentation warning says "the device engine" (was "Agent
  Device … runner bug"); three schema and MCP field descriptions ("Agent-device selector expression",
  "… session name", "… state directory") and the proxy connection's service label say the device
  engine; help examples open "Example App" instead of upstream's fixture app.
- `src/__tests__/hermetic-env-setup.ts` removes `FORCE_COLOR` (set by some terminals), which made
  plain-output assertions see colour codes outside CI. `.gitignore` also ignores `/.silicon-extend/`,
  where the engine now writes inside a project.
- `package.json`: the `build:xcuitest:*` and Android helper scripts set `EXTEND_ENGINE_*` names.
- Files: the removals above, `package.json`, `vitest.config.ts`, `.gitignore`,
  `src/__tests__/hermetic-env-setup.ts`,
  `packages/capture-kit/src/snapshot/snapshot-presentation/quality-warnings.ts`,
  `src/mcp/tool-control-fields.ts`, `src/commands/{command-input,common-input-fields}.ts`,
  `src/cli/connection/connect-provider-adapters.ts`, `src/commands/schema/cli-help.ts`,
  `src/commands/management/app.ts`, and the tests named above.

## 2026-09-27 — The engine's own text says Silicon Extend (*uncommitted*)

- Help, usage, errors, hints and log lines say `extend <command>` and "the device engine"; advice
  names `EXTEND_ENGINE_<X>`, and settings are read under `EXTEND_ENGINE_<X>` first, then
  `AGENT_DEVICE_<X>` (`packages/kernel/src/source-value.ts`), so an error about a bad value names
  the variable that was set.
- The home folder is `~/.silicon-extend/engine` (`packages/kernel/src/extend-names.ts`): the default
  state dir, `config.json`, device claims, logs, the Apple runner's builds and leases, the macOS,
  fold and snapshot-bridge helpers, and the web tool. Project files go to `.silicon-extend/engine`;
  the project config is `./extend-engine.json`, and the fork's upstream config files are not read
  (a separate upstream install on the same Mac can't leak its settings in).
- Temporary files use the prefix `extend-engine-`; recordings on a device are
  `silicon-extend-recording-*` (the recovery patterns match them), since a Carbon can find them in
  Files.
- The iOS helper is `SiliconExtendHelper` (`com.teamofsilicons.extend.helper`); the macOS helper
  product is `silicon-extend-macos-helper` (a tree still building the old product is used as a
  fallback); the Android helpers are `com.teamofsilicons.extend.{imehelper,snapshothelper}`, and
  installing one removes the old `com.callstack.agentdevice.*` package (best effort, logged).
  Keyboard restore also recognises the old test keyboard; the app list hides the old iOS helper too.
- Saved scripts write `# extend:target-v1` and still read `# agent-device:target-v1`.
- The upstream npm update notice is no longer shown (it pointed at the upstream package).
- The MCP server, doctor check (`engine`), JUnit suite, log markers (`[extend-engine][mark]`,
  `[extend-engine][diag]`), the daemon's port lines, the crash-report key, the download User-Agent
  and the usbmux program name say Silicon Extend or the device engine.
- Project-root detection looks for the package name `silicon-extend-engine` (this fixes daemon
  launch from a source checkout).
- Kept: daemon RPC method names, `x-agent-device-*` headers and the `/agent-device` base path (a
  client and daemon of different versions must still talk); Swift compile flags and runner log
  markers shared with the helpers; upstream cloud provider identifiers; the `@agent-device/*`
  workspace packages and TypeScript symbols.
- Files: `src/**`, `packages/**`, `scripts/**`, `test/**`, `linux/**` and
  `contracts/fixtures/runner-requests.json`, as listed in the rename-engine-text work.

## 2026-09-27 — Helpers renamed to Silicon Extend (*uncommitted*)

- **iPhone/iPad/Mac helper (`apple/runner`):** `AgentDeviceRunner` became `SiliconExtendHelper`
  (folder, `SiliconExtendHelper.xcodeproj`, scheme and app target), the UI-test target
  `SiliconExtendHelperUITests` and test plan `SiliconExtendHelperUITests.xctestplan`; pbxproj
  object ids are unchanged. The UI-test `PRODUCT_NAME` is `SiliconExtend`, because Xcode always
  names the runner `<PRODUCT_NAME>-Runner` (`SiliconExtend-Runner`) and gives it no display name.
  The app shows "Silicon Extend" (`CFBundleDisplayName`, label, window and banner). Default bundle
  ids are `com.teamofsilicons.extend.helper` and `.uitests`, set by
  `EXTEND_ENGINE_IOS_RUNNER_APP_BUNDLE_ID` / `EXTEND_ENGINE_IOS_RUNNER_TEST_BUNDLE_ID`. The scheme's
  testable for the nonexistent `AgentDeviceRunnerTests` target was dropped. Error domains, queue
  labels, synthesized-event names and the fallback recording name say Silicon Extend; the
  `AGENT_DEVICE_RUNNER_*` log markers, the unit-test and canary conditions, fixture launch arguments
  and accessibility ids, and the `AgentDeviceSnapshotPresentation` module keep upstream spelling.
- **macOS helper:** package and executable product `silicon-extend-macos-helper` (targets
  unchanged), and the audio-probe queue label.
- **snapshot-presentation:** package name `silicon-extend-snapshot-presentation` (module unchanged).
  The Simulator AX bridge logs as `[silicon-extend-snapshot-bridge]`; its error domain and
  `kSourceVersion` are unchanged.
- **Android:** `com.teamofsilicons.extend.imehelper` ("Silicon Extend Keyboard", log tag
  `SiliconExtendKeyboard`, actions under the new package) and
  `com.teamofsilicons.extend.snapshothelper` ("Silicon Extend Snapshot Helper"); the Java sources
  moved to match; the `agentDeviceProtocol` status key is unchanged.
- Renaming the helpers makes macOS ask again for Accessibility (and Screen Recording) for the
  XCUITest runner and the macOS helper, and puts new apps on an iPhone or iPad.
- Files: `apple/runner/**`, `apple/macos-helper/Package.swift`,
  `apple/macos-helper/Sources/AgentDeviceMacOSHelper/AudioProbe.swift`,
  `apple/snapshot-presentation/Package.swift`, `Package.runner.swift`,
  `apple/snapshot-bridge/SnapshotBridge.m`, `android/**`.

## 2026-09-27 — Renamed to Silicon Extend's device engine (*uncommitted*)

Silicon Extend 1.1 names this fork the device engine everywhere a Carbon or Silicon can see it.
Internal names nobody sees (the `@agent-device/*` workspace packages, TypeScript symbols, module
paths, the `AGENT_DEVICE_*` names the code reads) keep the fork's spelling, so an upstream sync
stays a plain merge.

- **Directory and package.** `vendor/agent-device` moved to `vendor/extend-engine` (`git mv`, so
  history follows). `package.json`: name `silicon-extend-engine`, `private: true`, a description
  that says what it is for Extend, no upstream homepage, repository, bugs, `mcpName` or keywords;
  the bin is `extend-engine` → `bin/extend-engine.mjs` (was `bin/agent-device.mjs`), and the `ad`
  script is `engine`. `pnpm-lock.yaml` was regenerated for the changes below.
- **Settings are `EXTEND_ENGINE_<X>`.** New `src/extend-env.ts` copies every set
  `EXTEND_ENGINE_<X>` onto `AGENT_DEVICE_<X>` (the new name wins; an old name set alone still
  works). It is imported first by `src/bin.ts` and `src/daemon.ts`, by `bin/extend-engine.mjs`
  (before the dist entry), and by Extend's `apps/desktop/runtime-entry.mjs`; `tsdown.config.ts`
  builds it as its own entry, `dist/src/internal/extend-env.js`, so that plain-JavaScript entry can
  import it. Nothing in the engine reads a setting while its modules load, so the bundler's module
  order within a chunk doesn't matter. Test: `src/extend-env.test.ts` (added to CI's `fork` list).
- **Upstream material nothing in Extend builds, tests or ships was removed:** `website/` (the
  docs site, also dropped from `pnpm-workspace.yaml`), `examples/` (dropped from `typecheck`),
  `skills/`, `.github/` (upstream CI; Extend's CI is at the repository root), `glama.json`,
  `smithery.yaml`, `server.json` (dropped from `files`), `CONTRIBUTING.md`, `SECURITY.md`,
  `.worktreeinclude`, the mutation-testing setup (`stryker.config.json`,
  `vitest.mutation.config.ts`, the `@stryker-mutator/*` dev dependencies and the `mutation:*`
  scripts), the dead-code tooling (`.fallowrc.json`, `fallow-baselines/`,
  `fallow-production-exports.json`, the `fallow` dev dependency and its scripts and build approval),
  the upstream release and MCP-registry scripts (`release:*`, `sync:mcp-metadata`,
  `check:mcp-metadata`, `version`, `prepack`), the `test-app:*` scripts, and three unreferenced
  notes in `docs/`. The upstream tooling left in `scripts/` for them, and the tests that read the
  removed files, went in the 1.1.0 integration entry above.
- `README.md` is rewritten for Extend; `AGENTS.md` and `CONTEXT.md` name the device engine. The
  ADRs and evidence notes in `docs/` are unchanged upstream history: they quote code strings and
  upstream issue links as they are.
- Not in this entry: the iPhone/iPad helper (`apple/runner`), the macOS helper's Swift product
  and the Android helpers (`android/`) are renamed in their own entries.
- Files: `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml`, `tsdown.config.ts`,
  `bin/extend-engine.mjs`, `src/extend-env.ts` (+ test), `src/bin.ts`, `src/daemon.ts`,
  `README.md`, `AGENTS.md`, `CONTEXT.md`, this file, and the removals above.

## 2026-09-27 — Linux: the whole app window is drawn again at record start; `--quality` picks the bit rate (*uncommitted*)

Round-2 fixes. They supersede two points of the next entry: the recorder now does call
`XClearArea`, and Linux no longer refuses `--quality`.

- **Every pixel is drawn again before the first frame.** An audit's probes found two leaks left by
  the Expose-based redraw proof below: a plain Xlib window whose background is `None` (the
  `XCreateWindow` default, which GLFW, SDL and many toolkits keep) and that doesn't advertise
  `_NET_WM_PING` could be recorded showing a window that had covered it, if its app stopped
  handling events while covered and the cover then moved away; and a second recorder started on a
  window another recorder had already redirected saw no Expose and could record the cover. The
  seeded off-screen copy is now never trusted. After the grabbed redirect, the recorder clears the
  window and every viewable window inside it with exposures (`XClearArea(…, 0, 0, 0, 0, True)`), so
  the X server paints each background there is and the app gets real Expose events for all of it,
  and it reads no frame until damage covers every pixel inside the window's shape. Borders (a
  window's bounding region less its clip region) are only ever painted by the server, so they are
  exempt (`XFixesCreateRegionFromWindow` for both, `XFixesTranslateRegion`). The clear can show as
  one flicker of the window's background at record start. Exposure selection on inner windows is
  gone; only `StructureNotify` on the target stays.
- **Refusals** now name the way out: "the app did not redraw its whole window within 5 seconds of
  recording start (it may not be responding), so its window could still hold pixels another window
  left there and a recording of just this app could show that window; record the whole screen
  instead (--scope device), or make sure the app is responding and record it again", and the same
  "record the whole screen instead (--scope device)" for an app that doesn't answer its ping. A
  stopped app whose windows all have a background (xmessage, xev, plain Xlib with a background
  pixel) is still recorded, since the server painted every pixel.
- **`--quality` on Linux.** Linux exports the recorder's own H.264 encode unchanged, so `--quality`
  now picks that encode's bit rate instead of being refused: `medium` (the default) 8 Mbit/s,
  `high` 20 Mbit/s, as Android's screenrecord. `LinuxScreenRecordingHost.start` takes `quality`,
  the Linux recording runtime passes `exportQuality` through (and keeps it across a durable
  reattach), the host adds `--quality <q>` to the worker only when one was asked for, and
  `screen-record.py --quality medium|high` sets `-b:v`, `-maxrate` and `-bufsize`. The refusal
  message for options Linux still can't honour no longer mentions quality. The `record` command's
  help text says so.
- **Build record.** `.gitignore` ignores `/.extend-build-manifest.json`, which Extend's packaging
  (`apps/desktop/dist-manifest.mjs`) writes to record which sources `dist/` was built from.
- Files: `linux/x11_composite.py`, `linux/screen-record.py`,
  `packages/contracts/src/screen-recording-runtime-host.ts`,
  `packages/platform-linux/src/recording/runtime.ts` (+ test: `--quality` is passed through and kept
  across reattach, replacing the old refusal test),
  `src/platform-runtime-screen-recording-linux-host.ts` (+ test: the worker gets `--quality` only
  when asked), `src/commands/recording/index.ts` (help text), `.gitignore`.
- Verified in Xvfb containers only, by a separate verifier: Extend's `record-hung-e2e.py` passed
  every case with no window manager and with openbox (the `plain …` cases: covered, left by its
  cover, stopped while covered or uncovered, with and without a background, and a shaped GTK window
  above the cover); the verifier's own probe refused all four background-`None` cases, the
  two-recorder one included, with no frame written; `record-e2e.py`, the isolation lane,
  `record-app-e2e.py` and `record-runtime-e2e.py` passed, the last including "`--quality high`
  records on Linux, encoded at 20 Mbit/s". The 23 fork vitest files Extend has touched pass
  (278 tests, macOS host). Not checked: a compositing desktop (GNOME, KDE, picom), Tk or Java apps
  on a real display, and how visible the one-time flicker is to a Carbon watching the app.

## 2026-09-26 — Linux X11 app recording: redraw proof, owner binding, timing (`911b3c7`)

Review fixes to the X11 recorder added in the entries below. Superseded in part by the 2026-09-27
entry above (the redraw proof and `--quality`).

- **Covered windows.** At `record start --scope app` the worker holds the X server
  (`XGrabServer`) while it redirects the window, and selects Expose on the window and every
  viewable InputOutput window inside it, so the Expose events the redirect generates say exactly
  which parts were hidden (covered or off screen). XDamage is created before the redirect; no frame
  is read until damage covers every hidden part. A window nothing covers starts at once. No
  `XClearArea`, so the recorder never erases the app's pixels. Measured grab: about 2 ms for a
  typical app, 45 ms at 500 inner windows, 285 ms at 4,000.
- **Refusals** (no video is written): a hidden part not redrawn within 5 s ("part of the app window
  was hidden when recording started … record the whole screen with --scope device"); an app that
  advertises `_NET_WM_PING` (GTK 3/4, Qt, Chromium/Electron, SDL, Firefox) and does not answer a
  ping within 5 s ("the app is not responding …"), even when uncovered, because a hung app keeps
  showing whatever last covered it; more than 2,048 inner windows; an input-only target; missing
  libXcomposite/libXdamage/libXfixes or the XComposite/DAMAGE/XFIXES extensions (the message names
  the Debian package).
- The part of a shaped window outside its bounding shape at start is recorded black.
- Unmapping, remapping, resizing or reparenting the window ends the recording as `source-ended`,
  even when faster than one frame (StructureNotify). Frames exclude the X border.
- The worker waits up to 5 s for the named app's window to appear before refusing.
- **Timing.** App frames are stamped with the wall clock at the encoder and written at a constant
  frame rate (`-r <fps>`), so a slow capture repeats frames instead of playing back fast. A duration
  watchdog that fires on a finalized, playable MP4 completes with `duration-limit`. A real failure
  returns "Linux recording did not finalize successfully: <worker reason>" with `retriable:false`.
- **Owner.** `screen-record.py --owner-pid` (the host passes the daemon's pid) is checked before any
  work, and `PR_SET_PDEATHSIG=SIGTERM` ends the worker if the daemon dies during startup; during
  recording the owner's death still finalizes with `owner-exited`.
- **Status file** is always `<native path>.status.json`; stop and cleanup remove it, also after a
  daemon restart, and a stale one is removed before a new recording.
- **`--quality`** is refused on Linux with `INVALID_ARGS` ("Linux recordings do not support
  --quality …"); `--fps` and `--hide-touches` still work. The warning after a daemon restart no
  longer says "resumed": the video ends when that daemon exited.
- Files: `linux/x11_composite.py`, `linux/screen-record.py`,
  `src/platform-runtime-screen-recording-linux-host.ts` (+ test),
  `packages/platform-linux/src/recording/runtime.ts` (+ test), `src/commands/recording/index.ts`
  (Linux help text).
- Verified only in Xvfb containers (no window manager and openbox): Extend's
  `apps/desktop/linux-e2e/record-hung-e2e.py` passed 18/18 in both; the same lane fails 6 cases
  against the previous worker. **Known gaps:** a plain Xlib client whose windows have background
  `None` and that does not advertise `_NET_WM_PING` can still be recorded showing a window that
  covered it earlier, if it hung while covered and the cover then moved away; a second recorder
  started on a window another recorder has already redirected sees no Expose and can record the
  cover. Not checked on a compositing desktop (GNOME, KDE, picom), or with Tk or Java apps on a
  real display.

## 2026-09-26 — macOS: text entry, recording start/stop and app screenshots (`911b3c7`)

- **Text entry follows the session surface.** On a `frontmost-app` session, `fill`, `type`,
  `focus`, `find … type` and `find … focus` act on the app that is frontmost when the command runs
  (before: the app that was frontmost when the session opened). App sessions stay bound to their
  bundle; desktop/menubar keep the bundle-or-frontmost rule. The helper's `text` stdin JSON gains
  `surface`; for `frontmost-app` the host omits `bundleId`; the helper refuses an `app` surface with
  no bundle, and desktop/menubar surfaces. Files: `packages/contracts/src/interactor-types.ts`
  (`RunnerContext.surface`), `src/daemon/snapshot-runtime-capture-input.ts`,
  `src/daemon/interaction/internal/find.ts` (`findLegContext`),
  `packages/platform-apple/src/interactions.ts`, `packages/platform-apple/src/os/macos/helper.ts`,
  `apple/macos-helper/…/TextEntry.swift`.
- **Text-entry limits.** Focus is checked before every posted key event (chunks of up to 20 UTF-16
  units, each character with `--delay-ms`, each Return/Tab). Timeout = max(30 s, 5 s + 2 ms per unit,
  or delay + 15 ms per unit with `--delay-ms`, + 15 ms per Return/Tab). Refused with `INVALID_ARGS`
  before any key: `--delay-ms` above 1,000, text over 1 MiB, or an estimate over 10 minutes (details
  carry `maxCharacters`).
- **Recording start.** A first frame seen at the 15 s deadline counts as a start. A refusal reaches
  the caller as "Native macOS recording could not start: <helper message>" with a reason and hint:
  `app_window_not_on_screen`, `app_not_available`, `screen_recording_permission_denied`
  (ScreenCaptureKit -3801), `no_screen_frames`, or `recording_start_failed`; a timeout is
  `recording_first_frame_timeout`. Exit code, signal, stdout and stderr stay in `details`.
- **Recording stop.** A stream macOS ends (the menu-bar indicator, a display disconnect, revoked
  capture) is a stop reason (`interrupted`), not a failure: the MP4 is finished and `record stop`
  returns it with a warning. A non-zero recorder exit no longer fails every `record stop`: the file
  is collected and the playability check decides; an unplayable or missing file fails with
  `retriable:false` and "Close this session …". New warnings: `duration-limit`, `size-limit`,
  `owner-exited`, `app-exited`, time the app had no window on screen (`appNotVisibleMs`).
- **App-scoped recording** picks its display from the app's on-screen, layer-0 windows matched to
  Accessibility windows (falling back to on-screen layer-0 windows), is refused when the app has no
  window on screen, and stops with `app-exited` when the app quits.
- **App screenshots** capture the display area under the bound window, filtered to the app, so the
  app's menus, popovers and sheets over the window are included and other apps are not; a window
  reaching past its display falls back to single-window capture. Menu parts outside the window's
  frame and the app's other windows are cropped out; `--fullscreen` is unchanged. macOS 13 uses a
  one-frame ScreenCaptureKit stream (the "requires macOS 14" refusal is gone); desktop, menubar and
  frontmost-app screenshots below macOS 15.2 capture the main display instead of refusing.
- Files: `apple/macos-helper/Sources/AgentDeviceMacOSHelper/{AppScreenshot,ScreenRecording,TextEntry,main}.swift`,
  new helper tests `BoundWindowSelectionTests.swift`, `TextEntryTargetTests.swift`,
  `packages/platform-apple/src/recording/runtime.ts`, `src/platform-runtime-screen-recording-macos-host.ts`,
  and their tests.
- Test hygiene: `packages/platform-apple/src/core/__tests__/interactions.test.ts` now mocks the
  macOS helper (its alternate-button test used to spawn the real helper and could click the real
  desktop); `packages/platform-apple/src/runtime.test.ts` reads `macOsSurfaceBackend()`.
- Not run: the live GUI checks (`apps/desktop/macos/text-e2e.py`), any macOS 13–15.1 run, two
  displays. Five vitest failures predate this work (runner-requests `mouseClick` coverage — see the
  bound-capture entry; an iOS runner screenshot; request-router record lock on Linux;
  request-router typed error; an iOS session-teardown case). Not changed: `MACOS_BUNDLE_ID_PATTERN`
  still reads `TextEdit.app` as a bundle id when agent-device is called directly (Extend passes the
  resolved `.app` path instead).

## 2026-09-26 — Rename to Silicon Extend (`7af7fd5`)

Only lines this project had added were renamed (Silicon Bridge → Silicon Extend,
`crates/bridge-agent` → `crates/extend-agent`, `com.teamofsilicons.bridge` →
`com.teamofsilicons.extend`). Upstream names that contain the word (`snapshot-bridge`,
`atspi-bridge`) are unchanged. No behaviour change.

## 2026-09-26 — Isolated X11 app recording through XComposite; Linux app identity (`c55cc45`)

- `record start --scope app` on X11 captures the named app's window through XComposite
  (`linux/x11_composite.py`, new), so an overlapping window is not recorded. Pixmap XImages report
  zero RGB masks, so pixels are read through the window's TrueColor visual.
- **Linux app identity:** `open <app>` records a named app identity on Linux (`resolveOpenTarget`
  sets the app for non-URL targets); app recording resolves exactly one mapped `WM_CLASS` matching
  the executable or desktop-file basename; missing or ambiguous targets are refused; the identity
  survives durable-resource recovery. Files: `packages/platform-linux/src/lifecycle.ts` (+ test),
  `packages/platform-linux/src/recording/runtime.ts`, `linux/screen-record.py`,
  `packages/contracts/src/screen-recording-runtime-host.ts`, `src/commands/recording/index.ts`,
  `src/platform-runtime-screen-recording-linux-host.ts`.

## 2026-09-26 — Public X11 recording with durable recovery (`32c3269`)

- Connects the worker to Linux recording admission, the host process authority and durable runtime
  resources. `record start --scope device|system` on X11; Wayland, XWayland and headless hosts do
  not advertise it. Recovery works after a daemon SIGKILL; the supervisor is stopped before any
  surviving identity-matched encoder (a double signal left MP4s without a moov atom).
- New: `packages/platform-linux/src/recording/runtime.ts` (+ test),
  `src/platform-runtime-screen-recording-linux-host.ts` (+ test). Changed:
  `packages/platform-linux/src/runtime.ts`, `packages/contracts/src/screen-recording-runtime-host.ts`,
  `packages/contracts/src/flag-definitions-action.ts`, `src/platform-runtime-screen-recording-host.ts`,
  `src/platform-runtime-screen-recording-process-host.ts` (a supervisor owner can opt out of tree
  signalling), `src/commands/recording/index.ts` (Linux `record` description),
  `packages/platform-linux/src/__tests__/clipboard.test.ts` (stale xclip expectation).

## 2026-09-26 — X11 recording worker (`6765b0b`)

`linux/screen-record.py` (new): captures a root screen, an X11 window id or an exact application
class with ffmpeg (`x11grab`, libx264) into H.264 MP4, publishes atomic readiness and finalization
status, bounds duration and file size, and finalizes on SIGINT/TERM/HUP or owner exit; Linux
parent-death signalling stops the encoder if its supervisor is killed.

## 2026-09-26 — Bound macOS app capture (`d9fdc31`)

- **The macOS `app` surface moved from the XCTest runner to the native helper** for snapshot,
  screenshot, read and pointer input (`packages/contracts/src/session-surface.ts`:
  `app: 'macos-helper'`). This is the largest difference from upstream on macOS: no macOS surface
  needs the XCTest runner any more. The runner's `mouseClick` request fixture was removed from
  `packages/contracts/fixtures/runner-requests.json`, so upstream's runner-request coverage test
  now fails on `mouseClick`; deleting the dead runner command or restoring a request is undecided.
- App surfaces keep their explicit bundle identity across focus changes; app snapshots carry it to
  Accessibility and fail rather than substitute the foreground app. App screenshots select the
  app's Accessibility windows from ScreenCaptureKit, excluding larger invisible backing windows.
  `.app` paths resolve bundle metadata; mixed-case bundle ids stay intact; native pointer delivery
  keeps primary, secondary and middle buttons; a named open replaces a prior foreground surface.
- Crop-target classification changed (`src/daemon/screenshot-crop-target.ts`): an `app` (or unset)
  surface crops as `macos-app-window`, every other surface as `macos-helper`.
- Files: `apple/macos-helper/…/{AppScreenshot,AppTarget,SnapshotTraversal,TextEntry,main}.swift`,
  `AgentDeviceMacOSInput/MouseClickDelivery.swift`, `packages/platform-apple/src/{interactions,interactor}.ts`,
  `packages/platform-apple/src/os/macos/{apps,helper,surface-snapshot}.ts`,
  `src/platform-runtime-open-target.ts`, and tests.
- Hidden Stage Manager windows and multiple displays were not verified.

## 2026-09-26 — Native macOS recording (`dce2102`)

A signed ScreenCaptureKit helper (`apple/macos-helper/…/ScreenRecording.swift`) writes H.264 MP4
without XCTest. Startup waits for its first frame and records exact process ownership; the existing
durable recording lifecycle handles stop, export and recovery. App/device scope, 1–60 fps, 30-minute
and 1-GiB bounds, signals and owner exit are implemented. Files also:
`packages/platform-apple/src/{macos-facade,os/macos/helper}.ts`,
`packages/platform-apple/src/recording/{recovery,runtime}.ts`,
`src/platform-runtime-screen-recording-{apple-host,host,macos-host}.ts`,
`packages/contracts/src/{flag-definitions-action,screen-recording-runtime-host}.ts`. (The display
choice described here at the time was replaced by the 2026-09-26 macOS entry above.)

Refreshing a running daemon after an update is handled outside the fork, by packaging (see the top
of this file); it is not a fork change.

## 2026-09-26 — Retryable session cleanup (`e2fa104`)

Failed platform close or resource cleanup keeps the session alongside its device claim and defers
provider lease release, so retrying `close` can finish cleanup and release ownership instead of
leaving a claim for a deleted session. Lifecycle tests cover recording failure, platform failure,
provider retention and a successful retry.

## 2026-09-26 — macOS native text entry (`414b96a`)

`fill`, `type` and `focus` route to the signed Accessibility helper instead of starting XCTest. It
validates app and field focus, selects text through AX ranges, sends Unicode keyboard events,
verifies non-secure replacement and stops on cancellation. Text is supplied over stdin.

## 2026-09-26 — Linux: xclip clipboard writes no longer time out

`clipboard write` with xclip ran `xclip -selection clipboard` directly; xclip forks a child that
keeps serving the selection and inherits the stdout/stderr pipes, so the 5 s wait always timed out.
It now runs through `sh -c 'exec xclip -selection clipboard >/dev/null 2>&1'`. Verified on Debian
trixie under Xvfb (`apps/desktop/linux-e2e`). File: `packages/platform-linux/src/tool-provider.ts`.

## 2026-09-26 — Linux: AT-SPI depth limit 12 → 40

GTK4 apps nest deeper than 12 (GNOME Calculator's keypad buttons were cut off, so `snapshot` showed
6 nodes and no buttons). Nodes stay capped at 1500. Files: `packages/platform-linux/src/atspi-bridge.ts`,
`linux/atspi-dump.py`.

## 2026-09-26 — `AGENT_DEVICE_JSON_TEXT=1` puts the printed text in `--json` output

With the variable set, a successful `--json` result is `{"success": true, "data": …, "text": "<what
the command prints without --json>"}`. Extend's desktop agent (`crates/extend-agent`) runs every
command once with `--json` and needs both: the structured result for the service and the text the
`extend` CLI prints (the snapshot tree with `@eN` refs, `Tapped @e6 (511, 374)`, …). Without the
variable nothing changes. Files: `src/cli/commands/shared.ts` (`writeCommandOutput`),
`src/commands/output/json.ts` (`printJson` type, `jsonTextRequested`). Upstreamable as an opt-in flag.
