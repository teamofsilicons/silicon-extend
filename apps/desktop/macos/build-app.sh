#!/usr/bin/env bash
# Builds "Silicon Extend.app": the extend-agent binary, Extend's agent-device fork, a Node runtime
# and agent-device's macOS helper. Set SIGN_IDENTITY for Developer ID signing.
#
#   apps/desktop/macos/build-app.sh            # release build for this Mac's architecture
#   PROFILE=debug apps/desktop/macos/build-app.sh
#   SIGN_IDENTITY='Developer ID Application: …' apps/desktop/macos/build-app.sh
#   NOTARY_PROFILE=extend-release SIGN_IDENTITY='…' apps/desktop/macos/build-app.sh
#   NODE_TARBALL=/path/node-v22.23.3-darwin-arm64.tar.gz … (offline, checksum still verified)
#
# Output: target/desktop/macos/Silicon Extend.app and one zip whose name says how it was signed:
#   Silicon-Extend-<version>-macos-<arch>.zip              Developer ID signed, notarized, stapled
#   Silicon-Extend-<version>-macos-<arch>-unnotarized.zip  Developer ID signed, no NOTARY_PROFILE
#   Silicon-Extend-<version>-macos-<arch>-adhoc.zip        ad-hoc signed, no SIGN_IDENTITY (this Mac only)
# A failed notarization leaves no zip behind.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${PROFILE:-release}"
NODE_VERSION="22.23.3"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
ARCH="$(uname -m)"; [[ "$ARCH" == "x86_64" ]] && NODE_ARCH=x64 || NODE_ARCH=arm64
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="$ROOT/target/desktop/macos"
APP="$OUT/Silicon Extend.app"
CACHE="$ROOT/target/desktop/.cache"
AD="$ROOT/vendor/agent-device"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

step() { printf '\n== %s\n' "$*"; }
die() { printf '%s\n' "$*" >&2; exit 1; }
# shellcheck source=../packaging.sh
source "$ROOT/apps/desktop/packaging.sh"

if [[ -n "${NOTARY_PROFILE:-}" && "$SIGN_IDENTITY" == "-" ]]; then
  die "NOTARY_PROFILE is set but SIGN_IDENTITY isn't: Apple notarizes only Developer ID signed apps. Set SIGN_IDENTITY='Developer ID Application: …', or unset NOTARY_PROFILE for an ad-hoc build."
fi

step "agent-device fork (pnpm install && pnpm build)"
(cd "$AD" && pnpm install --frozen-lockfile && pnpm build)
# What this dist was built from, so a later package without pnpm can check it (dist-manifest.mjs).
node "$ROOT/apps/desktop/dist-manifest.mjs" record "$AD"
# The macOS helper, built once and shipped signed inside the app so permissions stick to it.
(cd "$AD" && pnpm build:macos-helper) >/dev/null
HELPER="$(find "$AD/apple/macos-helper/.build" -type f -name agent-device-macos-helper -perm -u+x -ipath '*release*' | head -1)"
[[ -n "$HELPER" ]] || die "agent-device-macos-helper didn't build: no release executable under $AD/apple/macos-helper/.build. Run (cd vendor/agent-device && pnpm build:macos-helper) to see the Swift error, fix it and build again."

step "extend-agent ($PROFILE)"
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release -p extend-agent --manifest-path "$ROOT/Cargo.toml"
else
  cargo build -p extend-agent --manifest-path "$ROOT/Cargo.toml"
fi
BIN="$TARGET_DIR/$PROFILE/extend-agent"

step "Node $NODE_VERSION for darwin-$NODE_ARCH"
mkdir -p "$CACHE"
NAME="node-v$NODE_VERSION-darwin-$NODE_ARCH.tar.gz"
NODE_URL="https://nodejs.org/dist/v$NODE_VERSION/$NAME"
SUMS="$ROOT/apps/desktop/macos/node-sha256.txt"
EXPECTED="$(awk -v name="$NAME" '$2 == name {print $1}' "$SUMS")"
[[ -n "$EXPECTED" ]] || die "No pinned SHA-256 for $NAME in $SUMS, so the Node download can't be verified. Add its line from https://nodejs.org/dist/v$NODE_VERSION/SHASUMS256.txt and build again."
sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
# Verify before caching: a download that fails the check is never kept, and a cached file that
# fails it (damaged or altered earlier) is replaced once rather than failing every build.
if [[ -n "${NODE_TARBALL:-}" ]]; then
  ACTUAL="$(sha256_of "$NODE_TARBALL")"
  [[ "$ACTUAL" == "$EXPECTED" ]] || die "NODE_TARBALL=$NODE_TARBALL has SHA-256 $ACTUAL, not the pinned $EXPECTED for $NAME, so it is the wrong file or it is damaged. Download $NODE_URL again, or unset NODE_TARBALL to let the build fetch it."
else
  NODE_TARBALL="$CACHE/$NAME"
  if [[ -f "$NODE_TARBALL" && "$(sha256_of "$NODE_TARBALL")" != "$EXPECTED" ]]; then
    echo "The cached $NODE_TARBALL doesn't match the pinned SHA-256 (an earlier download was damaged or altered); downloading it again." >&2
    rm -f "$NODE_TARBALL"
  fi
  if [[ ! -f "$NODE_TARBALL" ]]; then
    rm -f "$NODE_TARBALL.partial"
    curl -fsSL "$NODE_URL" -o "$NODE_TARBALL.partial" \
      || { rm -f "$NODE_TARBALL.partial"; die "Couldn't download $NODE_URL (curl's error is above). Check the network, or pass NODE_TARBALL=<path to $NAME>."; }
    ACTUAL="$(sha256_of "$NODE_TARBALL.partial")"
    if [[ "$ACTUAL" != "$EXPECTED" ]]; then
      rm -f "$NODE_TARBALL.partial"
      die "Downloaded $NODE_URL, but its SHA-256 is $ACTUAL, not the pinned $EXPECTED. Something between this Mac and nodejs.org (a proxy or captive portal) may have changed it; nothing was cached. Check the network and build again, or fetch the file another way, check it against $SUMS and pass NODE_TARBALL=<path>."
    fi
    mv "$NODE_TARBALL.partial" "$NODE_TARBALL"
  fi
fi

step "Assemble $APP"
# The previous app and its zips go together, so no zip outlives the build it came from.
ZIP_BASE="$OUT/Silicon-Extend-$VERSION-macos-$ARCH"
rm -rf "$APP"
rm -f "$ZIP_BASE.zip" "$ZIP_BASE-unnotarized.zip" "$ZIP_BASE-adhoc.zip" "$OUT/notarization.json"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/agent-device" "$APP/Contents/Resources/node"
sed "s/@VERSION@/$VERSION/g" "$ROOT/apps/desktop/macos/Info.plist.in" > "$APP/Contents/Info.plist"
# The app icon (Finder, and the Privacy & Security lists); icon/make-icns.sh rebuilds it from AppIcon.svg.
cp "$ROOT/apps/desktop/macos/icon/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
cp "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$ROOT/THIRD_PARTY_LICENSES.txt" "$APP/Contents/Resources/"
plutil -replace CFBundleIconFile -string AppIcon "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/extend-agent"
cp "$HELPER" "$APP/Contents/MacOS/agent-device-macos-helper"
# agent-device: its self-contained dist, plus the Apple sources it builds on first use
# (the UI testing runner for recording and other runner commands, and the helpers).
tar -C "$AD" --exclude='.build' --exclude='.swiftpm' --exclude='DerivedData' --exclude='xcuserdata' \
  -cf - bin dist package.json LICENSE apple/runner apple/snapshot-presentation apple/macos-helper apple/snapshot-bridge apple/fold-helper \
  | tar -C "$APP/Contents/Resources/agent-device" -xf -
tar -xzf "$NODE_TARBALL" -C "$APP/Contents/Resources/node" --strip-components=1 \
  --include='*/bin/node' --include='*/LICENSE'
BUNDLED_NODE="$APP/Contents/Resources/node/bin/node"
RUNTIME="$APP/Contents/Resources/agent-device"
# Extend's entry (runtime-entry.mjs) runs in front of agent-device's own: it replaces a daemon that
# a copy of the app at another location started, which fails once that copy is moved or deleted.
mv "$RUNTIME/bin/agent-device.mjs" "$RUNTIME/bin/agent-device-cli.mjs"
install -m 0755 "$ROOT/apps/desktop/runtime-entry.mjs" "$RUNTIME/bin/agent-device.mjs"
# Stamp before signing: signing timestamps must not invalidate an otherwise identical runtime.
# The stamp must be printed and in the staged manifest: an unstamped runtime would keep reusing an
# older daemon of the same upstream version after an update. stamp_runtime sets STAMPED, or stops
# the build saying why (even when the stamp is killed by a signal and prints nothing).
stamp_runtime "$BUNDLED_NODE" "$RUNTIME" "$APP/Contents/MacOS/agent-device-macos-helper"
# Read back with plutil rather than the stamp's own code (and without launching Node again).
MANIFEST_VERSION="$(plutil -extract version raw -o - "$RUNTIME/package.json" 2>/dev/null || true)"
MANIFEST_DIGEST="$(plutil -extract extendRuntime.sha256 raw -o - "$RUNTIME/package.json" 2>/dev/null || true)"
STAMP_PATTERN='^[^[:space:]]+[+.]extend\.[0-9a-f]{64}$'
if [[ ! "$STAMPED" =~ $STAMP_PATTERN || "$MANIFEST_VERSION" != "$STAMPED" || "$STAMPED" != *"extend.$MANIFEST_DIGEST" ]]; then
  die "agent-device in $RUNTIME wasn't stamped with a build identity (stamp-runtime.mjs printed '$STAMPED'; package.json has '$MANIFEST_VERSION'). Unstamped, the app would reuse an older agent-device daemon after an update. Fix the cause stamp-runtime.mjs reported above, if any, and build again."
fi
echo "agent-device runtime $STAMPED"
plutil -lint "$APP/Contents/Info.plist"

step "Code signature ($SIGN_IDENTITY)"
SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  SIGN_ARGS+=(--timestamp=none)
else
  SIGN_ARGS+=(--timestamp --options runtime)
fi
codesign "${SIGN_ARGS[@]}" --entitlements "$ROOT/apps/desktop/macos/node-entitlements.plist" "$APP/Contents/Resources/node/bin/node"
codesign "${SIGN_ARGS[@]}" "$APP/Contents/MacOS/agent-device-macos-helper"
codesign "${SIGN_ARGS[@]}" --entitlements "$ROOT/apps/desktop/macos/app-entitlements.plist" --identifier com.teamofsilicons.extend "$APP"
codesign --verify --deep --strict "$APP" && echo "signature ok"
"$APP/Contents/Resources/node/bin/node" -e 'if (new Function("return 42")() !== 42) process.exit(1)'

# Only a notarized, stapled and assessed app gets the plain release name, and every zip is written
# under a temporary name first, so an interrupted or failed run never leaves a release-named zip.
step "Zip"
zip_app() {
  rm -f "$1.partial"
  (cd "$OUT" && ditto -c -k --keepParent "Silicon Extend.app" "$1.partial")
  mv "$1.partial" "$1"
}
if [[ "$SIGN_IDENTITY" == "-" ]]; then
  ZIP="$ZIP_BASE-adhoc.zip"
  SIGNED="Ad-hoc signed (no SIGN_IDENTITY): it runs on this Mac, and Gatekeeper rejects it on others. Not for distribution."
elif [[ -z "${NOTARY_PROFILE:-}" ]]; then
  ZIP="$ZIP_BASE-unnotarized.zip"
  SIGNED="Developer ID signed but not notarized (no NOTARY_PROFILE): Gatekeeper rejects it on other Macs until it is notarized."
else
  step "Notarization"
  SUBMISSION="$OUT/notary-submission.zip"
  trap 'rm -f "$SUBMISSION" "$SUBMISSION.partial"' EXIT
  zip_app "$SUBMISSION"
  RESULT="$OUT/notarization.json"
  xcrun notarytool submit "$SUBMISSION" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json > "$RESULT" \
    || die "notarytool couldn't submit $SUBMISSION (its error is above; the reply, if any, is in $RESULT). No zip was written. Check the Keychain profile $NOTARY_PROFILE and the network, then build again."
  STATUS="$(plutil -extract status raw "$RESULT" 2>/dev/null || true)"
  if [[ "$STATUS" != "Accepted" ]]; then
    cat "$RESULT" >&2
    die "Apple didn't accept the notarization (status: ${STATUS:-unknown}), so the app would be rejected by Gatekeeper. No zip was written. See why with: xcrun notarytool log $(plutil -extract id raw "$RESULT" 2>/dev/null || echo '<id>') --keychain-profile $NOTARY_PROFILE; fix it and build again."
  fi
  { xcrun stapler staple "$APP" && xcrun stapler validate "$APP"; } \
    || die "Apple accepted the notarization, but stapling its ticket to $APP failed (error above), often because the ticket hasn't reached Apple's servers yet. No zip was written. Wait a few minutes and build again."
  spctl --assess --type execute --verbose "$APP" \
    || die "Gatekeeper rejected the notarized $APP (spctl's reason is above). No zip was written. Fix the signature or entitlements it names and build again."
  ZIP="$ZIP_BASE.zip"
  SIGNED="Developer ID signed, notarized and stapled."
fi
zip_app "$ZIP"
du -sh "$APP" "$ZIP"
echo
echo "Built $APP"
echo "Zip: $ZIP"
echo "$SIGNED"
echo "Try it: \"$APP/Contents/MacOS/extend-agent\" probe"
