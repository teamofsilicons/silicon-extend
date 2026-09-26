#!/usr/bin/env bash
# Creates a disposable test-only APK. No runtime permissions or executable code.
set -euo pipefail
sdk=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
rm -f "$out/test.keystore" # remove the previous generated fixture key from the assets directory
buildtools="$sdk/build-tools/36.0.0"
"$buildtools/aapt2" link --manifest "$here/AndroidManifest.xml" -I "$sdk/platforms/android-36/android.jar" -o "$out/unsigned.apk"
"$buildtools/zipalign" -f 4 "$out/unsigned.apk" "$out/aligned.apk"
if [ ! -f "$out/../adb-test.keystore" ]; then
  "$JAVA_HOME/bin/keytool" -genkeypair -keystore "$out/../adb-test.keystore" -storepass android -keypass android -alias test -keyalg RSA -keysize 2048 -validity 3650 -dname CN=BridgeTest -noprompt >/dev/null 2>&1
fi
"$buildtools/apksigner" sign --ks "$out/../adb-test.keystore" --ks-pass pass:android --key-pass pass:android --out "$out/fixture.apk" "$out/aligned.apk"
rm "$out/unsigned.apk" "$out/aligned.apk"
