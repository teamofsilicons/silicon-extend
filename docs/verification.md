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
