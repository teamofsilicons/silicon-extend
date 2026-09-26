#!/usr/bin/env bash
# Against a paired Android test device with Android debugging connected to the local dev service.
# Usage: e2e/android-adb.sh <device-id> [api-url]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEV=${1:?Pass the paired Android test device id}
export BRIDGE_API_URL=${2:-http://127.0.0.1:8480} BRIDGE_TELEMETRY=off
WORK=$(mktemp -d)
export SILICON_HOME="$WORK/chef"
mkdir -p "$SILICON_HOME" "$WORK/alice"
BRIDGE="$ROOT/target/debug/bridge"
SID=""
cleanup() {
  if [ -n "$SID" ]; then "$BRIDGE" session end "$SID" >/dev/null 2>&1 || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT
"$BRIDGE" login si:chef >/dev/null
SILICON_HOME="$WORK/alice" "$BRIDGE" login c:alice >/dev/null
SID=$("$BRIDGE" session new "$DEV" --connect)
"$BRIDGE" adb shell id -u | grep -qx 2000
printf 'PASS local Android shell through Bridge\n'
"$BRIDGE" install com.teamofsilicons.bridge.adbfixture "$ROOT/apps/android/app/build/generated/adb-test-assets/fixture.apk" | grep -q Success
"$BRIDGE" reinstall com.teamofsilicons.bridge.adbfixture "$ROOT/apps/android/app/build/generated/adb-test-assets/fixture.apk" | grep -q Success
"$BRIDGE" adb shell pm path com.teamofsilicons.bridge.adbfixture | grep -q package:
"$BRIDGE" adb uninstall com.teamofsilicons.bridge.adbfixture | grep -q Success
printf 'PASS APK install and reinstall through CLI attachments\n'
printf 'binary round trip\000\377\n' >"$WORK/input.bin"
REMOTE="/data/local/tmp/bridge-e2e-$SID.bin"
"$BRIDGE" adb push "$WORK/input.bin" "$REMOTE" >/dev/null
"$BRIDGE" adb pull "$REMOTE" --out "$WORK/output.bin" >/dev/null
cmp "$WORK/input.bin" "$WORK/output.bin"
"$BRIDGE" adb shell rm "$REMOTE" >/dev/null
printf 'PASS push/pull binary integrity and artifact upload\n'
"$BRIDGE" logs start >/dev/null
"$BRIDGE" logs mark bridge-cli-completion >/dev/null
"$BRIDGE" adb shell sleep 1 >/dev/null
"$BRIDGE" logs stop --out "$WORK/device.log" >/dev/null
grep -q bridge-cli-completion "$WORK/device.log"
printf 'PASS log collection, marker and file retrieval\n'
"$BRIDGE" record start cli-proof >/dev/null
"$BRIDGE" adb shell sleep 2 >/dev/null
"$BRIDGE" record stop --out "$WORK/proof.mp4" >/dev/null
python3 - "$WORK/proof.mp4" <<'PY'
import sys,pathlib
p=pathlib.Path(sys.argv[1]); assert p.stat().st_size > 1000
assert p.read_bytes()[4:8] == b'ftyp'
PY
printf 'PASS recording through Bridge with downloadable MP4\n'
"$BRIDGE" logs start >/dev/null
"$BRIDGE" record start cancelled >/dev/null
SILICON_HOME="$WORK/alice" "$BRIDGE" device stop "$DEV" >/dev/null
set +e
"$BRIDGE" adb shell id >"$WORK/stopped" 2>&1
RC=$?
set -e
[ "$RC" -eq 6 ] || { cat "$WORK/stopped"; echo "Expected session-ended exit 6, got $RC"; exit 1; }
SID=$("$BRIDGE" session new "$DEV" --connect)
LEFT=$("$BRIDGE" adb shell "find /data/local/tmp -maxdepth 1 -name 'silicon-bridge-*'")
[ -z "$LEFT" ] || { echo "Capture files remained after Stop: $LEFT"; exit 1; }
printf 'PASS Carbon Stop ends access and cleans capture processes/files\n'
"$BRIDGE" session end "$SID" >/dev/null
SID=""
