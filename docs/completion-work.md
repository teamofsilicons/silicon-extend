# Open gates

The current list of what stands between this checkout and a released Silicon Extend, as of
2026-09-27 after the second round of fixes. The Carbon-owned
[`understanding/UNDERSTANDING.md`](../understanding/UNDERSTANDING.md) is the product authority;
nothing here changes it. An implementation, a passing mock or an emulator run does not close a
physical-device or production gate. What was run, and on what, is in
[`verification.md`](verification.md). Round 2 is not committed yet: it is the working tree on top of
`5b3c578`.

*1.0.0 went live on 2026-09-27 (the service, the website, the CLI through Honeycomb, and the apps);
the sections below from "Needs the Carbon" on are the 1.0.0 record. What 1.1.0 still needs comes
first.*

## 1.1.0

1.1.0 (devices belong to the Carbons who paired them, several Carbons per device, waking a device,
setup retry, the device engine named Silicon Extend) is built and its automated suites pass together
(`verification.md`, "1.1.0 integration"); its design is
`extend-publish-drafts/release-1.1.0/design.json` with the Carbon's decisions of 2026-09-27.

### Needs the Carbon

- **Approve the 1.1 drafts** of `understanding/TECHNICAL.md`, `api.yaml` and `cli.yaml`, and commit
  the `UNDERSTANDING.md` edits they follow.
- **Answer `TECHNICAL.md` open questions 16–20**: the terminal when the installer's pair ends, the
  terminal rule and terminal apps on the screen, the device for the iPhone lock-state check,
  app-level Ting types, and routed requests to a Carbon who shares no Team with the asking Silicon.
- **Ting types in every Team.** A Ting manager of each Team Extend sends in registers Extend's four
  types (`docs/deployment.md`, "Releasing 1.1.0", step 1); in production only
  `extend.device.requested` in `tos` exists today.
- **Send the Ting request**, `docs/requests/ting-app-level-types.md`, to the Ting and Honeycomb
  maintainers.
- **A Carbon's logout** ends their Silicons' sessions as `access_removed` (the Silicon's hint says the
  Carbon took access away or signed out); `stopped_by_carbon` was the other choice. Confirm
  (`TECHNICAL.md` C9).

### Engineering left for 1.1.0

The interrupted final-fix pass resumed on 2026-09-28. The shared banner setting, service/client/CLI
controls, website setup and settings, Android setup switch and notification/badge timer, and
desktop timer/icon hiding are implemented locally. Android TV's phone-only background exemption
step is removed. See the new verification entry; this is not a released build.

Remaining from the Carbon's final requests, before the release gates below:

- Finish the banner audit on actual app surfaces: carried-device switches in the host app (the
  website and CLI can already set each carried device), offline desktop preference persistence,
  and restart/reconnect behavior. Exercise drag/collapse, ten-second hiding, takeover persistence,
  Stop after hiding and TV bottom-centre placement on native targets. A metadata-only attach now
  preserves the live driver; verify it during a real recording as well.
- Diagnose and repair the TV's Android debugging disconnect/reconnect after process death;
  measure and reduce its memory usage. These are not covered by the banner changes.
- iPhone/iPad: first-screenshot attachment is implemented and verified through the real engine on
  an isolated iPad simulator, preserving the current screen without launching an app or a runner.
  Verify it on a physical iPad as well (including disconnect/reconnect and a new session).
- Finish the TV display failure path and allow a Silicon to display its own Extend files, with
  correct owner/session authorization and bounded downloads. Verify actual image decode failures
  are returned as failures.
- Re-run the final feature and requirements audit and integration checks after those fixes;
  reconcile the proposed API/CLI/technical drafts, build/sign/notarize the final artifacts, then
  publish/deploy and verify the release. None of this checkpoint updates installed apps.


- `e2e/real-iam/realiam.py --ting` doesn't know 1.1 yet: seed Extend's four types in acme and globex,
  show a third Team reporting them missing, and cover waking, woken, declined and routed requests.
  It also has to confirm that Ting offers the `types.register` call the service uses to register
  missing types for a Team's Ting manager (`TingNotifier::register_type`); if not, the service keeps
  showing the command.
- `extend click` doesn't reach an Android TV: its capabilities have no pointer or touch, so the TV
  click fallback is only reached through `find … click`. Allowing it means adding `input.pointer`
  to Android TV's full capabilities in the protocol crate, which the Android work judged not
  additive for 1.0 readers.
- The Simulator's copy of the iPhone helper's runner could show "Silicon Extend" by copying the
  helper's display name into it before its re-sign (optional; a device runner can't be changed).

### Release gates

- `e2e/real-iam/realiam.py --ting` against real IAM and Ting: a Silicon reading its Carbon's directory
  entry (200, then 404 after removal), and Ting accepting a Ting whose recipient is its own sender.
- The rollback down step rehearsed on a copy of the production schema, and the roll-forward.
- A release rehearsal in a test environment: the 1.1 service driven by the 1.0.0 CLI, website,
  Android app and desktop agent; then each upgraded, including two Carbons on one Android TV and on
  one Mac (credential rotation, carried-device linking, the remote-stop rule, the terminal rule).

### Physical devices

- Android: Pixel and Samsung lock screens (what the wake notification shows), an Android TV in
  standby, a Fire TV, the keep-screen-on overlay during a session, API 26, 29, 34 and 36.
- Mac, Windows and Linux: the awake report on lock, unlock and sleep; the wake notification; the
  display kept on during a session and released after; session processes ended at session end
  (including `setsid`/`start`), on macOS 15 and 26, Windows 10 and 11, Ubuntu GNOME and KDE.
- iPhone and iPad: the renamed helper (Silicon Extend Helper) installing and replacing the old one;
  a lock-state reading on a real device before Extend reports their awake state.
- Apple TV, Samsung and LG: power refused while asleep or in standby, and the awake mapping.

## Needs the Carbon

### 1. The name and the GitHub repository

Settled on 2026-09-27. The Carbon deleted the unrelated repository that held the name, and Extend
now lives at `teamofsilicons/silicon-extend`. The crates are `silicon-extend-protocol`,
`silicon-extend-client` and `silicon-extend-cli`; the Honeycomb app is `extend`.
`teamofsilicons/silicon-bridge` is an unrelated older product and is left alone.

### 2. Signing and notarization

- Which **Developer ID Application** identity signs Mac releases. It should be the Team's; builds so
  far were signed locally with a Developer ID found in this Mac's keychain. macOS ties Accessibility
  and Screen Recording grants to the signing identity, so changing it after Carbons have granted
  them resets those grants.
- A `notarytool` Keychain profile for `NOTARY_PROFILE`. Notarization is implemented in
  `apps/desktop/macos/build-app.sh` and exercised only with stubbed Apple tools; it has never run
  against Apple.
- The Android **release signing key**: `assembleRelease` now builds an unsigned APK and signs it only
  when `EXTEND_ANDROID_SIGNING_PROPERTIES` points at the key's properties.
- Which Apple development team signs the iPhone/iPad runner (TECHNICAL.md open question 11).

### 3. Publishing

Nothing is deployed or published. Each needs the Carbon's go-ahead and credentials, and existing
infrastructure must be checked before anything is changed:

- The Extend service at `backend.extend.teamofsilicons.com` (one instance; `docs/deployment.md`).
  `deploy/aws/` now holds a stack for one ARM64 EC2 host behind Caddy with a private RDS database,
  and its first-deploy and release steps; it has not been deployed.
- The configuration website at `extend.teamofsilicons.com` (Vercel), and DNS for both.
- The CLI: tag, `release.yml`, `honeycomb releases upload` from a Carbon session.
- Downloads of the Mac, Linux, Windows and Android apps. The website's download pages link to the
  latest GitHub release under stable names (`web/src/config.ts`), so they return 404 until that
  release is published.
- Silicon IAM registration of Extend's OBO catalog, including Briefcase's approval of
  `invitations.create` (critical), and of the webhook endpoint with its signing secret (production
  now refuses to start without `EXTEND_IAM_WEBHOOK_SECRET`).
- Production smoke tests after each.

### 4. Contract and product decisions

- **`TECHNICAL.md`, "Carbon decisions after round 2" (C1–C9):** logout elsewhere ended by a 15 s
  heuristic (IAM sends no logout events); self-destruct and Ting retries depending on logins held in
  memory; the 1,000-character raw reason cap; the `activate` participant action; IAM event records
  kept across a clean; the bug-report address (`gmail.com` in the build, `gmails.com` in
  `UNDERSTANDING.md`); the CLI's JSON shape and `login status` exit 0; IAM's sign-up address; and
  Carbon logout. Its numbered open questions 1–12 and 14 still stand; 13 and 15 were settled by
  building them.
- **Contract edits need review**, because each of these files says changes need a Carbon's
  approval. Round 1: `cli.yaml` (`adb` arguments verbatim, `--` rules, local-file-only `install`,
  `record start --quality`), `TECHNICAL.md`, `api.yaml` (the 8 MiB attachment total, device error
  codes, `file keep`, file names). Round 2: `api.yaml` (the file download route, removed devices,
  request semantics and `last_error`, `warnings`, deprecation headers and 410, the contracts matrix,
  test-environment errors on every route, world-bound pairing codes, the lifecycle rules, close code
  4503, the webhook rules), `cli.yaml` (the JSON shape, `login status`, `EXTEND_TEST_SECRET`,
  `--verbose`, `config home --use-existing`, `device ls --removed`, downloads, `version`), and
  `TECHNICAL.md` (the as-built notes of round 2).
- **`UNDERSTANDING.md` was edited by the rename.** Commit `7af7fd5` rewrote 128 lines of the
  Carbon-owned file (Bridge → Extend, `bridge.teamofsilicons.com` → `extend.teamofsilicons.com`),
  although the file says agents must not edit it. Review the diff (`git show 7af7fd5 --
  understanding/UNDERSTANDING.md`) and keep or revert it.
- **Licences:** whether the root MIT `LICENSE` also covers the Android app, the website and the
  packaging (the Android app's notices state no licence for Extend's own code), and whether the
  LGPL-3.0 approach for spake2-android (separately loaded `.so`, unobfuscated classes, source linked,
  re-signing explained) is sufficient.
- **Revoke wording** on the Mac: `UNDERSTANDING.md` names the action "Revoke pair"; the desktop
  window keeps that label and titles the confirmation "Unpair this Mac?".

## Physical devices and platforms

Nothing below has run. Record the exact device, OS version and operations when it does, and keep
simulator or mock results separate.

- **Android:** physical phones and tablets, Android 11–12, physical Android TV, Google TV and Fire
  TV (`amazon.hardware.fire_tv` detection and Fire OS settings paths come from documentation), TV
  remote buttons through `input keyevent` (unit tests only), the after-restart prompt on a real
  reboot, TalkBack, the TV D-pad on the licences screen.
- **Mac:** the GUI checks that would drive the Carbon's desktop (listed in `verification.md`), start
  at login after a real login, a real lock screen and display sleep, the Stop rows for carried
  devices in a real menu bar and banner, two displays, macOS 13–15.1, the full 30-minute and 1-GiB
  recording caps, a notarized build installed from a download.
- **Linux:** a real X11 desktop with a compositor and reparenting window manager (GNOME, KDE,
  picom), the one-time flicker at app-recording start, Tk, Java and GL/Electron apps, logind's
  lock state on a real desktop, the tray, x64 packages, distributions other than Debian trixie, and
  Wayland recording (the ScreenCast portal is not implemented).
- **Windows:** the Windows driver and `windows/build-zip.ps1` have never run on Windows; they are
  only compile-checked and unit-tested from macOS.
- **Hosted devices:** physical iPhone and iPad (runner signing, Trust, Developer Mode), Apple TV,
  Samsung and LG TVs (mocks only; Apple TV pairing crypto against pyatv's server).

## Engineering work that remains

No decision needed; each has an owner area. Items marked *(verifier)* were found by round 2's
verifiers and not fixed.

- **Release and CI.**
  - The new CI jobs (`fork`, `android`, the named contract step) have not run on GitHub. CI still
    doesn't run the website's real-service lane, the Swift helper tests, the Linux container lanes
    or anything on an emulator. The fork's full vitest suite has failures that predate Extend, so CI
    runs only the files Extend touched; `check:affected` and the layering gates can't run from this
    nested checkout.
- **Service: sessions, requests, files.**
  - *(verifier)* A `not_a_team_member` refusal ends the Silicon's sessions in that team as
    `left_team` whenever IAM doesn't confirm membership, including when IAM can't answer (no other
    member's login held, or it expired); a Silicon still in the team whose login was approved for
    another team would lose its sessions with the wrong reason. End only when IAM says "not a
    member", as the logout path already does.
  - *(verifier)* `request_send` folds a repeat before the idempotency lookup: the same key and body
    within 60 s answer 200 through the repeat path instead of replaying the stored 201; two
    concurrent identical sends can both be stored (as before).
  - *(verifier)* The pending-request message says it "is sent when <sender> next uses Extend", but
    the request fails after 6 attempts (about 2.5 minutes). The file download's 404 has no hint.
  - A takeover released while a command runs resets the idle window to 300 s (matters only for a
    command started within 2 s of the release with the maximum 300 s timeout).
  - Logins used for self-destruct, Ting retries and identifying a refused Silicon are held in
    memory: a restart forgets them (Carbon decision C2 for the durable fix).
  - The download route holds the whole file (up to 1 GiB) in memory; stream it. Large recordings
    may need Briefcase's staged upload (an OBO proof lives 60 s; recordings reach 1 GiB).
  - The reserved-flag check (`--session`, `--device`, …) still applies to `adb` arguments, which are
    verbatim (`extend adb shell tool --session x` is refused).
  - Timestamps are serialised with microseconds, where `TECHNICAL.md` §1 says milliseconds.
- **Service: devices.**
  - *(verifier)* Retrying the pairing that filled a test environment, with the same
    `Idempotency-Key` and body, answers `409 test_device_limit` instead of replaying its 201: the
    early limit check runs before the idempotency lookup. Move it inside, after the replay, with a
    test.
  - A host that was offline when a device it carries was removed is never sent `attach
    removed:true` on reconnecting, so it keeps carrying and probing that device; renaming a carried
    device sends the host only `refresh`, so the host shows the old name until it reconnects; the
    hub keeps a carried device's last online state after its host reconnects, until the host
    reports again.
  - There is no route for a host computer to revoke a device it carries (the desktop window sends
    the Carbon to the website instead).
- **Service: identity.** Extend uses `silicon-iam-client` 4.0.0 from crates.io, and parses IAM 4
  events itself where the SDK rejects a non-UUID aggregate id; drop that parser once an SDK
  release accepts them.
- **Test infrastructure.** The service suites leave one throwaway database per test
  (`extend_e2e_*`, `extend_core_*`, `extend_gaps_*`, `extend_contracts_*`); nothing drops them
  automatically, so run `e2e/clean-test-dbs.sh` (which drops all four) after a run. On 2026-09-27 the
  development PostgreSQL had collected 1,307 of them. The enrollment limit (60 per hour per address,
  in memory) still blocks repeated end-to-end runs against one shared service.
- **Protocol crate.** `capability.rs` still shows `install <app> <file_id|path>`; it should read
  `install <package> <path.apk>` (the CLI overrides the usage line).
- **CLI.** `-v` shows request ids only for failed calls (the client crate doesn't expose the
  `x-request-id` of a successful answer).
- **Desktop and packaging.** `probe_macos::gather` still runs `automationmodetool` and
  `xcode-select` although nothing uses the result. `build-package.sh` doesn't exclude `__pycache__`,
  and the Linux `.desktop` file has no `Icon=`.
- **Android.** The app's argument parser ignores `--`; `adb shell -- ls` runs `--`; `attachment:`
  inside `adb shell` arguments is rewritten. After `am instrument` or an app update the foreground
  service returns only when the app is opened. APKs above the 8 MiB attachment limit install only
  through the parts recipe. The apps don't dump their own frames for the contract fixtures yet
  (`contracts/v1/device` is derived by hand).
- **Website.** Sign-up guesses IAM's `/signup` address (Carbon decision C8).

## Done in round 2 (2026-09-27)

Summarised here; each item's checks are in `verification.md`, and the contracts describe the result.

- **Service:** the idle timer holds during a command; a session ending mid-command answers at once;
  a refused login on a session route ends the Silicon's sessions; Ting recipients are registered at
  session start; pending requests are retried with the sender's latest login and fail with a
  reason; self-destruct keeps its record until Briefcase confirms; a file download route; storage
  problems reported as `warnings`; every new request reason delivered, raw.
- **Devices:** removed devices readable to their Carbon; an atomic test device limit that can't
  starve other requests; hosted devices tested end to end; full pages with the online filter.
- **Test environments:** the secret checked on every route; readiness from IAM (or `activate`);
  disable without unpairing (close code 4503); pairing codes bound to their world; the 10-slot limit
  on every move into an active state; durable, ordered lifecycle receipts; the clean fence;
  webhooks recorded only after they apply, in aggregate order; logout by refresh token; production
  as the image default and the webhook secret required.
- **Versioning:** majors side by side, deprecation and sunset with headers and 410, the matrix from
  live state, and consumer contract fixtures replayed against a real service.
- **CLI:** the house JSON shape, the test-environment line on every failure, `EXTEND_TEST_SECRET`,
  full `device ls` paging and `--removed`, help that follows the connected device, strict grammar
  with hints, `--verbose`, validated settings, `config home` moving the state, downloads through
  Extend, attachments in the client crate, and `extend version` reading the matrix.
- **Website:** the Removed tab and read-only removed-device page, the design critic's leftovers,
  and sign-up through IAM.
- **Android:** no enrollment loop, a precise 429 message, "Silicon Extend TV", the after-restart
  prompt for Wireless debugging, every mDNS candidate tried, TV remote keys through ADB, and the
  restyle leftovers; the old `com.teamofsilicons.bridge` app is gone from the emulator.
- **Desktop and fork:** `--quality normal|high` on Mac and Linux, `--` respected by the runtime
  entry and the record written before hand-off, start at login by default, locked and asleep
  computers reported, Stop for carried devices everywhere, "Download the update", terminal-only on a
  headless Linux box, app recording refused when isolation can't be guaranteed, packaging checks
  (stamp errors, required entries, content-hashed dist freshness), and the Linux package's X11
  libraries listed.
- **Licences:** the CLI archive, the service image and the desktop packages now ship `LICENSE`,
  `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` (generated by `cargo about`).
