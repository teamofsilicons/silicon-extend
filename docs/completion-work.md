# Pending implementation and release work

Objective: complete the pending parts identified in the 2026-09-26 verification record.
The human-owned `understanding/UNDERSTANDING.md` remains the product authority.
An implementation or passing mock does not close a physical-device or production gate.

## Open gates

- Android: physical-device coverage, large APK/Briefcase inputs, long-recording service transfer
  and full 30-minute/1-GiB recording stress. Local ADB pairing, command execution, installation, logs and
  recording are implemented; see the verification record for exercised paths.
- Desktop: Mac recording stress/recovery, hidden-stage/multi-display coverage and active-session update policy; notarized helper distribution;
  Windows runtime and recording; Linux multiwindow/minimized-app coverage, Wayland recording and current desktop support.
- Hardware: physical Android/Fire TV, iPhone/iPad, Apple TV, Samsung/LG TVs. Record actual
  device/OS and exercised operations; simulator/mock results remain separate.
- Integrations: Briefcase and Ting through real IAM OBO, file sharing and retention;
  current official IAM client and webhook compatibility.
- Release: repository remote and CI, signed Android and Apple artifacts, six CLI targets,
  downloadable device apps, Honeycomb publication, configuration website/backend deployment,
  DNS and production smoke tests. Verify existing infrastructure before modifying it.
- Final audit: compare product and technical contracts against implementation and
  evidence, including revocation, session cancellation and test-environment isolation.

## Evidence collected in this continuation

- Initial checkout clean at `fcf9a90`; no Git remote configured.
- Existing tests and device exercises are recorded in `docs/verification.md`; these are
  historical results, not reruns by this continuation.
- No Android device/emulator was connected at the initial `adb devices -l` check.
- At the initial checkout, Android explicitly refused `adb`, `install`, `reinstall`, `logs`, `record`.
- No changes to the human-owned requirements are authorized or needed.

## In progress

- Android ADB implementation built; protocol unit tests, real-daemon instrumentation and
  real-service CLI tests have passed. Crash/reconnect and the 84-check phone regression pass.
  See `docs/verification.md` and `apps/android/README.md` for exact scope and limits.
- Remaining Android coverage includes physical phones/TVs, large APK/Briefcase inputs,
  long-recording service transfer and full 30-minute/1-GiB stress. Do not infer full contract completion from
  the small installation fixture or a short MP4 recording.
- Next implementation area: desktop missing recording/input capabilities and release packaging.
- Mac native `fill`/`type`/`focus` now pass live AppKit and packaged local-driver tests without
  XCTest. Unicode, exact whitespace, clearing, secure-field replacement and cancellation are
  covered. Both OS grants are enabled and recognized by the running GUI app. Native ScreenCaptureKit
  recording now passes short static/animated recording and service artifact-transfer checks without
  XCTest. Full 30-minute/1-GiB limits, hidden-stage app capture, notarization,
  other desktop platforms and production remain open.

## Release prerequisites inspected

- macOS keychain contains valid Apple Development, Apple Distribution and Developer ID Application
  identities. Signing may be possible locally; notarization credentials and provisioning still
  need checking. No signing/release gate is marked complete by this inventory.
- `gh`, `aws` and `vercel` CLIs are installed; authentication and production permissions have not
  yet been verified in this continuation.
- Built an optimized macOS arm64 bundle with the existing Developer ID identity, hardened
  runtime and secure timestamps. Signature verification and bundled Node execution pass.
  The signed app correctly reports that its own Accessibility and Screen Recording grants
  are still needed. This does not prove typing/recording or notarization.
- The Mac build pins and verifies Node 22.23.3, always rebuilds the agent-device fork, and
  supports notarization through an existing Keychain profile. No notarization credentials
  were established or submission made in this continuation.
- Physical iPhone and iPad entries are known to Xcode but currently unavailable. Only the
  Android emulator is attached through adb. Hardware verification remains open.

- Named Mac app binding now passes snapshots, nonblank screenshots and app-scoped recording
  with a second owned app in front. See the named-app verification entry for scope and
  Stage Manager limitations. URL-only opens still follow the foreground app.

- Packaged daemon refresh now has a content-based runtime version. A real empty-session daemon
  test reproduces unstamped stale reuse, verifies replacement by changed stamped code, and checks
  reuse by an identical relocated artifact. Active-session updates remain unverified.

- Mac reduced duration/file limits and abrupt native recorder-owner exit now pass real
  GUI-owned recording and full decoding. The full 30-minute run was stopped by the Carbon;
  full caps remain unverified. See the recording limits entry for exact results.

- The native X11 recording worker now passes real unprivileged Xvfb/GTK tests for short window
  and device capture, reduced limits, owner loss and supervisor death. Public whole-screen command wiring and daemon-crash recovery now pass. Single mapped-window
  app binding and capture beneath an overlapping app pass in Xvfb. Multiwindow/minimized apps,
  physical desktops, Wayland portal support, production file transfer and cross-distro/architecture
  release verification remain open.


- Linux arm64 optimized tarball and `.deb` now build and install on Debian trixie. Packaging
  rebuilds agent-device, pins/checks Node, and derives native dependency versions from both
  binaries. An installed unprivileged app with no source runtime mounted passed app/device
  recording through the local service, upload, full decode and repeat CLI download. This closes
  local Linux recording transfer; production IAM/Briefcase, x64 and other distros remain open.


- Android now supervises multiple native recording segments and combines them into one MP4.
  A real emulator test failed at 180.4 seconds before the change and produced a fully decoded
  187.8-second video after it. Reduced automatic duration/rollover, session Stop and ADB disconnect
  cleanup pass. Native restarts can leave brief capture gaps. The current device inventory also
  includes a connected Pixel 8; this continuation used only the emulator and does not establish
  physical-phone coverage.
