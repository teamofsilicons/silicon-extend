# Silicon Extend for Mac, Windows and Linux: packaging and end-to-end runs

The app is one Rust binary, [`crates/extend-agent`](../../crates/extend-agent/README.md). This
directory packages it and holds the Linux end-to-end environment. The published 1.0 macOS app
is Developer ID signed and notarized. The 1.1 candidate has also passed Apple notarization,
stapling and Gatekeeper assessment; publication and the remaining native checks are tracked in
[`docs/completion-work.md`](../../docs/completion-work.md).

| Path | What it does |
|---|---|
| `macos/build-app.sh`, `macos/Info.plist.in` | Builds `target/desktop/macos/Silicon Extend.app` and a zip named for how it was signed |
| `macos/icon/` | The app icon (`AppIcon.svg` → `make-icns.sh` → `AppIcon.icns`), Extend's mark |
| `runtime-entry.mjs` | Extend's entry for the packaged device engine runtime (replaces a daemon started from another install path) |
| `stamp-runtime.mjs` | Stamps the packaged runtime's version with a content digest |
| `packaging.sh` | Sourced by both build scripts: the checked stamp step (`stamp_runtime`) and the dist freshness check (`require_fresh_dist`) |
| `dist-manifest.mjs` | Records which source `vendor/extend-engine/dist` was built from, and checks it before packaging a dist that can't be rebuilt |
| `linux/build-package.sh` | Builds the Linux tarball and `.deb` layout (run it on Linux) |
| `linux/build-in-docker.sh` | Runs `build-package.sh` in the linux-e2e image, then installs the `.deb` in a container and runs it |
| `windows/build-zip.ps1` | Builds the Windows zip (run it on Windows) |
| `linux-e2e/` | A real X11 desktop in Docker (`Dockerfile`, `build-image.sh`, `run.sh`, `e2e.sh`) and the recording lanes |
| `banner-ui.e2e.mjs` | Runs the actual desktop WebView page in headless Chromium: carried switches, offline status, Stop, drag, collapse and takeover controls |

The app's in-use banner controls apply immediately, including while disconnected. Choices live
in the private `.extend-agent/indicators.json`, scoped to the service URL, and synchronize in the
background when a pair connects. Each carried device has its own switch in the host app. Stop
and takeover controls remain available when the banner is hidden. Restarting or reconnecting does
not restart an old session's ten-second announcement; new sessions get a new announcement.

Run `node --test apps/desktop/banner-ui.e2e.mjs` after installing `web`'s development dependencies
and Playwright Chromium. This checks rendering and WebView messages; native window movement,
multi-monitor placement and driver recording continuity still require native verification.

## macOS app

```
apps/desktop/macos/build-app.sh                 # release
PROFILE=debug apps/desktop/macos/build-app.sh   # faster, for checking the layout
SIGN_IDENTITY='Developer ID Application: Your Name (TEAMID)' apps/desktop/macos/build-app.sh
# With an existing notarytool Keychain profile, submit, staple and assess before making the final zip:
SIGN_IDENTITY='Developer ID Application: Your Name (TEAMID)' NOTARY_PROFILE=extend-release apps/desktop/macos/build-app.sh
```

```
Silicon Extend.app/Contents/
  Info.plist                    LSUIElement (menu-bar only), usage descriptions, com.teamofsilicons.extend,
                                CFBundleIconFile AppIcon
  MacOS/extend-agent            the app
  MacOS/Silicon Extend Helper   the engine's macOS helper, built here and signed with the app
                                (1.0 named it agent-device-macos-helper; after the rename macOS
                                asks for Accessibility and Screen Recording once more)
  Resources/AppIcon.icns        the app icon
  Resources/engine/             the engine's dist/, package.json, LICENSE and the Apple sources it builds
                                on first use (Apple device runners and the native helpers);
                                bin/extend-engine.mjs is Extend's runtime-entry.mjs, and the engine's
                                own entry is bin/extend-engine-cli.mjs
  Resources/node/bin/node       Node 22 (official build, downloaded and cached in target/desktop/.cache)
  Resources/node/LICENSE        Node.js's licence file
```

What 1.1 asks for on the Carbon's Mac, besides Accessibility and Screen Recording: permission to
show notifications (the first start asks once; a Silicon's request to wake the Mac shows as one),
and nothing for keeping the display on during a session (an IOKit power assertion needs no
permission). Notifications need the app bundle: `extend-agent` run from a terminal answers the
service that it couldn't show them.

The zip's name says how the app was signed:

| Zip | When |
|---|---|
| `Silicon-Extend-<version>-macos-<arch>-adhoc.zip` | No `SIGN_IDENTITY`: ad-hoc signed, runs only on the Mac that built it |
| `Silicon-Extend-<version>-macos-<arch>-unnotarized.zip` | `SIGN_IDENTITY` set, no `NOTARY_PROFILE` |
| `Silicon-Extend-<version>-macos-<arch>.zip` | Only after Apple accepted the notarization, the ticket was stapled and `spctl` accepted the app |

A failed notarization (submit error, a status other than Accepted, a stapling or validation
failure, or an `spctl` rejection) stops with a message saying what happened, why and what to do,
and leaves no zip. The submission goes up as a temporary `notary-submission.zip`, which is always
deleted. Assembling a new app deletes the previous app's three zip variants and its
`notarization.json`. The run ends by printing the zip path and a one-line signing verdict.

- Without `SIGN_IDENTITY`, development builds use an ad-hoc signature. With a Developer ID,
  the app and helper use hardened runtime and secure timestamps; Node receives only the JIT
  entitlements used by the bundled runtime. `NOTARY_PROFILE` requires Developer ID signing
  and completes only after Apple accepts the submission, the ticket is stapled and Gatekeeper
  assessment succeeds. Credentials stay in Keychain.
- Node is pinned to 22.23.3. Both architecture checksums in `macos/node-sha256.txt` come from
  `https://nodejs.org/dist/v22.23.3/SHASUMS256.txt`. A download is checked before it is cached, and
  one that fails is never cached; a cached tarball that fails is deleted and downloaded once more; a
  `NODE_TARBALL` you supply that fails is reported with both hashes and left alone.
  The device engine is rebuilt on every packaging run to include current source changes.
- Mac and Linux packaging stamp the staged runtime version with a SHA-256 build suffix
  (`0.21.15+extend.<64 hex>`). The digest covers packaged code/assets, the Node platform/version
  and (on Mac) the native helper before signing. Changed code therefore triggers the existing
  daemon takeover path; identical copied artifacts keep the same identity despite paths, mtimes or
  signing timestamps. Before writing anything, `stamp-runtime.mjs` checks the staged runtime is
  complete (non-empty entries, every relative import resolvable, no symlinks). The required
  entries include every `internal/*` entry of the engine's `tsdown.config.ts` (bin, daemon, the
  EXTEND_ENGINE_* settings shim `extend-env` that every entry imports first, and the PNG worker,
  Metro companion tunnel, Maestro runScript child and update check, which the engine loads by
  computed paths the import check can't follow); a test fails when the engine adds one that isn't
  listed. Both build scripts stamp through `packaging.sh`'s `stamp_runtime`, which stops
  the build with why and what to do when the stamp fails or is killed by a signal (it prints
  nothing then), and fail unless the printed stamp and the staged `package.json` agree. The vendor
  source manifest and Extend's public version are not changed by packaging.
- The location is the entry's job: before each command that uses the local daemon,
  `runtime-entry.mjs` compares the runtime's real install path with the one recorded in
  `<state dir>/extend-runtime-root.json`. If a daemon of the same version was started from another
  location (the app moved from Downloads to Applications, a translocated or second copy), it stops
  that daemon with the engine's own `daemon stop` and the engine starts a fresh one, which ends
  the old daemon's sessions once. A daemon of another version is left to the engine's own rules.
  Help, `--version`, `daemon …` and remote-daemon runs are not checked. If the stop fails, stderr
  says so with the command to run, and the next command retries. Flags are read only before a
  `--`, as the engine's parser reads them, so text typed after `--` (`type -- --state-dir=~/x`)
  never picks the state directory, the help check or a remote daemon. The record keeps, per
  version, the location its daemon was started from (`{"root", "roots": {"<version>": <path>}}`),
  and it is written before the engine runs, also when a daemon of another release is running:
  a first command after an update that is killed (timeout, SIGKILL) still leaves it, so the next
  command doesn't stop the daemon that location has just started. A claim for one version never
  covers a newer daemon the engine keeps.
- `extend-agent` finds the bundled device engine and Node through `../Resources`. Setting
  `EXTEND_ENGINE` (1.0's `EXTEND_AGENT_DEVICE` still works) or `EXTEND_NODE` overrides them.
- The engine's own settings are `EXTEND_ENGINE_<X>` environment variables. The engine's code reads
  the fork's internal `AGENT_DEVICE_<X>` names, so every entry (and `runtime-entry.mjs`) first
  imports `dist/src/internal/extend-env.js`, which copies each set `EXTEND_ENGINE_<X>` onto
  `AGENT_DEVICE_<X>`: the new name wins, and an old name set alone still works. The runtime entry
  also turns off the engine's update check, which looks for the upstream package.
- Checked on 2026-09-26: the first bundle built, `plutil -lint` passed, and
  `codesign --verify --deep --strict` passed. `extend-agent probe` and `exec apps` ran from the
  bundle with `PATH=/usr/bin:/bin`, so only the bundled Node was reachable. It found the engine
  0.21.15 and listed 227 apps. The later optimized Developer ID build measured 127 MB (43 MB
  zipped); sizes change with every build.
- Checked on 2026-09-27 (in a scratch copy, with pnpm and cargo stubbed and ad-hoc signing): the
  build through symlinked paths, the signing-aware zip names, the checked Node download and the
  notarization failure paths with stubbed Apple tools. See `docs/verification.md`.
- Seen when a freshly assembled, unsigned bundle launches its bundled Node: macOS sometimes held it
  at `_dyld_start` while `syspolicyd` timed out ("ASP: Security policy would not allow process"); a
  fresh bundle path worked. If a Mac build seems to hang right after "Assemble", this is the likely
  cause.
- Start at login: once paired, the app turns it on by itself (a LaunchAgent that points at the
  bundle's binary) unless the Carbon turned it off (the window's switch, the menu, `run
  --no-autostart`, `uninstall-autostart`); `extend-agent install-autostart` turns it on by hand.
  An app opened from Downloads (App Translocation) or a disk image is never registered.

### Native Mac input and recording (2026-09-26)

The signed GUI app uses Accessibility for text entry and ScreenCaptureKit for recording, without
Xcode or an XCTest setup prompt. Recording produces H.264 MP4, accepts 1–60 fps, and records the
selected app or the main display. App recording chooses the display from the app's on-screen
windows matched to its Accessibility windows; other apps are excluded. Recordings are bounded to 30
minutes and 1 GiB and stop when their owning process exits.

As of 2026-09-27:
- `record start --scope app` is refused when the app has no window on screen, and every refusal
  says why ("Native macOS recording could not start: …" with a reason such as
  `app_window_not_on_screen` or `screen_recording_permission_denied` and a hint).
- A stream macOS ends (the menu-bar recording indicator, a display change, revoked capture) still
  returns the video, with a warning; so do the app quitting, the duration and size limits and the
  owner exiting. Time the app had no window on screen is reported.
- On a frontmost-app session, typing goes to the app that is frontmost when the command runs.
- App screenshots include the app's menus, popovers and sheets over its window.
- Verified live on 2026-09-26: manual stop, a short duration limit, a 16-MiB file limit and abrupt
  owner exit (session `105`). Not verified: the full 30-minute and 1-GiB limits, hidden Stage Manager
  windows, two displays, and the 2026-09-27 changes above, whose live checks drive the desktop and
  were not run (`docs/verification.md`).

After pairing the GUI app to the local test service, run:

```sh
python3 apps/desktop/macos/text-e2e.py --record-only --device <local-test-device-id> --artifacts target/desktop/macos/recording-verification
```

This opens an animated fixture, verifies manual and automatic recording stop, then exercises
the service's app selection, capture, upload and CLI download. It fully decodes the resulting
video, checks that frames change and saves a frame for visual inspection. Omit `--record-only`
and use `--record` to include native text-entry checks. These checks require the signed GUI app
to be running; shell-launched helpers can have different Screen Recording permissions. Since
2026-09-27 the fixture window is a known green and its peer pink: screenshots and decoded recording
frames are checked by colour share, cancellation checks that every key pressed was released
(`<state>.keys.json`), and `--extend` adds a frontmost-app switch step. Run it on a test Mac, not on
a Mac someone is using: it drives the desktop.

### Initial Mac run before native input and recording (macOS 27, this machine; historical)

Kept as the record of the first run. The XCUITest blocker described below no longer applies: typing,
recording and app-surface snapshots all use the native helper now.

These ran through the agent's own driver (`extend-agent exec …`). The rest ran through the local
Extend service on `:8480`: pair, a Silicon session, then commands with uploads.

- **Worked:**
  - `probe` detected Accessibility ✓ and Screen Recording ✓ (both granted to the terminal
    running the agent).
  - `open TextEdit`; `open --surface frontmost-app`.
  - `snapshot -i`, which gave 46 nodes with `@eN` refs through the engine's macOS helper.
  - `click @e6`, which answered "Tapped @e6 (511, 374)".
  - `screenshot`, which made a PNG and uploaded it to the service (a file id and URL came back).
  - `appstate`, `apps` (227 apps) and `close`.
  - `terminal run …`, including a non-zero exit, which came back as `command_failed` with
    `exit_code`.
  - A reserved flag was refused, by the agent and by the service.
  - Stop from `extend-agent stop` and from the in-use banner's **Stop** button: the service ended
    the session with `stopped_by_carbon`.
  - The Carbon removing the device made the app return to a new pairing code, and so did
    `extend-agent revoke --yes`.
  - The tray icon turned orange while in use. The window showed the pairing code, then the paired
    view with the in-use card and "Not available yet". The banner appeared at bottom centre and
    hid when the session ended.
- **Blocked by a macOS prompt nobody could answer:** the engine's **typing** (`fill`/`type`),
  **recording**, and **app-surface snapshots** go through its XCUITest runner. The first use
  showed "XCTest is trying to Enable UI Automation. Touch ID or enter your password" (it's
  `automationmodetool`: "requires user authentication"). `type` through the agent ended as
  `command_timeout` after its 25 s deadline, and the runner was killed on `close`. The probe
  reports this precisely: `input.text` and `screen.record` are `missing`, with the
  `automationmodetool enable-automationmode-without-authentication` fix. The service turns that
  into `unsupported_on_device` for `type`, naming the reason.
- The app's own permission prompts (Accessibility, Screen Recording) were already granted to the
  terminal. The Silicon Extend.app identity itself hasn't been through TCC yet.

## Linux

Wake notifications require the desktop build and a running D-Bus notification service that
supports replacing and withdrawing notifications. If it is unavailable, Extend reports the
notification as not shown and keeps the request in the app and website. It does not invoke an
untrackable notification command that could leave another Carbon's request in notification history.

```
apps/desktop/linux-e2e/run.sh                                                    # driver run on a real desktop
EXTEND_E2E_SERVICE=http://host.docker.internal:8480 apps/desktop/linux-e2e/run.sh    # plus the full agent vs a service
RUN_TESTS=0 …                                                                    # skip cargo test in the container
```

The image (`silicon-extend-linux-e2e`, host architecture, arm64 here) is Debian trixie with Xvfb,
openbox, D-Bus, at-spi2-core, python3-gi with Atspi, xdotool, ImageMagick, scrot, xclip, wmctrl,
GNOME Calculator, Node 22 and Rust 1.98. The repository is mounted read-only, and cargo's caches
live in the named volumes `silicon-extend-linux-target` and `silicon-extend-cargo-registry`.

`e2e.sh` does the following:
1. Builds `extend-agent` with the tray. This also proves the Linux UI (GTK, WebKitGTK,
   appindicator) compiles.
2. Runs `cargo test -p extend-agent` (107 unit and 7 integration tests passed on Linux on
   2026-09-26; on 2026-09-27 the suite, now 142 and 9, was run on macOS, and on Linux only through
   the driver lane with `RUN_TESTS=0`).
3. Checks that a probe with no screen reports `apps.launch`, `replay` and `terminal`.
4. Starts the desktop, then runs the probe and drives GNOME Calculator with the agent's driver:
   - `open gnome-calculator` and `snapshot -i` (64 nodes, keypad buttons with refs)
   - `click @e9` ("7"), then the selectors `role="button" label="+"`, `"5"`, `"="`
   - a snapshot, where the history reads **"7+5 = 12"**
   - `type "3*4"` plus Return, and the history reads **"3×4 = 12"**
   - `screenshot` (PNG), `clipboard write` then `clipboard read` (the round trip)
   - `apps` and `appstate` answer `unsupported_on_device` (the engine has none on Linux)
   - `terminal run` (stdout, stderr, exit 0), then an exit 5 that comes back as `command_failed`
   - a reserved flag refused, and `close`
5. With `EXTEND_E2E_SERVICE`, runs the real agent headless against that service:
   - a Carbon pairs it, the device is online and `ready`, and a Silicon session starts
   - `open`, `snapshot`, `click`, and a `screenshot` uploaded with a URL back
   - `terminal`; the headless status line shows "si:chef is using this computer"
   - `extend-agent stop` ends the session with `stopped_by_carbon`
   - the Carbon removes the device, and the app forgets the credential and shows a new code

Two fork fixes came out of this run, both logged in `vendor/extend-engine/FORK.md`:
- The AT-SPI depth limit (GTK4 buttons were invisible).
- xclip clipboard writes, which always timed out.

GTK4 on X11 tells AT-SPI its window sits at (0,0), so the engine's click coordinates are right
only for a window at the top-left. `e2e.sh` moves the calculator there with `wmctrl`. That is an
engine/GTK4 limitation, noted for later.

Packaging: `linux/build-in-docker.sh` builds the tarball and `.deb` into `target/desktop/linux/`,
installs the `.deb` into the container, and runs `extend-agent probe` and a terminal command from
`/usr/bin`. It rebuilds the bundled device engine before packaging. Node is pinned to
22.23.3; both Linux architecture checksums in `linux/node-sha256.txt` come from
`https://nodejs.org/dist/v22.23.3/SHASUMS256.txt`. Cached and offline archives are verified.
The `.deb` derives native dependencies and minimum versions from the agent and bundled Node
using `dpkg-shlibdeps` (dpkg-dev). Build on the oldest distro you intend to support; a package
built on Debian trixie is not evidence of compatibility with older distributions. Layout:

```
bin/extend-agent
lib/silicon-extend/engine/{bin,dist,linux,package.json}
lib/silicon-extend/node/bin/node
share/applications/silicon-extend.desktop
```

- Runtime dependencies include GTK 3, WebKitGTK 4.1, libxdo and their native dependencies,
  plus `python3-gi`, `gir1.2-atspi-2.0` and `at-spi2-core` for screen reading. The generated
  `.deb` metadata carries the exact native requirements of that build.
- Recommended: xdotool, xclip, ImageMagick, xdg-utils, ffmpeg (with libx264 and x11grab),
  x11-utils, libxcomposite1, libxdamage1 and libxfixes3 (the last three are loaded through ctypes
  by the X11 recorder for app recording, so `dpkg-shlibdeps` can't see them).
- `build-package.sh` rebuilds the fork with `pnpm install --frozen-lockfile && pnpm build` when pnpm
  is on `PATH`, then records what the dist was built from (`dist-manifest.mjs record`: the SHA-256
  of every build input, and of the dist, in `vendor/extend-engine/.extend-build-manifest.json`).
  Without pnpm (inside the linux-e2e container, where `build-in-docker.sh` has built and recorded
  the fork on the host first) it refuses a missing `dist`, one with no record, one rebuilt since,
  and one whose inputs changed, were added or were deleted since, naming the files. Content
  hashes, not timestamps, so the container's copy checks the same as the host's tree.
- The packaged runtime's `bin/extend-engine.mjs` is Extend's `runtime-entry.mjs` (see the macOS
  section); the engine's entry is `bin/extend-engine-cli.mjs`. Node's `LICENSE` ships beside it.
- `linux-e2e/package-install-check.sh` installs the `.deb` with apt into a clean `debian:trixie`
  twice: with Depends only (every library resolves for `extend-agent` and the bundled Node; version,
  probe and the Atspi import work) and with Recommends (also ffmpeg, ffprobe, xwininfo, xdotool and
  the X libraries app recording loads). It needs network access for apt.

To check an installed package against a local development service with local IAM members
`c:alice` and `si:chef`, run from the host (requires the built CLI and ffmpeg):

```sh
python3 apps/desktop/linux-e2e/record-service-e2e.py --package target/desktop/linux/silicon-extend_1.0.0_arm64.deb
```

The lane first builds or refreshes the `silicon-extend-linux-e2e` image (`linux-e2e/build-image.sh`)
and runs `package-install-check.sh` in both modes (`--skip-install-check` skips it offline;
`--install-image` picks the base image). It then installs the `.deb` into a disposable container
and runs it as an unprivileged user with no source runtime mounted. It pairs its own device, opens an animated fixture, records
app and device scopes, uploads through the service, downloads through the CLI, fully decodes
both videos and compares repeat downloads byte for byte. It ends its session, removes its
device and stops its container. Artifacts, `summary.txt` and the two install logs stay under
`target/desktop/linux-recording/service-recording-*/`.
Use `--service-url` and `--container-service-url` when the host/container addresses differ.
This verifies local relay and file storage, not production IAM or Briefcase integration.

## Windows

`windows/build-zip.ps1` (run it on Windows) builds `extend-agent.exe` and zips it with a README.
There's no Node and no device engine: Windows uses Extend's own driver. The window needs WebView2,
which Windows 10 and 11 ship.

The 1.1 release workflow built x64 and arm64 packages on native Windows runners and ran each
packaged agent's `--version` successfully. The driver's pure logic is unit-tested; interactive
window, input, recording, sleep and lock behavior still require native verification.

The release workflow also runs `windows/verify-native.ps1` on its disposable x64/arm64 Windows
runners. It runs native agent unit/fake-service tests, then explicitly opts into the owned-window
and terminal-process fixtures in `tests/windows_native.rs`. Those fixtures are ignored in ordinary
test runs and require the runner opt-in. The captured PNG covers the full disposable runner
desktop; this lane does not establish physical sleep/lock, UAC, multi-monitor or banner behavior.
Its logs, runner metadata and owned-fixture evidence upload even when the checks fail.

### Packaged daemon update verification

```sh
node --test apps/desktop/*.test.mjs
node apps/desktop/runtime-update-e2e.mjs "target/desktop/macos/Silicon Extend.app/Contents/Resources/engine"
node apps/desktop/runtime-update-e2e.mjs <unpacked tarball>/lib/silicon-extend/engine   # Linux
```

The second command takes a packaged runtime (or `vendor/extend-engine`, into which it installs
Extend's entry). When a Node is bundled beside the runtime it reruns itself under that Node and
says which Node ran; it uses the app's macOS helper (`--macos-helper` overrides it) and runs
the engine the way extend-agent does (environment only, arguments over stdin). It works on
isolated copies with an empty session store and prints:

- REPRODUCED: an unstamped update in place reuses the old daemon;
- PASS: a stamped update gets a new daemon;
- PASS: an identical reinstall at the same location reuses it;
- REPRODUCED: without Extend's entry, a moved install reuses a daemon whose location is gone;
- PASS: a moved install replaces it;
- PASS: a second copy starts its own daemon, then reuses it;
- PASS: after that copy is deleted, the remaining install replaces the daemon.

If stopping its daemon fails it prints the pid and keeps its scratch directory. It never opens a
device, does not start the real `extend-agent` or install the `.deb`, and does not exercise an
update during an active device session.


### X11 recording worker development (2026-09-26)

`vendor/extend-engine/linux/screen-record.py` is the native recording worker under development.
It accepts a root screen, explicit X11 window ID or exact application class, writes H.264 MP4, publishes first-frame
readiness, and enforces duration/file limits. SIGINT/TERM/HUP and owner exit finalize the video;
Linux parent-death signaling also stops the encoder if its supervisor is killed.

Run its manual Linux lane in the `linux-e2e` image. `build-image.sh` builds
`silicon-extend-linux-e2e` and labels it with the Dockerfile's SHA-256, rebuilding when the image is
missing or came from another Dockerfile (`--rebuild` forces it):

```sh
apps/desktop/linux-e2e/build-image.sh
mkdir -p target/desktop/linux-recording
docker run --rm --init \
  -v "$PWD":/src:ro \
  -v "$PWD/target/desktop/linux-recording":/tmp/out \
  silicon-extend-linux-e2e bash /src/apps/desktop/linux-e2e/record-e2e.sh
```

The fixture runs only inside the isolated Xvfb desktop. Each invocation preserves artifacts
in a fresh directory. The lane tests manual stop, reduced duration/file limits, owner exit,
supervisor SIGKILL, window/device dimensions, full decoding, nonblank frames and invalid inputs.
It is a manual lane, not selected by CI.

The public runtime connects this worker for `record start --scope device/system`, including
fps, hide-touches, quality and daemon-crash recovery. Linux exports the recorder's own H.264
unchanged, so `--quality` picks its bit rate: medium (Extend's `normal`, the default) 8 Mbit/s,
high 20 Mbit/s, as Android's screenrecord. Extend returns a copied recording artifact and retains its export
for retry. App scope binds the active named app to exactly one mapped WM_CLASS matching its
executable or desktop-file basename, waiting up to 5 s for the window to appear. It requires xdotool
and libXcomposite, libXdamage and libXfixes with the matching X extensions (a missing one is named,
with its Debian package). Multiple matching windows are refused; unmapping, remapping, resizing or
reparenting the window ends the recording (`source-ended`). Minimized-window starts and multiwindow
apps remain unsupported.

How a covered window is recorded (since 2026-09-27, revised the same day): at start the recorder
holds the X server for a moment (about 2 ms for a typical app) while it redirects the window
through XComposite. The off-screen copy the server seeds from the screen is never trusted, since
where nothing covers the window now it can still show a cover that left (a window with background
`None`, the XCreateWindow default, whose app hasn't redrawn). So the recorder has all of it drawn
again, as if it had just been uncovered: it clears the window and every window inside it with
exposures (`XClearArea`), so the server paints each background there is and the app gets real
Expose events for everything, and it reads no frame until every pixel inside the window's shape
has been drawn again, by the app or by the server. Borders (a window's bounding shape less its
clip shape) are only ever painted by the server, so they are never someone else's pixels and are
not waited for. The clear can show as one flicker of the window's background at record start. An
app that advertises `_NET_WM_PING` (GTK, Qt, Chromium/Electron, SDL, Firefox) must also answer a
ping within 5 s. Refused, with no video and "record the whole screen instead (--scope device)": a
window not fully redrawn within 5 s (an app that isn't responding, whose windows have no
background), an app that does not answer its ping, a window holding more than 2,048 windows, an
input-only window. A stopped app whose windows all have backgrounds (xmessage, xev, a plain Xlib
window with a background pixel) is recorded showing those backgrounds, since the server painted
every pixel. The part of a shaped window outside its
shape is recorded black. Frames are stamped on the wall clock at a constant frame rate, so a slow
capture repeats frames rather than playing back fast. The two leaks the verifier's probes found (a
plain Xlib window with background `None` and no ping, stopped after its cover left; a second
recorder on a window another recorder had redirected) are refused now (`record-hung-e2e.py`'s
`plain …` cases and the probe, 2026-09-27). Only Xvfb has been exercised, with no window manager and
with openbox.

Set `RECORD_LANE=record-app-e2e.py` to exercise public named-app open/start/stop with an already
covered target and missing/ambiguous-target refusal. `RECORD_LANE=record-isolation-e2e.py` tests
native isolation; add `RECORD_ISOLATION_EDGE=unmap`, `resize` or `remap` to verify finalization on
source changes. `RECORD_LANE=record-hung-e2e.py` is the cover and redraw lane (25 cases, every pixel
of every frame checked; `RECORD_WM=openbox` adds a window manager, `RECORD_WORKER` compares another
`screen-record.py`, `RECORD_CASES` picks cases; artifacts under `cover-recording-*`).
`record-e2e.py` also covers an owner gone before start, a slow 3000x2000 60 fps capture keeping real
time, and a recording started before the window maps. The app, isolation and cover lanes check every
frame. These are manual Xvfb lanes, not CI or physical-desktop evidence. Real service transfer,
other desktop/app coverage and Wayland portal/PipeWire support remain open. Wayland deliberately
refuses this worker, including XWayland displays.

The public daemon and Extend driver lane is `record-runtime-e2e.py`. Set
`RECORD_LANE=record-runtime-e2e.py` on the container command above to exercise the daemon;
also mount the built Linux agent and set `EXTEND_RECORD_DRIVER` to its path to test Extend's
capability probe and recording artifact handoff. Build the device engine before running this lane.
It deliberately crashes only its own isolated daemon, verifies the recovered export, then
stops its daemons using their own state directories.
