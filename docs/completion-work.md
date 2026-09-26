# Pending implementation and release work

Objective: complete the pending parts identified in the 2026-09-26 verification record.
The human-owned `understanding/UNDERSTANDING.md` remains the product authority.
An implementation or passing mock does not close a physical-device or production gate.

## Open gates

- Android: physical-device coverage, large APK/Briefcase inputs and recording beyond the
  native 180-second limit. Local ADB pairing, command execution, installation, logs and
  recording are implemented; see the verification record for exercised paths.
- Desktop: Mac typing/recording without an unanswered setup prompt; distributable helper;
  Windows runtime and recording; Linux recording and current desktop support.
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
- Remaining Android coverage includes physical phones/TVs, large APK/Briefcase inputs, and
  recording beyond the native 180-second limit. Do not infer full contract completion from
  the small installation fixture or a short MP4 recording.
- Next implementation area: desktop missing recording/input capabilities and release packaging.

## Release prerequisites inspected

- macOS keychain contains valid Apple Development, Apple Distribution and Developer ID Application
  identities. Signing may be possible locally; notarization credentials and provisioning still
  need checking. No signing/release gate is marked complete by this inventory.
- `gh`, `aws` and `vercel` CLIs are installed; authentication and production permissions have not
  yet been verified in this continuation.
