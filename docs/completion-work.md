# Open gates

The current list of what stands between this checkout and a released Silicon Extend, as of
2026-09-27. The Carbon-owned [`understanding/UNDERSTANDING.md`](../understanding/UNDERSTANDING.md)
is the product authority; nothing here changes it. An implementation, a passing mock or an emulator
run does not close a physical-device or production gate. What was run, and on what, is in
[`verification.md`](verification.md).

## Needs the Carbon

### 1. The name and the GitHub repository

Checked read-only with `gh` on 2026-09-27; nothing was pushed, created or changed.

- **`teamofsilicons/silicon-extend` already exists and is a different product.** Private, created
  2026-07-24, described as "Our own tool integration layer": a Python tool layer for Silicons
  (`pip install silicon-extend`, a `silicon-extend` command, connected to Glass or standalone),
  Apache-2.0, releases v0.1.0 to v0.1.4, last pushed 2026-08-03. This product was renamed from
  Silicon Bridge to Silicon Extend on 2026-09-26, so the two now share a name, and
  `Cargo.toml`'s `repository` field points at the other product's repository.
- **`teamofsilicons/silicon-bridge` is a different, older product line.** Private, created
  2026-08-21; `main` has one commit, `cb3257e` "Initial commit" (2026-08-24), holding its own
  `UNDERSTANDING.md` (6,983 bytes: "silicon-bridge is to bridge stemcell and interface", a Rust
  relay over NATS core and JetStream between Silicons and Interface) and `interface-api-ref.html`
  (6.5 MB). No workflows, no tags; branch protection is not available on the current GitHub plan.
- This checkout has **no remote** and a different root commit. Putting it into either repository is
  a Carbon decision, not a git step: which name this product keeps, which repository it lives in,
  and what happens to the other product's `UNDERSTANDING.md` (two `UNDERSTANDING.md` files for two
  products must not end up side by side, and agents may edit neither). Until then: no push, never a
  force-push to any `main`, and treat `Cargo.toml`'s `repository` value as unsettled.
- Names that may collide the same way and were not checked: the crates.io names
  (`silicon-extend-client`, `extend-protocol`), the Honeycomb app `extend`, and the `extend`
  command on a Silicon's `PATH`.

### 2. Signing and notarization

- Which **Developer ID Application** identity signs Mac releases. It should be the Team's; builds so
  far were signed locally with a Developer ID found in this Mac's keychain. macOS ties Accessibility
  and Screen Recording grants to the signing identity, so changing it after Carbons have granted
  them resets those grants.
- A `notarytool` Keychain profile for `NOTARY_PROFILE`. Notarization is implemented in
  `apps/desktop/macos/build-app.sh` and exercised only with stubbed Apple tools; it has never run
  against Apple.
- The Android **release signing key** (APKs are development-signed today).
- Which Apple development team signs the iPhone/iPad runner (TECHNICAL.md open question 11).

### 3. Publishing

Nothing is deployed or published. Each needs the Carbon's go-ahead and credentials, and existing
infrastructure must be checked before anything is changed:

- The Extend service at `backend.extend.teamofsilicons.com` (one instance; `docs/deployment.md`).
- The configuration website at `extend.teamofsilicons.com` (Vercel).
- DNS for both.
- The CLI: tag, `release.yml`, `honeycomb releases upload` from a Carbon session.
- Downloads of the Mac, Linux, Windows and Android apps.
- Silicon IAM registration of Extend's OBO catalog, including Briefcase's approval of
  `invitations.create` (critical).
- Production smoke tests after each.

### 4. Contract and product decisions

- **`UNDERSTANDING.md` was edited by the rename.** Commit `7af7fd5` rewrote 128 lines of the
  Carbon-owned file (Bridge → Extend, `bridge.teamofsilicons.com` → `extend.teamofsilicons.com`),
  although the file says agents must not edit it. Review the diff (`git show 7af7fd5 --
  understanding/UNDERSTANDING.md`) and keep or revert it.
- **Contract edits made on 2026-09-26/27 need review**, because each of these files says changes
  need a Carbon's approval: `understanding/cli.yaml` (CLI agent: `adb` arguments are verbatim, `--`
  rules, local-file-only `install`, `record start --quality`); `understanding/TECHNICAL.md`
  (as-built corrections, listed in its status line); `understanding/api.yaml` (the 8 MiB attachment
  limit is a total, not per file; device error codes on `ok:false` results; `file keep` acts in
  Extend only; a file's `name` versus its Briefcase name).
- **TECHNICAL.md open questions 1–15**, including the new ones: Briefcase share rights
  (Briefcase refuses `write` on files, so Extend shares read + update), downloading Briefcase files
  through Extend, Briefcase file ids as `install`/`replay`/`display` inputs, and what
  `--quality normal` means on Mac and Linux.
- Linux now **refuses** `record start --quality` (before, the option was accepted); a project config
  that sets quality now makes Linux recording fail. Refusing rather than ignoring it is the fork
  agent's choice and needs confirming.
- **Licences:** whether the root MIT `LICENSE` also covers the Android app, the website and the
  packaging (the Android app's notices state no licence for Extend's own code), and whether the
  LGPL-3.0 approach for spake2-android (separately loaded `.so`, unobfuscated classes, source linked,
  re-signing explained) is sufficient.
- **Revoke wording** on the Mac: `UNDERSTANDING.md` names the action "Revoke pair"; the desktop
  restyle kept that label and titles the confirmation "Unpair this Mac?".

## Physical devices and platforms

Nothing below has run. Record the exact device, OS version and operations when it does, and keep
simulator or mock results separate.

- **Android:** physical phones and tablets (a Pixel 8 was attached to this Mac but not used),
  Android 11–12, physical Android TV, Google TV and Fire TV (`amazon.hardware.fire_tv` detection and
  Fire OS settings paths come from documentation), TV D-pad scrolling of the licences screen.
- **Mac:** the GUI checks the fork and desktop agents could not run without driving the Carbon's
  desktop (listed in `verification.md`), two displays, hidden Stage Manager windows, macOS 13–15.1,
  the full 30-minute and 1-GiB recording caps, a notarized build installed from a download.
- **Linux:** a real X11 desktop with a compositor and reparenting window manager (GNOME, KDE,
  picom), Tk, Java and GL/Electron apps, the tray on a real desktop, x64 packages, distributions
  other than Debian trixie, and Wayland recording (the ScreenCast portal is not implemented).
- **Windows:** the Windows driver and `windows/build-zip.ps1` have never run on Windows; they are
  only compile-checked and unit-tested from macOS.
- **Hosted devices:** physical iPhone and iPad (runner signing, Trust, Developer Mode), Apple TV,
  Samsung and LG TVs (mocks only; Apple TV pairing crypto against pyatv's server).

## Engineering work that remains

No decision needed; each has an owner area.

- **Service (Briefcase and Ting).** Register a Silicon as a Ting recipient when it joins a session
  (`TingNotifier::register_recipient` exists; the call belongs in `routes/sessions.rs`); until then a
  real Ting refuses requests with `recipient_not_registered` and they stay `pending`. Retry pending
  Ting requests and self-destructs with the sender's latest authorized principal instead of only
  while it has a running session (today a self-destruct after the session ended deletes Extend's
  record but leaves the file in Briefcase). Keep the Briefcase name alongside the device's name.
  `extend file get` and `screenshot --out` fail in Briefcase mode (Briefcase answers 400 to the
  CLI's token); they need a service download route (an `api.yaml` change). Large recordings may need
  Briefcase's staged upload (an OBO proof lives 60 s; recordings reach 1 GiB).
- **Service (other).** Re-check session state after taking the session lock (defence in depth for
  late commands). Stop applying agent-device's reserved-flag check to `adb` arguments, which are now
  verbatim (`extend adb shell tool --session x` is refused with an agent-device message). The
  enrollment limit (60 per hour per address, in memory) blocks repeated end-to-end runs against one
  shared service; test runs should use their own instance.
- **Protocol crate.** `capability.rs` still shows `install <app> <file_id|path>`; it should read
  `install <package> <path.apk>` (the CLI overrides the usage line until then).
- **Desktop agent.** `record start --quality normal` fails with `invalid_args` on Mac and Linux: the
  driver passes `normal` to agent-device, which accepts `medium|high` (Mac) or no quality at all
  (Linux), while `cli.yaml` now says `normal` works everywhere. `probe_macos::gather` still runs
  `automationmodetool` and `xcode-select` although nothing uses the result.
- **CLI.** A device-level `session_ended` result exits 1 instead of 6 and does not clear the
  current session.
- **Packaging.** `apps/desktop/runtime-entry.mjs` reads `--state-dir` past a `--`, so text a
  Silicon types (`extend type -- --state-dir=~/notes`) can create a directory and a record file
  there (found by the packaging verifier, not yet fixed). A first command after an upstream-release
  update that is killed on timeout leaves no location record, so the next command restarts the
  daemon once more. `build-package.sh` should list `libxdamage1` and `libxfixes3` in Recommends and
  exclude `__pycache__`; the Linux `.desktop` file has no `Icon=`.
- **Android.** Wireless-debugging discovery uses the first matching mDNS advertisement, so a stale
  `adb-<guid>` advertisement kept the TLS reconnect lane failing on the emulator; try every
  candidate. The app's argument parser ignores `--`; `adb shell -- ls` runs `--`;
  `attachment:` inside `adb shell` arguments is rewritten; an HTTP 429 shows as "Can't reach
  Extend". Large APKs above the 8 MiB attachment limit install only through the parts recipe
  (verified with host adb, not through Extend's own ADB connection).
- **Fork (Linux recording).** Plain Xlib windows with background `None` and no `_NET_WM_PING` can
  still show a former cover; a second recorder on an already-redirected window can record the
  cover (`vendor/agent-device/FORK.md`).
- **Look.** The critics' remaining low-severity notes for the website, the desktop window and
  banner, and the Android app (for example the update-required card has no action, a takeover still
  shows "In use" in the desktop top bar, the Android notification has no accent colour, the TV focus
  ring is ink rather than cobalt, and the licences screen shows 72-column hard-wrapped text).
- **Licence texts in artifacts.** The CLI archive, the service image, the desktop packages and the
  website do not yet ship the licence texts of their Rust crates and fonts
  ([`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) lists them).
- **CI.** `cargo fmt --all --check` fails on 78 files (formatting predates this work; the
  integration step formats). CI does not run the Android, Swift helper, agent-device fork, packaging
  or Linux recording lanes. The fork's `check:affected` and layering gates cannot run from this
  nested checkout.

## Done on 2026-09-26/27

Summarised here; each item's commands and results are in `verification.md`.

- **Renamed** Silicon Bridge to Silicon Extend: crates, the `extend` CLI, `EXTEND_*` settings,
  `Extend-Device`/`Extend-Enrollment` auth schemes, `Silicon-Extend-API-Version`, `ees_`/`edc_`
  credential prefixes, `extend.teamofsilicons.com`, the Android package
  `com.teamofsilicons.extend`, and the `extend`/`extend_global`/`extend_test_*` schemas.
- **New mark and restyle** of the website, the desktop window and banner, and the Android app, as a
  sibling of Silicon Interface (IBM Plex Sans and Mono, Source Serif 4 titles, cobalt on paper,
  risograph orange-red for Stop).
- **Review fixes**, each re-verified by a separate agent: Android debugging (session retention,
  ADB flow control, recording timeline, delivery retries, trust checks, notices, dependency
  pinning); the desktop agent (named Mac apps, held-computer handling, per-device queues, stdin
  arguments); the CLI (`adb` verbatim, local-file inputs, attachment limits); macOS and Linux
  recording in the fork; packaging (checked downloads, signing-aware zip names, daemon replacement
  after a move).
- **Briefcase and Ting** exercised against real local services for the first time; three Briefcase
  defects fixed in the service.
- **Licensing:** `LICENSE` (MIT) and `THIRD_PARTY_NOTICES.md`; the Android app shows its notices.
