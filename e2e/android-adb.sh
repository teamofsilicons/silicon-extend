#!/usr/bin/env bash
# Against a paired Android test device with Android debugging connected to the local dev service.
# Usage: e2e/android-adb.sh <device-id> [api-url]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEV=${1:?Pass the paired Android test device id}
export EXTEND_API_URL=${2:-http://127.0.0.1:8480} EXTEND_TELEMETRY=off
WORK=$(mktemp -d)
export SILICON_HOME="$WORK/chef"
mkdir -p "$SILICON_HOME" "$WORK/alice"
EXTEND="$ROOT/target/debug/extend"
SID=""
cleanup() {
  if [ -n "$SID" ]; then "$EXTEND" session end "$SID" >/dev/null 2>&1 || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT
"$EXTEND" login si:chef >/dev/null
SILICON_HOME="$WORK/alice" "$EXTEND" login c:alice >/dev/null
SID=$("$EXTEND" session new "$DEV" --connect)
"$EXTEND" adb shell id -u | grep -qx 2000
printf 'PASS local Android shell through Extend\n'
"$EXTEND" install com.teamofsilicons.extend.adbfixture "$ROOT/apps/android/app/build/generated/adb-test-assets/fixture.apk" | grep -q Success
"$EXTEND" reinstall com.teamofsilicons.extend.adbfixture "$ROOT/apps/android/app/build/generated/adb-test-assets/fixture.apk" | grep -q Success
"$EXTEND" adb shell pm path com.teamofsilicons.extend.adbfixture | grep -q package:
"$EXTEND" adb uninstall com.teamofsilicons.extend.adbfixture | grep -q Success
printf 'PASS APK install and reinstall through CLI attachments\n'
printf 'binary round trip\000\377\n' >"$WORK/input.bin"
REMOTE="/data/local/tmp/extend-e2e-$SID.bin"
"$EXTEND" adb push "$WORK/input.bin" "$REMOTE" >/dev/null
"$EXTEND" adb pull "$REMOTE" --out "$WORK/output.bin" >/dev/null
cmp "$WORK/input.bin" "$WORK/output.bin"
"$EXTEND" adb shell rm "$REMOTE" >/dev/null
printf 'PASS push/pull binary integrity and artifact upload\n'
"$EXTEND" logs start >/dev/null
"$EXTEND" logs mark extend-cli-completion >/dev/null
"$EXTEND" adb shell sleep 1 >/dev/null
"$EXTEND" logs stop --out "$WORK/device.log" >/dev/null
grep -q extend-cli-completion "$WORK/device.log"
printf 'PASS log collection, marker and file retrieval\n'
"$EXTEND" record start cli-proof >/dev/null
"$EXTEND" adb shell sleep 2 >/dev/null
"$EXTEND" record stop --out "$WORK/proof.mp4" >/dev/null
python3 - "$WORK/proof.mp4" <<'PY'
import sys,pathlib
p=pathlib.Path(sys.argv[1]); assert p.stat().st_size > 1000
assert p.read_bytes()[4:8] == b'ftyp'
PY
printf 'PASS recording through Extend with downloadable MP4\n'
"$EXTEND" logs start >/dev/null
"$EXTEND" record start cancelled >/dev/null
SILICON_HOME="$WORK/alice" "$EXTEND" device stop "$DEV" >/dev/null
set +e
"$EXTEND" adb shell id >"$WORK/stopped" 2>&1
RC=$?
set -e
[ "$RC" -eq 6 ] || { cat "$WORK/stopped"; echo "Expected session-ended exit 6, got $RC"; exit 1; }
SID=$("$EXTEND" session new "$DEV" --connect)
LEFT=$("$EXTEND" adb shell "find /data/local/tmp -maxdepth 1 -name 'silicon-extend-*'")
[ -z "$LEFT" ] || { echo "Capture files remained after Stop: $LEFT"; exit 1; }
printf 'PASS Carbon Stop ends access and cleans capture processes/files\n'
"$EXTEND" session end "$SID" >/dev/null
SID=""
