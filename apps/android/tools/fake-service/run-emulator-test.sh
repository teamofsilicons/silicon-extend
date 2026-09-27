#!/bin/sh
# End-to-end test of the Android app on a running emulator against fake_extend.py.
#
#   tools/fake-service/run-emulator-test.sh phone      # phone/tablet scenario
#   tools/fake-service/run-emulator-test.sh tv         # TV scenario (on a TV emulator, or FORCE_TV=1 on a phone)
#
# Needs: an emulator/device on adb, the debug APK built (./gradlew :app:assembleDebug).
# Steps: install → launch pointed at the fake service → check the pairing code on screen matches
# the service's → turn on accessibility + notification access + background (adb, like the Carbon
# would in Settings) → claim the code → the scenario runs commands and checks every result.
set -eu
SCENARIO=${1:-phone}
PORT=${PORT:-8490}
HERE=$(cd "$(dirname "$0")" && pwd)
APP_DIR=$(cd "$HERE/../.." && pwd)
ADB=${ADB:-$HOME/Library/Android/sdk/platform-tools/adb}
APK=${APK:-$APP_DIR/app/build/outputs/apk/debug/app-debug.apk}
PKG=com.teamofsilicons.extend
OUT=${OUT:-$HERE/out}
LOG=$OUT/fake-$SCENARIO.log
mkdir -p "$OUT"

state() { curl -s "http://127.0.0.1:$PORT/_test/state"; }
field() { python3 -c "import sys,json; v=json.load(sys.stdin).get('$1'); print('' if v is None else v)"; }

SDK=$("$ADB" shell getprop ro.build.version.sdk | tr -d '\r')
echo "== install $APK (API $SDK)"
# Accessibility off before the app is replaced or stopped: Android 8 and 9 leave a service that was
# bound when its app was updated or force-stopped switched on but never bound again (until reboot).
"$ADB" shell settings put secure enabled_accessibility_services null
sleep 1
"$ADB" install -r -g "$APK" | tail -1
"$ADB" shell am force-stop $PKG
for p in com.android.settings com.google.android.settings.intelligence; do "$ADB" shell am force-stop $p || true; done

echo "== fake service on :$PORT (log: $LOG)"
pkill -f "fake_extend.py --port $PORT" 2>/dev/null || true
sleep 0.5
IMG="$OUT/test-image.png"
[ -f "$IMG" ] || python3 "$HERE/make_test_png.py" "$IMG"
python3 "$HERE/fake_extend.py" --port "$PORT" --scenario "$SCENARIO" --image "$IMG" --out "$OUT/uploads" > "$LOG" 2>&1 &
FAKE=$!
trap 'kill $FAKE 2>/dev/null || true' EXIT
sleep 1

EXTRA=""
[ "${FORCE_TV:-0}" = "1" ] && EXTRA="--ez force_tv true"
[ "${FORCE_TV:-0}" = "0" ] && EXTRA="--ez force_tv false"
echo "== launch the app → http://10.0.2.2:$PORT"
# forget_pair: start unpaired even if an earlier run left a pair behind.
"$ADB" shell am start -n $PKG/.ui.MainActivity --es service_url "http://10.0.2.2:$PORT" --ez forget_pair true $EXTRA >/dev/null

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
  "$ADB" shell uiautomator dump /sdcard/extend-ui.xml >/dev/null 2>&1 || true
  SHOWN=$("$ADB" shell cat /sdcard/extend-ui.xml | grep -o 'text="[0-9A-F]\{3\} [0-9A-F]\{3\}"' | head -1 | sed 's/text="\(.*\)"/\1/' | tr -d ' ')
  [ -n "$SHOWN" ] && break
  # Dismiss a system "isn't responding" dialog if the emulator shows one.
  "$ADB" shell input keyevent KEYCODE_ENTER || true
  sleep 2
done
if [ "$SHOWN" = "$CODE" ]; then echo "PASS pairing screen shows the live code ($SHOWN)"; else echo "FAIL pairing screen shows '$SHOWN', service has '$CODE'"; fi

echo "== grant accessibility, notification access and background use"
"$ADB" shell settings put secure enabled_accessibility_services $PKG/$PKG.a11y.ExtendAccessibilityService
"$ADB" shell settings put secure accessibility_enabled 1
LISTENER=$PKG/$PKG.notif.ExtendNotificationListener
"$ADB" shell cmd notification allow_listener $LISTENER >/dev/null 2>&1 || true
# Android 8.0 has no `cmd notification`; before Android 9 the setting itself is what Android reads.
if ! "$ADB" shell settings get secure enabled_notification_listeners | grep -qF "$LISTENER"; then
  CUR=$("$ADB" shell settings get secure enabled_notification_listeners | tr -d '\r')
  case "$CUR" in ""|null) NEW=$LISTENER ;; *) NEW="$CUR:$LISTENER" ;; esac
  "$ADB" shell settings put secure enabled_notification_listeners "$NEW"
fi
"$ADB" shell dumpsys deviceidle whitelist +$PKG >/dev/null
sleep 4

echo "== claim the code (what a Carbon entering it on the website does)"
curl -s -X POST "http://127.0.0.1:$PORT/_test/claim"; echo
set +e
wait $FAKE
RC=$?
set -e
grep -E "PASS|FAIL|SCENARIO DONE" "$LOG" | cut -c1-220
exit $RC
