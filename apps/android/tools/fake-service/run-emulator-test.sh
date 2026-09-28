#!/bin/sh
# End-to-end test of the Android app on an emulator against fake_extend.py.
#
#   SERIAL=emulator-5580 tools/fake-service/run-emulator-test.sh phone   # an emulator you started
#   AVD=extend_api28 EMU_PORT=5580 tools/fake-service/run-emulator-test.sh multi
#                                  # starts that AVD on port 5580, and stops it (only it) at the end
#
# Scenarios: phone, tv (on a TV emulator, or FORCE_TV=1 on a phone), smoke, and multi (several
# Carbons on one device: Pair with another Carbon, wake requests, sides, Stop, revoke per Carbon).
#
# Every adb call names the device (-s): the script never picks one by itself, and it never stops a
# process it didn't start. With AVD it stops the emulator it started, by its serial; the fake
# service it started, by its process id. It changes settings on the device (accessibility,
# notification access, the screen timeout), so it runs on emulators only, unless ALLOW_PHYSICAL=1.
#
# Needs: the debug APK built (./gradlew :app:assembleDebug).
# Steps: install → launch pointed at the fake service → check the pairing code on screen matches
# the service's → turn on accessibility + notification access + background (adb, like the Carbon
# would in Settings) → claim the code → the scenario runs commands and checks every result.
set -eu
SCENARIO=${1:-phone}
PORT=${PORT:-8490}
HERE=$(cd "$(dirname "$0")" && pwd)
APP_DIR=$(cd "$HERE/../.." && pwd)
SDK=${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}
ADB_BIN=${ADB:-$SDK/platform-tools/adb}
APK=${APK:-$APP_DIR/app/build/outputs/apk/debug/app-debug.apk}
PKG=com.teamofsilicons.extend
OUT=${OUT:-$HERE/out}
LOG=$OUT/fake-$SCENARIO.log
mkdir -p "$OUT"

STARTED_EMULATOR=""
FAKE=""
cleanup() {
  [ -n "$FAKE" ] && kill "$FAKE" 2>/dev/null || true
  if [ -n "$STARTED_EMULATOR" ]; then
    echo "== stopping the emulator this script started ($STARTED_EMULATOR)"
    "$ADB_BIN" -s "$STARTED_EMULATOR" emu kill >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

if [ -n "${AVD:-}" ]; then
  EMU_PORT=${EMU_PORT:-5580}
  case "$EMU_PORT" in *[!0-9]*) echo "EMU_PORT must be a number"; exit 2 ;; esac
  SERIAL=emulator-$EMU_PORT
  if "$ADB_BIN" devices | grep -q "^$SERIAL[[:space:]]"; then
    echo "FAIL: $SERIAL is already running; pick another EMU_PORT (this script stops only an emulator it started)"
    exit 2
  fi
  echo "== starting $AVD on port $EMU_PORT"
  "$SDK/emulator/emulator" -avd "$AVD" -port "$EMU_PORT" -no-window -no-audio -no-boot-anim -no-snapshot-save \
    > "$OUT/emulator-$EMU_PORT.log" 2>&1 &
  STARTED_EMULATOR=$SERIAL
  BOOTED=""
  for _ in $(seq 1 180); do
    if [ "$("$ADB_BIN" -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; then BOOTED=1; break; fi
    sleep 2
  done
  [ -n "$BOOTED" ] || { echo "FAIL: $AVD didn't boot within 6 minutes (log: $OUT/emulator-$EMU_PORT.log)"; exit 1; }
fi

: "${SERIAL:?set SERIAL (for example emulator-5580) or AVD: the script never picks a device itself}"
case "$SERIAL" in
  emulator-*) ;;
  *) [ "${ALLOW_PHYSICAL:-0}" = "1" ] || { echo "FAIL: $SERIAL isn't an emulator; set ALLOW_PHYSICAL=1 to change settings on a real device"; exit 2; } ;;
esac
export ANDROID_SERIAL=$SERIAL
adb() { "$ADB_BIN" -s "$SERIAL" "$@"; }

state() { curl -s "http://127.0.0.1:$PORT/_test/state"; }
field() { python3 -c "import sys,json; v=json.load(sys.stdin).get('$1'); print('' if v is None else v)"; }

SDK_INT=$(adb shell getprop ro.build.version.sdk | tr -d '\r')
echo "== install $APK on $SERIAL (API $SDK_INT)"
# Accessibility off before the app is replaced or stopped: Android 8 and 9 leave a service that was
# bound when its app was updated or force-stopped switched on but never bound again (until reboot).
adb shell settings put secure enabled_accessibility_services null
sleep 1
adb install -r -g "$APK" | tail -1
adb shell am force-stop $PKG
for p in com.android.settings com.google.android.settings.intelligence; do adb shell am force-stop $p || true; done
# The multi scenario needs an awake device: screen on, unlocked, and a long screen timeout.
adb shell settings put system screen_off_timeout 1800000 || true
adb shell input keyevent KEYCODE_WAKEUP || true
adb shell wm dismiss-keyguard || true

if curl -s -o /dev/null "http://127.0.0.1:$PORT/_test/state"; then
  echo "FAIL: something already answers on port $PORT; set PORT to a free one"
  exit 2
fi
echo "== fake service on :$PORT (log: $LOG)"
IMG="$OUT/test-image.png"
[ -f "$IMG" ] || python3 "$HERE/make_test_png.py" "$IMG"
python3 "$HERE/fake_extend.py" --port "$PORT" --scenario "$SCENARIO" --image "$IMG" --out "$OUT/uploads" > "$LOG" 2>&1 &
FAKE=$!
sleep 1

EXTRA=""
[ "${FORCE_TV:-0}" = "1" ] && EXTRA="--ez force_tv true"
[ "${FORCE_TV:-0}" = "0" ] && EXTRA="--ez force_tv false"
echo "== launch the app → http://10.0.2.2:$PORT"
# forget_pair: start unpaired even if an earlier run left pairs behind.
adb shell am start -n $PKG/.ui.MainActivity --es service_url "http://10.0.2.2:$PORT" --ez forget_pair true $EXTRA >/dev/null

CODE=""
for _ in $(seq 1 60); do
  CODE=$(state | field pairing_code)
  [ -n "$CODE" ] && break
  sleep 1
done
[ -n "$CODE" ] || { echo "FAIL: the app never asked for a pairing code"; exit 1; }
echo "== service issued pairing code $CODE"
sleep 3
CODE=$(state | field pairing_code)
SHOWN=""
for _ in $(seq 1 10); do
  adb shell uiautomator dump /sdcard/extend-ui.xml >/dev/null 2>&1 || true
  SHOWN=$(adb shell cat /sdcard/extend-ui.xml | grep -o 'text="[0-9A-F]\{3\} [0-9A-F]\{3\}"' | head -1 | sed 's/text="\(.*\)"/\1/' | tr -d ' ')
  [ -n "$SHOWN" ] && break
  # Dismiss a system "isn't responding" dialog if the emulator shows one.
  adb shell input keyevent KEYCODE_ENTER || true
  sleep 2
done
if [ "$SHOWN" = "$CODE" ]; then echo "PASS pairing screen shows the live code ($SHOWN)"; else echo "FAIL pairing screen shows '$SHOWN', service has '$CODE'"; fi

echo "== grant accessibility, notification access and background use"
adb shell settings put secure enabled_accessibility_services $PKG/$PKG.a11y.ExtendAccessibilityService
adb shell settings put secure accessibility_enabled 1
LISTENER=$PKG/$PKG.notif.ExtendNotificationListener
adb shell cmd notification allow_listener $LISTENER >/dev/null 2>&1 || true
# Android 8.0 has no `cmd notification`; before Android 9 the setting itself is what Android reads.
if ! adb shell settings get secure enabled_notification_listeners | grep -qF "$LISTENER"; then
  CUR=$(adb shell settings get secure enabled_notification_listeners | tr -d '\r')
  case "$CUR" in ""|null) NEW=$LISTENER ;; *) NEW="$CUR:$LISTENER" ;; esac
  adb shell settings put secure enabled_notification_listeners "$NEW"
fi
adb shell dumpsys deviceidle whitelist +$PKG >/dev/null
sleep 4

echo "== claim the code (what a Carbon entering it on the website does)"
curl -s -X POST "http://127.0.0.1:$PORT/_test/claim"; echo
set +e
wait $FAKE
RC=$?
FAKE=""
set -e
grep -E "PASS|FAIL|SCENARIO DONE" "$LOG" | cut -c1-220
exit $RC
