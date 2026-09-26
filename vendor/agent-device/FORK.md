# Silicon Extend fork of agent-device

Forked from https://github.com/callstack/agent-device at commit `bce6f52` (2026-09-25, v0.21.15), MIT licensed.

Silicon Extend runs this fork on Mac and Linux computers (and on a Mac hosting an iPhone or iPad) to
read the screen and act on it. Every change made for Extend is listed below, newest first, so it can
be offered upstream or carried across an upstream sync. Each entry names its files. Entries marked
*uncommitted* were in the working tree on 2026-09-27 and not yet in a commit.

Outside this tree: Extend's packaging installs its own entry, `apps/desktop/runtime-entry.mjs`, as
the packaged runtime's `bin/agent-device.mjs` and ships this fork's entry beside it as
`bin/agent-device-cli.mjs`; it also appends `+extend.<sha256>` to the packaged `package.json`
version. Neither changes the fork's source; see `apps/desktop/README.md`.

## 2026-09-26 — Linux X11 app recording: redraw proof, owner binding, timing (*uncommitted*)

Review fixes to the X11 recorder added in the entries below.

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

## 2026-09-26 — macOS: text entry, recording start/stop and app screenshots (*uncommitted*)

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
