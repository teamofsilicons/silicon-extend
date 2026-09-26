# Silicon Extend for Mac, Windows and Linux: packaging and end-to-end runs

The app is one Rust binary, [`crates/extend-agent`](../../crates/extend-agent/README.md). This
directory packages it and holds the Linux end-to-end environment. macOS packaging supports
Developer ID signing and notarization; a successful local build alone does not imply publication.

| Path | What it does |
|---|---|
| `macos/build-app.sh`, `macos/Info.plist.in` | Builds `target/desktop/macos/Silicon Extend.app` and a zip |
| `linux/build-package.sh` | Builds the Linux tarball and `.deb` layout (run it on Linux) |
| `linux/build-in-docker.sh` | Runs `build-package.sh` in the linux-e2e image, then installs the `.deb` in a container and runs it |
| `windows/build-zip.ps1` | Builds the Windows zip (run it on Windows) |
| `linux-e2e/` | A real X11 desktop in Docker (`Dockerfile`, `run.sh`, `e2e.sh`) |

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
  Info.plist                    LSUIElement (menu-bar only), usage descriptions, com.teamofsilicons.extend
  MacOS/extend-agent            the app
  MacOS/agent-device-macos-helper   agent-device's helper, built here and signed with the app
  Resources/agent-device/       the fork's bin/, dist/, package.json and the Apple sources it builds
                                on first use (Apple device runners and the native helpers)
  Resources/node/bin/node       Node 22 (official build, downloaded and cached in target/desktop/.cache)
```

- Without `SIGN_IDENTITY`, development builds use an ad-hoc signature. With a Developer ID,
  the app and helper use hardened runtime and secure timestamps; Node receives only the JIT
  entitlements used by the bundled runtime. `NOTARY_PROFILE` requires Developer ID signing
  and completes only after Apple accepts the submission, the ticket is stapled and Gatekeeper
  assessment succeeds. Credentials stay in Keychain.
- Node is pinned to 22.23.3. Both architecture checksums in `macos/node-sha256.txt` come from
  `https://nodejs.org/dist/v22.23.3/SHASUMS256.txt`; cached and offline tarballs are checked too.
  The agent-device fork is rebuilt on every packaging run to include current source changes.
- Mac and Linux packaging stamp the staged runtime version with a SHA-256 build suffix.
  The digest covers packaged code/assets, the Node platform/version and (on Mac) the native
  helper before signing. Changed code therefore triggers the existing daemon takeover path;
  identical copied artifacts retain the same identity despite paths, mtimes or signing timestamps.
  The vendor source manifest and Extend's public version are not changed by packaging.
- `extend-agent` finds the bundled agent-device and Node through `../Resources`. Setting
  `EXTEND_AGENT_DEVICE` or `EXTEND_NODE` overrides them.
- Checked here: the bundle built (176 MB, 52 MB zipped), `plutil -lint` passed, and
  `codesign --verify --deep --strict` passed. `extend-agent probe` and `exec apps` ran from the
  bundle with `PATH=/usr/bin:/bin`, so only the bundled Node was reachable. It found agent-device
  0.21.15 and listed 227 apps.
- Start at login: `extend-agent install-autostart` writes a LaunchAgent that points at the
  bundle's binary.

### Native Mac input and recording (2026-09-26)

The signed GUI app now uses Accessibility for text entry and ScreenCaptureKit for recording,
without an XCTest setup prompt. Both grants are enabled for Silicon Extend. Recording produces
H.264 MP4, accepts 1–60 fps, and supports the selected app or the main display. App capture chooses
the display with the largest overlap with its windows; other apps are excluded. Recordings are
bounded to 30 minutes and 1 GiB, and stop when their owning process exits. Long-duration, file-limit
and owner-exit live stress checks remain pending; the short automatic-duration path is verified.

After pairing the GUI app to the local test service, run:

```sh
python3 apps/desktop/macos/text-e2e.py --record-only --device <local-test-device-id> --artifacts target/desktop/macos/recording-verification
```

This opens an animated fixture, verifies manual and automatic recording stop, then exercises
the service's app selection, capture, upload and CLI download. It fully decodes the resulting
video, checks that frames change and saves a frame for visual inspection. Omit `--record-only`
and use `--record` to include native text-entry checks. These checks require the signed GUI app
to be running; shell-launched helpers can have different Screen Recording permissions.

### Initial Mac run before native input and recording (macOS 27, this machine)

These ran through the agent's own driver (`extend-agent exec …`). The rest ran through the local
Extend service on `:8480`: pair, a Silicon session, then commands with uploads.

- **Worked:**
  - `probe` detected Accessibility ✓ and Screen Recording ✓ (both granted to the terminal
    running the agent).
  - `open TextEdit`; `open --surface frontmost-app`.
  - `snapshot -i`, which gave 46 nodes with `@eN` refs through agent-device's helper.
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
- **Blocked by a macOS prompt nobody could answer:** agent-device's **typing** (`fill`/`type`),
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
2. Runs `cargo test -p extend-agent`: 107 unit tests and 7 integration tests passed on Linux.
3. Checks that a probe with no screen reports `apps.launch`, `replay` and `terminal`.
4. Starts the desktop, then runs the probe and drives GNOME Calculator with the agent's driver:
   - `open gnome-calculator` and `snapshot -i` (64 nodes, keypad buttons with refs)
   - `click @e9` ("7"), then the selectors `role="button" label="+"`, `"5"`, `"="`
   - a snapshot, where the history reads **"7+5 = 12"**
   - `type "3*4"` plus Return, and the history reads **"3×4 = 12"**
   - `screenshot` (PNG), `clipboard write` then `clipboard read` (the round trip)
   - `apps` and `appstate` answer `unsupported_on_device` (agent-device has none on Linux)
   - `terminal run` (stdout, stderr, exit 0), then an exit 5 that comes back as `command_failed`
   - a reserved flag refused, and `close`
5. With `EXTEND_E2E_SERVICE`, runs the real agent headless against that service:
   - a Carbon pairs it, the device is online and `ready`, and a Silicon session starts
   - `open`, `snapshot`, `click`, and a `screenshot` uploaded with a URL back
   - `terminal`; the headless status line shows "si:chef is using this computer"
   - `extend-agent stop` ends the session with `stopped_by_carbon`
   - the Carbon removes the device, and the app forgets the credential and shows a new code

Two fork fixes came out of this run, both logged in `vendor/agent-device/FORK.md`:
- The AT-SPI depth limit (GTK4 buttons were invisible).
- xclip clipboard writes, which always timed out.

GTK4 on X11 tells AT-SPI its window sits at (0,0), so agent-device's click coordinates are right
only for a window at the top-left. `e2e.sh` moves the calculator there with `wmctrl`. That is an
agent-device/GTK4 limitation, noted for later.

Packaging: `linux/build-in-docker.sh` builds the tarball and `.deb` into `target/desktop/linux/`,
installs the `.deb` into the container, and runs `extend-agent probe` and a terminal command from
`/usr/bin`. It rebuilds the bundled agent-device fork before packaging. Node is pinned to
22.23.3; both Linux architecture checksums in `linux/node-sha256.txt` come from
`https://nodejs.org/dist/v22.23.3/SHASUMS256.txt`. Cached and offline archives are verified.
The `.deb` derives native dependencies and minimum versions from the agent and bundled Node
using `dpkg-shlibdeps` (dpkg-dev). Build on the oldest distro you intend to support; a package
built on Debian trixie is not evidence of compatibility with older distributions. Layout:

```
bin/extend-agent
lib/silicon-extend/agent-device/{bin,dist,linux,package.json}
lib/silicon-extend/node/bin/node
share/applications/silicon-extend.desktop
```

- Runtime dependencies include GTK 3, WebKitGTK 4.1, libxdo and their native dependencies,
  plus `python3-gi`, `gir1.2-atspi-2.0` and `at-spi2-core` for screen reading. The generated
  `.deb` metadata carries the exact native requirements of that build.
- Recommended: xdotool, xclip, ImageMagick, xdg-utils, ffmpeg, x11-utils and libxcomposite1.

To check an installed package against a local development service with local IAM members
`c:alice` and `si:chef`, run from the host (requires the built CLI and ffmpeg):

```sh
python3 apps/desktop/linux-e2e/record-service-e2e.py --package target/desktop/linux/silicon-extend_1.0.0_arm64.deb
```

The lane installs the `.deb` into a disposable container and runs it as an unprivileged user
with no source runtime mounted. It pairs its own device, opens an animated fixture, records
app and device scopes, uploads through the service, downloads through the CLI, fully decodes
both videos and compares repeat downloads byte for byte. It ends its session, removes its
device and stops its container. Artifacts stay under `target/desktop/linux-recording/`.
Use `--service-url` and `--container-service-url` when the host/container addresses differ.
This verifies local relay and file storage, not production IAM or Briefcase integration.

## Windows

`windows/build-zip.ps1` (run it on Windows) builds `extend-agent.exe` and zips it with a README.
There's no Node and no agent-device: Windows uses Extend's own driver. The window needs WebView2,
which Windows 10 and 11 ship.

**None of the Windows side has run on Windows.** From this Mac it is checked with
`cargo check` and `cargo clippy -- -D warnings` against `--target x86_64-pc-windows-msvc`, and
the driver's pure logic is unit-tested. The script itself hasn't run.

### Packaged daemon update verification

```sh
node --test apps/desktop/stamp-runtime.test.mjs
node apps/desktop/runtime-update-e2e.mjs <built-agent-device-package>
```

The second command creates isolated copies and an empty session store. It first reproduces stale
reuse without a build stamp, then verifies that the new artifact executes in a new daemon and that
an identical relocated copy reuses it. It stops its owned daemon afterward and never opens a device.
The changed-version path uses the runtime's existing shutdown/cleanup behavior; this check does
not exercise updating during an active device session.


### X11 recording worker development (2026-09-26)

`vendor/agent-device/linux/screen-record.py` is the native recording worker under development.
It accepts a root screen, explicit X11 window ID or exact application class, writes H.264 MP4, publishes first-frame
readiness, and enforces duration/file limits. SIGINT/TERM/HUP and owner exit finalize the video;
Linux parent-death signaling also stops the encoder if its supervisor is killed.

Run its manual Linux lane with a rebuilt `linux-e2e` image containing ffmpeg:

```sh
docker build -t silicon-extend-linux-e2e apps/desktop/linux-e2e
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
fps, quality, hide-touches and daemon-crash recovery. Extend returns a copied recording artifact
and retains its export for retry. App scope binds the active named app to exactly one mapped
WM_CLASS matching its executable or desktop-file basename. It requires xdotool and libXcomposite.
XComposite captures the window's off-screen pixels even when another app covers it. Multiple
matching windows are refused; resizing or unmapping the window ends recording without switching
to desktop pixels. Minimized-window starts and multiwindow apps remain unsupported.

Set `RECORD_LANE=record-app-e2e.py` to exercise public named-app open/start/stop with an already
covered target and missing/ambiguous-target refusal. `RECORD_LANE=record-isolation-e2e.py` tests
native isolation; add `RECORD_ISOLATION_EDGE=unmap` or `resize` to verify finalization on source
changes. These are manual Xvfb lanes, not CI or physical-desktop evidence. Real service transfer,
other desktop/app coverage and Wayland portal/PipeWire support remain open. Wayland deliberately
refuses this worker, including XWayland displays.

The public daemon and Extend driver lane is `record-runtime-e2e.py`. Set
`RECORD_LANE=record-runtime-e2e.py` on the container command above to exercise the daemon;
also mount the built Linux agent and set `EXTEND_RECORD_DRIVER` to its path to test Extend's
capability probe and recording artifact handoff. Build agent-device before running this lane.
It deliberately crashes only its own isolated daemon, verifies the recovered export, then
stops its daemons using their own state directories.
