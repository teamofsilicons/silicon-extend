# Silicon Bridge fork of agent-device

Forked from https://github.com/callstack/agent-device at commit `bce6f52` (2026-09-25, v0.21.15), MIT licensed.

Bridge runs this fork on Mac and Linux devices (and on a Mac hosting an iPhone or iPad) to read the screen
and act on it. Changes made for Bridge are listed below, newest first, so they can be offered upstream.

- **2026-09-26 — Native macOS recording.** A signed ScreenCaptureKit helper writes H.264 MP4
  without XCTest. Startup waits for its first frame and records exact process ownership;
  the existing durable recording lifecycle handles stop, export and recovery. App/device scope,
  FPS, duration/file bounds, signals and owner exit are implemented. Native display selection
  ignores empty/off-screen auxiliary windows. Live animated app recordings pass manual stop,
  a short duration cap, artifact transfer, full decoding and visual inspection; longer limits
  and owner-loss stress remain separate gates.
- **2026-09-26 — Retryable session cleanup.** Failed platform close or resource cleanup keeps
  the session alongside its device claim, and defers provider lease release. Retrying close can
  finish cleanup and release ownership instead of leaving a claim for a deleted session.
  Lifecycle tests cover recording failure, platform failure, provider retention and successful retry.
- **2026-09-26 — macOS native text entry.** `fill`, `type` and `focus` route to the signed
  Accessibility helper instead of starting XCTest. It validates app/field focus, selects text
  through AX ranges, sends Unicode keyboard events, verifies non-secure replacement and stops
  on cancellation. Text is supplied over stdin. Unit dispatch tests and Bridge's live AppKit
  fixture cover the helper and packaged selector path.
- **2026-09-26 — Linux: xclip clipboard writes no longer time out.** `clipboard write` with xclip ran
  `xclip -selection clipboard` directly; xclip forks a child that keeps serving the selection and
  inherits the stdout/stderr pipes, so the 5 s wait always timed out. It now runs through
  `sh -c 'exec xclip -selection clipboard >/dev/null 2>&1'`. Verified on Debian trixie under Xvfb
  (`apps/desktop/linux-e2e`). File: `packages/platform-linux/src/tool-provider.ts`.
- **2026-09-26 — Linux: AT-SPI depth limit 12 → 40.** GTK4 apps nest deeper than 12 (GNOME
  Calculator's keypad buttons were cut off, so `snapshot` showed 6 nodes and no buttons). Nodes stay
  capped at 1500. Files: `packages/platform-linux/src/atspi-bridge.ts`, `linux/atspi-dump.py`.
- **2026-09-26 — `AGENT_DEVICE_JSON_TEXT=1` puts the human text in `--json` output.** With the variable
  set, a successful `--json` result is `{"success": true, "data": …, "text": "<what the command prints
  without --json>"}`. Bridge's desktop agent (`crates/bridge-agent`) runs every command once with
  `--json` and needs both: the structured result for the service and the text the `bridge` CLI prints
  (the snapshot tree with `@eN` refs, `Tapped @e6 (511, 374)`, …). Without the variable nothing
  changes. Files: `src/cli/commands/shared.ts` (`writeCommandOutput`), `src/commands/output/json.ts`
  (`printJson` type, `jsonTextRequested`). Upstreamable as an opt-in flag.
