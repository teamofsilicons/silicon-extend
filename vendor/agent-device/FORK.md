# Silicon Bridge fork of agent-device

Forked from https://github.com/callstack/agent-device at commit `bce6f52` (2026-09-25, v0.21.15), MIT licensed.

Bridge runs this fork on Mac and Linux devices (and on a Mac hosting an iPhone or iPad) to read the screen
and act on it. Changes made for Bridge are listed below, newest first, so they can be offered upstream.

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
