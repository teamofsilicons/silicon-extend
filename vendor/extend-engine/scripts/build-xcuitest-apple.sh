#!/bin/sh
set -eu

# Silicon Extend names every engine setting EXTEND_ENGINE_<X>; the fork's AGENT_DEVICE_<X> still
# works, and the new name wins when both are set.
setting() {
  eval "printf '%s' \"\${EXTEND_ENGINE_$1:-\${AGENT_DEVICE_$1:-}}\""
}

PLATFORM="$(setting XCUITEST_PLATFORM)"
PROJECT_PATH="apple/runner/SiliconExtendHelper/SiliconExtendHelper.xcodeproj"
SCHEME="SiliconExtendHelper"
DEFAULT_IOS_RUNNER_APP_BUNDLE_ID="com.teamofsilicons.extend.helper"
ENGINE_HOME="$HOME/.silicon-extend/engine"

if [ -z "$PLATFORM" ]; then
  echo "EXTEND_ENGINE_XCUITEST_PLATFORM is required (ios, macos, tvos, visionos)" >&2
  exit 1
fi

is_truthy() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

resolve_default_destination() {
  case "$PLATFORM" in
    ios)
      resolve_simulator_destination 'iOS' 'iPhone' || printf '%s\n' 'generic/platform=iOS Simulator'
      ;;
    macos)
      printf 'platform=macOS,arch=%s\n' "$(uname -m)"
      ;;
    tvos)
      resolve_simulator_destination 'tvOS' 'Apple TV' || printf '%s\n' 'generic/platform=tvOS Simulator'
      ;;
    visionos)
      resolve_simulator_destination 'visionOS' 'Apple Vision' || printf '%s\n' 'generic/platform=visionOS Simulator'
      ;;
    *)
      echo "Unsupported EXTEND_ENGINE_XCUITEST_PLATFORM: $PLATFORM" >&2
      exit 1
      ;;
  esac
}

resolve_simulator_destination() {
  command -v node >/dev/null 2>&1 || return 1
  node -e '
const { execFileSync } = require("node:child_process");
const platformName = process.argv[1];
const deviceNamePattern = new RegExp(process.argv[2]);
const platformNameLower = platformName.toLowerCase();
try {
  const output = execFileSync("xcrun", ["simctl", "list", "devices", "available", "-j"], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
    timeout: 3000,
  });
  const parsed = JSON.parse(output);
  const devices = Object.entries(parsed.devices ?? {})
    .filter(([runtime]) => runtime.toLowerCase().includes(platformNameLower))
    .flatMap(([, runtimeDevices]) => Array.isArray(runtimeDevices) ? runtimeDevices : [])
    .filter(
      (device) =>
        device &&
        device.isAvailable !== false &&
        typeof device.udid === "string" &&
        typeof device.name === "string" &&
        deviceNamePattern.test(device.name),
    );
  const selected = devices.find((device) => device.state === "Booted") ?? devices[0];
  if (!selected) process.exit(1);
  console.log(`platform=${platformName} Simulator,id=${selected.udid}`);
} catch {
  process.exit(1);
}
' "$1" "$2"
}

resolve_default_derived_path() {
  case "$PLATFORM" in
    ios)
      printf '%s\n' "$ENGINE_HOME/apple-runner/derived"
      ;;
    macos)
      printf '%s\n' "$ENGINE_HOME/apple-runner/derived/macos"
      ;;
    tvos)
      printf '%s\n' "$ENGINE_HOME/apple-runner/derived/tvos"
      ;;
    visionos)
      printf '%s\n' "$ENGINE_HOME/apple-runner/derived/visionos"
      ;;
    *)
      echo "Unsupported EXTEND_ENGINE_XCUITEST_PLATFORM: $PLATFORM" >&2
      exit 1
      ;;
  esac
}

resolve_clean_path() {
  if [ -n "$(setting IOS_RUNNER_DERIVED_PATH)" ]; then
    printf '%s\n' "$DERIVED_PATH"
    return
  fi

  case "$PLATFORM" in
    ios)
      printf '%s\n' "$DERIVED_PATH/device"
      ;;
    macos|tvos|visionos)
      printf '%s\n' "$DERIVED_PATH"
      ;;
    *)
      echo "Unsupported EXTEND_ENGINE_XCUITEST_PLATFORM: $PLATFORM" >&2
      exit 1
      ;;
  esac
}

DESTINATION="$(setting XCUITEST_DESTINATION)"
DESTINATION="${DESTINATION:-$(resolve_default_destination)}"
DERIVED_PATH="$(setting IOS_RUNNER_DERIVED_PATH)"
DERIVED_PATH="${DERIVED_PATH:-$(resolve_default_derived_path)}"
CLEAN_PATH="$(resolve_clean_path)"
RUNNER_APP_BUNDLE_ID="$(setting IOS_BUNDLE_ID)"
RUNNER_APP_BUNDLE_ID="${RUNNER_APP_BUNDLE_ID:-$(setting IOS_RUNNER_APP_BUNDLE_ID)}"
RUNNER_APP_BUNDLE_ID="${RUNNER_APP_BUNDLE_ID:-$DEFAULT_IOS_RUNNER_APP_BUNDLE_ID}"
RUNNER_TEST_BUNDLE_ID="$(setting IOS_RUNNER_TEST_BUNDLE_ID)"
RUNNER_TEST_BUNDLE_ID="${RUNNER_TEST_BUNDLE_ID:-$RUNNER_APP_BUNDLE_ID.uitests}"
SIGNING_BUILD_SETTINGS=""

if [ "$PLATFORM" = "macos" ]; then
  SIGNING_BUILD_SETTINGS="CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY= DEVELOPMENT_TEAM="
fi

if is_truthy "$(setting IOS_CLEAN_DERIVED)"; then
  rm -rf "$CLEAN_PATH"
fi

SWIFT_FLAGS='$(inherited) -disable-sandbox -D AGENT_DEVICE_RUNNER_ISOLATION_CANARY'
if is_truthy "$(setting XCUITEST_INCLUDE_UNIT_TESTS)"; then
  SWIFT_FLAGS="$SWIFT_FLAGS -D AGENT_DEVICE_RUNNER_UNIT_TESTS"
fi

# Optional arch override. A generic simulator destination leaves the active arch
# undefined; Xcode versions differ on the default (26.6 picks x86_64, which runs
# under Rosetta on arm64 hosts). Set EXTEND_ENGINE_XCUITEST_ARCHS=arm64 to pin it.
ARCH_BUILD_SETTINGS=""
XCUITEST_ARCHS="$(setting XCUITEST_ARCHS)"
if [ -n "$XCUITEST_ARCHS" ]; then
  ARCH_BUILD_SETTINGS="ARCHS=$XCUITEST_ARCHS"
fi

build_for_testing() {
  node --experimental-strip-types scripts/swift-toolchain-tmpdir.ts xcodebuild build-for-testing \
    -project "$PROJECT_PATH" \
    -scheme "$SCHEME" \
    -destination "$DESTINATION" \
    -derivedDataPath "$DERIVED_PATH" \
    EXTEND_ENGINE_IOS_RUNNER_APP_BUNDLE_ID="$RUNNER_APP_BUNDLE_ID" \
    EXTEND_ENGINE_IOS_RUNNER_TEST_BUNDLE_ID="$RUNNER_TEST_BUNDLE_ID" \
    COMPILER_INDEX_STORE_ENABLE=NO \
    ENABLE_CODE_COVERAGE=NO \
    ONLY_ACTIVE_ARCH=YES \
    ENABLE_PREVIEWS=NO \
    ENABLE_DEBUG_DYLIB=NO \
    -IDEPackageSupportDisableManifestSandbox=1 \
    -IDEPackageSupportDisablePluginExecutionSandbox=1 \
    ENABLE_USER_SCRIPT_SANDBOXING=NO \
    OTHER_SWIFT_FLAGS="$SWIFT_FLAGS" \
    $ARCH_BUILD_SETTINGS \
    $SIGNING_BUILD_SETTINGS
}

# The isolation scan reads the compiler diagnostics in the build log, and an incremental build
# prints them only for the files it recompiles. The scan's positive control must print on every
# build, so its source is always stale.
REUSED_DERIVED_DATA=0
if [ -d "$DERIVED_PATH/Build/Intermediates.noindex" ]; then
  REUSED_DERIVED_DATA=1
fi
touch apple/runner/SiliconExtendHelper/SiliconExtendHelperUITests/RunnerIsolationCanary.swift
mkdir -p "$DERIVED_PATH/Logs"
BUILD_LOG="$DERIVED_PATH/Logs/extend-engine-build-for-testing.log"
BUILD_STATUS_FILE="$DERIVED_PATH/Logs/extend-engine-build-for-testing.status"
{
  BUILD_STATUS=0
  build_for_testing 2>&1 || BUILD_STATUS=$?
  printf '%s\n' "$BUILD_STATUS" > "$BUILD_STATUS_FILE"
} | tee "$BUILD_LOG"
BUILD_STATUS="$(cat "$BUILD_STATUS_FILE")"
if [ "$BUILD_STATUS" -ne 0 ]; then
  exit "$BUILD_STATUS"
fi
if [ "$REUSED_DERIVED_DATA" = 1 ]; then
  echo "Isolation scan covers only the files this build recompiled: it reused DerivedData at $DERIVED_PATH. Run pnpm build:xcuitest:$PLATFORM:clean to scan every file." >&2
fi
if ! node --experimental-strip-types scripts/runner-isolation-diagnostics.ts "$BUILD_LOG"; then
  # Unchanged files print no diagnostics on the next incremental build, so a rerun would pass.
  rm -rf "$DERIVED_PATH/Build/Intermediates.noindex"
  exit 1
fi

if ! is_truthy "$(setting XCUITEST_SKIP_ICON_PATCH)"; then
  node --experimental-strip-types scripts/patch-xcuitest-runner-icon.ts "$DERIVED_PATH"
fi
node scripts/write-xcuitest-cache-metadata.mjs "$PLATFORM" "$DERIVED_PATH" "$DESTINATION"
