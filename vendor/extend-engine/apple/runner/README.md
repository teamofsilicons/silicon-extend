# Silicon Extend helper (XCUITest runner)

This folder contains the lightweight XCUITest runner the device engine uses for element-level
automation on Apple-family targets: an iPhone or iPad a Mac hosts, a Simulator, tvOS, and the Mac
itself.

## Intent

- Provide a minimal XCTest target that exposes UI automation over a small HTTP server.
- Allow local builds via `xcodebuild` and caching for faster subsequent runs.
- Support simulator prebuilds where compatible.

## Names

Everything a Carbon can see on a device says Silicon Extend.

| What | Name |
| --- | --- |
| Folder, Xcode project, scheme | `SiliconExtendHelper`, `SiliconExtendHelper.xcodeproj`, scheme `SiliconExtendHelper` |
| App target and product | `SiliconExtendHelper`, `SiliconExtendHelper.app`; the name under its icon is "Silicon Extend" (`CFBundleDisplayName`) |
| UI-test target and test plan | `SiliconExtendHelperUITests`, `SiliconExtendHelperUITests.xctestplan`; `-only-testing` names the target: `SiliconExtendHelperUITests/RunnerTests/testCommand` |
| UI-test product | `SiliconExtend.xctest`, which XCTest wraps in `SiliconExtend-Runner.app` |
| Default bundle ids | `com.teamofsilicons.extend.helper` (app), `com.teamofsilicons.extend.helper.uitests` (tests); XCTest gives the runner app `com.teamofsilicons.extend.helper.uitests.xctrunner` |
| Build settings that override them | `EXTEND_ENGINE_IOS_RUNNER_APP_BUNDLE_ID`, `EXTEND_ENGINE_IOS_RUNNER_TEST_BUNDLE_ID` |

The UI-test product is `SiliconExtend`, not the target name, because of how iOS names the runner.
Xcode builds the runner app from its own `XCTRunner.app` template: the app is always
`<PRODUCT_NAME>-Runner.app`, and its `CFBundleName` (the name under the icon) is always
`<PRODUCT_NAME>-Runner`, with no display name a project can set. Signing a device runner freezes
that name, so nothing may patch it afterwards. `SiliconExtend-Runner` is the closest to "Silicon
Extend" iOS allows; with the target name it would read `SiliconExtendHelperUITests-Runner`.

Earlier releases installed the helper as `com.callstack.agentdevice.runner` (runner
`com.callstack.agentdevice.runner.uitests.xctrunner`). Extend removes that old helper from a device
once the new one is installed.

## Status

Current internal runner for iOS, tvOS, and macOS desktop automation.

Protocol and maintenance references:

- Protocol overview: [`RUNNER_PROTOCOL.md`](RUNNER_PROTOCOL.md)
- TypeScript client: [`../../packages/platform-apple/src/runner/runner-client.ts`](../../packages/platform-apple/src/runner/runner-client.ts)
- Swift wire models: [`SiliconExtendHelper/SiliconExtendHelperUITests/RunnerTests+Models.swift`](SiliconExtendHelper/SiliconExtendHelperUITests/RunnerTests+Models.swift)

## UITest Runner File Map

`SiliconExtendHelperUITests/RunnerTests` is split into focused files to keep each one small.

- `RunnerTests.swift`: shared state/constants, `setUp()`, and `testCommand()` entry flow.
- `RunnerTests+Models.swift`: wire protocol models (`Command`, `Response`, snapshot payload models).
- `RunnerTests+Environment.swift`: environment and CLI argument helpers (`RunnerEnv`).
- `RunnerTests+Transport.swift`: TCP request handling and HTTP parsing/encoding.
- `RunnerTests+CommandDispatch.swift`: the dispatch entry (`executeAccepted`, `executeDispatched`),
  its recovery loops, target preparation, and recorded-failure conversion.
- `RunnerTests+CommandExecution.swift`: the prepared-command switch (`executeOnMainPrepared`).
- `RunnerTests+GestureExecution.swift`, `RunnerTests+ScrollDragExecution.swift`,
  `RunnerTests+TypeExecution.swift`, `RunnerTests+SnapshotExecution.swift`: per-family command
  execution.
- `RunnerTests+Lifecycle.swift`: activation/retry/stabilization and recording lifecycle helpers.
- `RunnerTests+Interaction.swift`: tap/drag/swipe/type/home/rotate/app-switcher helpers.
- `RunnerTests+Navigation.swift`: back/navigation-control helpers.
- `RunnerTests+Snapshot.swift`: fast/raw snapshot builders and include/filter helpers.
- `RunnerTests+SystemModal.swift`: SpringBoard/system modal detection and modal snapshot shaping.
- `RunnerTests+ScreenRecorder.swift`: nested `ScreenRecorder` implementation.
- `UnitTests/RunnerTests+<Source>Tests.swift`: the `AGENT_DEVICE_RUNNER_UNIT_TESTS` tests for each
  source file. The packaged runner source omits this directory.

Names nobody sees keep the upstream spelling so an upstream sync stays a merge: the
`AGENT_DEVICE_RUNNER_*` log markers the engine reads from the runner log, the
`AGENT_DEVICE_RUNNER_UNIT_TESTS` compilation condition, the `AGENT_DEVICE_*` environment the
engine passes in, the `--agent-device-*` launch arguments and `agent-device-*` accessibility
identifiers of the test fixtures in the helper app, and the `AgentDeviceSnapshotPresentation`
module.

## Snapshot Strategy

iOS snapshots have two explicit public capture modes:

- full/raw snapshots use recursive XCTest snapshots for rich hierarchy and diagnostics;
- interactive snapshots filter the same visible tree down to the refs a Silicon acts on.

Some iOS apps expose accessibility trees that lower-level AX services can inspect but XCTest cannot
serialize reliably. In those cases interactive snapshots may return a sparse root quickly, while
full snapshots preserve the XCTest error. A penalized simulator can recover through private AX;
physical devices use a short XCTest probe because no non-XCTest semantic backend is available
there. See
[`../../docs/adr/0004-ios-snapshot-backend-strategy.md`](../../docs/adr/0004-ios-snapshot-backend-strategy.md)
for the backend boundary and future simulator AX-service direction.

## Protocol Notes

- The daemon posts JSON commands to `POST /command` on the runner's local HTTP listener.
- The runner responds with a JSON envelope shaped as `{ ok, data?, error? }`.
- The protocol is internal to the device engine; when adding or renaming commands, update both wire models and the protocol tests/docs in the same change.
