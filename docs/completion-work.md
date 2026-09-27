# Open gates

The current list of what stands between this checkout and a released Silicon Extend 1.1.0,
updated on 2026-09-28. The Carbon-owned
[`understanding/UNDERSTANDING.md`](../understanding/UNDERSTANDING.md) is the product authority;
nothing here changes it. An implementation, a passing mock or an emulator run does not close a
physical-device or production gate. What was run, and on what, is in
[`verification.md`](verification.md). The 1.1.0 integration and subsequent fixes are local commits
on `release/1.1.0`; later work may still be in progress.

*1.0.0 went live on 2026-09-27 (the service, the website, the CLI through Honeycomb, and the apps);
the sections below from "Needs the Carbon" on are the 1.0.0 record. What 1.1.0 still needs comes
first.*

## 1.1.0

1.1.0 (devices belong to the Carbons who paired them, several Carbons per device, waking a device,
setup retry, the device engine named Silicon Extend) is built and its automated suites pass together
(`verification.md`, "1.1.0 integration"); its design is
`extend-publish-drafts/release-1.1.0/design.json` with the Carbon's decisions of 2026-09-27.

### Needs the Carbon

- **Review the concrete contract patch** for `understanding/TECHNICAL.md`, `api.yaml` and `cli.yaml`:
  `../extend-publish-drafts/release-1.1.0/contracts/REVIEW.md` from the repository root. The copies
  and patch are validated; their protected-file edit approval is pending. `UNDERSTANDING.md`
  changes are already committed (`3fe0347`, `5326b68`) and are not being requested again.
- **Physical TV and iPad availability** is needed for their remaining checks. The iPad is
  unavailable and no physical TV is connected through ADB in the latest local inventory.
  Technical questions 16–20 introduce no new product decision: accepted first-pair terminal and
  routed-request behavior is retained, the Ting premise is corrected, and iOS awake-state evidence
  remains a physical verification task. Transferring terminal ownership would be a separate change.
- Production Ting registration is complete: the owning Team's manager registered the three wake
  types and listing confirmed all four Extend types. Each separate test context needs its own
  supported setup. Delivery Teams do not need duplicate registrations; no OBO `types.register`
  exists. No notification was sent as part of the production registration.
- The obsolete Ting request is now marked superseded in `docs/requests/ting-app-level-types.md`.
  No maintainer message was sent; the unsupported per-Team premise is not a release dependency.
- **A Carbon's logout** uses the accepted `access_removed` behavior (`TECHNICAL.md` C9 and the
  saved design); verification remains required, but the reason does not need another decision.

### Release access checked on 2026-09-28

The existing 1.0 Mac signing identity and `extend-release` notarization profile work; that release
was accepted by Apple. The permanent Android key exists under `~/.silicon-release/extend`.
Preserve those identities for update compatibility. Honeycomb, Ting, GitHub and Vercel sessions
are authenticated, and the published backend and GitHub assets remain 1.0.0. The Carbon renewed
AWS profile `silicon-production`; STS access and the existing production stack were verified.
Historical signing and "nothing published" entries below are not current blockers. The 1.1 Mac
candidate passed real Apple notarization, stapling and Gatekeeper, and the six CLI targets plus
Linux/Windows desktop packages built successfully in release workflow `36357336309`. The first
1.1 CI run exposed consumer-contract setup races and a misplaced browser test; fixes and a green
rerun are required. No 1.1 publication or deployment has run.

### Engineering left for 1.1.0

The interrupted final-fix pass resumed on 2026-09-28. The shared banner setting, service/client/CLI
controls, website setup and settings, Android setup switch and notification/badge timer, and
desktop timer/icon hiding are implemented locally. Android TV's phone-only background exemption
step is removed. See the new verification entry; this is not a released build.

Remaining from the Carbon's final requests, before the release gates below:

- Finish remaining native banner verification. Mac native collapse/restore, position retention,
  edge clamping, ten-second hiding, Stop and focus preservation now pass. Physical dragging is
  unverified because CUA cannot move a standard native title bar either. Verify human dragging,
  multi-monitor movement and Windows native behavior. Linux drag/collapse/restore/Stop now pass
  on an owned X11 desktop after fixing GTK's 200-pixel minimum banner height; physical
  GNOME/KDE behavior remains open. TV bottom-centre placement and
  ten-second hiding pass on the owned API 34 emulator; physical TV verification remains.
  Carried-device controls, durable offline choices, shared aliases and restart timing are built;
  a metadata-only attach preserves the live driver, but still needs a carried-device
  real-recording check. Local Linux app/full-screen recordings survive host banner/name updates.
- Reproduce the reported debugging disconnect on the physical TV and inspect its logs. Fresh
  Android TV 14 and Android 9 (TV-mode) emulators recover automatically after app process death
  and adbd restart; the repeatable lane and timings are in `verification.md`. No production
  reconnect defect was reproduced there. Measure and reduce overall TV memory use; initial
  debug-build background baselines are recorded, not a physical-TV memory result.
  Screenshot peak memory is reduced locally by preserving raw ADB PNGs and streaming transformed
  images from disk, with immediate bitmap cleanup; see the measured workload in `verification.md`.
- iPhone/iPad: first-screenshot attachment is implemented and verified through the real engine on
  an isolated iPad simulator, preserving the current screen without launching an app or a runner.
  Verify it on a physical iPad as well (including disconnect/reconnect and a new session).
- TV image failures now reach the command result; bounded downloads, downsampling and asynchronous
  readiness are implemented and verified on an isolated Android TV emulator. Verify on the physical
  TV. Stored Extend images/videos now resolve to ordinary device attachments with Silicon/Team,
  expiry, active-session and bounded-read checks. The real Briefcase/native TV emulator handoff
  passed 12 checks, including actual replayed pixels and failures; the physical TV remains open.
  Stored media uses the existing combined
  8-file/8-MiB attachment limit. Overall TV memory use and debugging reconnect still need the work
  listed above.
- Re-run the final feature and requirements audit and integration checks after those fixes;
  reconcile the proposed API/CLI/technical drafts, build/sign/notarize the final artifacts, then
  publish/deploy and verify the release. None of this checkpoint updates installed apps.


- The 1.1 real IAM/Ting lane now covers wake events and routing across Teams, genuine missing-type
  failures, self-send and Carbon removal. Unsupported automatic registration was removed; service,
  CLI and web guidance points to the app-owning Team. Keep these checks in final integration.
- Direct Android TV element click is fixed for apps 1.1+, with native navigation proof and frozen
  1.0 response compatibility. Verify it on the physical TV; coordinate/repeated/held clicks still
  depend on gesture injection, and no pointer/touch capability is advertised.
- The Simulator's copy of the iPhone helper's runner could show "Silicon Extend" by copying the
  helper's display name into it before its re-sign (optional; a device runner can't be changed).

### Release gates

- Automatic file sharing still has a known Briefcase dependency: a delegated invitation to a
  Carbon Briefcase has never seen is refused with `invalid_principal`. Extend retains the file
  and reports the sharing failure, but it does not automatically re-share that file later. The
  successful real-service/native-TV sharing checks first sign the Carbon into Briefcase; they
  do not close the first-time-recipient requirement in `UNDERSTANDING.md`'s Files section.
  Resolve the upstream recipient-projection behavior and verify the first-file path, or obtain
  an explicit scope decision before calling the full file-sharing requirement complete.
- The real-service fixture gates passed: a Silicon reads its granting Carbon's directory entry
  (200, then 404 after removal), and Ting accepts a self-addressed notification (202). Re-run against
  the final release candidate and verify production configuration after deployment.
- The production-schema-copy upgrade/down/forward rehearsal passes with synthetic data and exact
  identity/grant/credential preservation checks. Take a fresh production snapshot before deployment;
  the schema-only rehearsal does not prove restoring actual production data from a full backup.
- The released 1.0 CLI/current CLI rehearsal passes, including shared saved logins and sessions,
  mixed-version takeover/Stop and all 119 current CLI checks. The actual 1.0 website-source/current
  website rehearsal also passes 13 checks on one origin with the login retained. The actual Mac
  1.0 native agent/current agent rehearsal passes seven groups: saved identity/credential/session,
  native Stop and two-Carbon terminal rules, using an isolated headless file-store fixture.
  The signed Android upgrade and native two-Carbon TV-emulator lane passed twelve checks, with
  pairing/credential/session continuity and the native sharing/Stop/removal behavior recorded.
  These do not close physical TV, installed Mac UI/Keychain upgrade, credential
  rotation or carried-device linking checks.

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
