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
- **Mac typing and screen recording**: need UI Automation enabled once
  (`automationmodetool enable-automationmode-without-authentication`); reported as missing until then.
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
  profile is needed. The artifact is signed, not notarized or published. Mac typing and
  recording still require the UI Automation setup documented above.
