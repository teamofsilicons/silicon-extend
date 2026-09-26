#!/usr/bin/env bash
# Manual instrumentation lane. Requires freshly installed debug + androidTest APKs.
# RUN_LONG=1 also checks real capture beyond 180 seconds. This lane targets emulators only.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SERIAL="${1:-emulator-5554}"
[[ "$(adb -s "$SERIAL" shell getprop ro.kernel.qemu | tr -d '\r')" == 1 ]] || {
  echo "This lane requires a dedicated Android emulator" >&2; exit 1;
}
mkdir -p "$ROOT/target/android-recording"
OUT="$(mktemp -d "$ROOT/target/android-recording/run-XXXXXXXX")"
trap 'adb -s "$SERIAL" shell am force-stop com.teamofsilicons.bridge.test >/dev/null 2>&1 || true' EXIT
run_case() {
  local method=$1 file=$2
  adb -s "$SERIAL" shell am instrument -w \
    -e class "com.teamofsilicons.bridge.RecordingTest#$method" -e long_recording true \
    com.teamofsilicons.bridge.test/androidx.test.runner.AndroidJUnitRunner >"$OUT/$method.log" 2>&1
  if ! grep -q 'OK (1 test)' "$OUT/$method.log"; then cat "$OUT/$method.log"; exit 1; fi
  adb -s "$SERIAL" pull "/sdcard/Android/data/com.teamofsilicons.bridge/files/$file" "$OUT/$file"
  ffmpeg -v error -xerror -i "$OUT/$file" -enc_time_base demux -fps_mode passthrough -f null -
  python3 - "$OUT/$file" <<'PY'
import json, subprocess, sys
packets=json.loads(subprocess.check_output(['ffprobe','-v','error','-select_streams','v','-show_packets','-of','json',sys.argv[1]]))['packets']
assert packets and all(int(a['dts']) < int(b['dts']) for a,b in zip(packets,packets[1:]))
if 'long-recording' in sys.argv[1]:
    assert float(packets[-1]['pts_time']) > 181, 'No encoded frame after the native cap'
print('PASS full decode and ordered timestamps:', sys.argv[1])
PY
}
run_case nativeDurationLimit duration-recording-proof.mp4
if [[ "${RUN_LONG:-0}" == 1 ]]; then run_case beyondNativeLimit long-recording-proof.mp4; fi
echo "Evidence: $OUT"
