#!/usr/bin/env bash
# Builds "Silicon Bridge.app": the bridge-agent binary, Bridge's agent-device fork, a Node runtime
# and agent-device's macOS helper. Set SIGN_IDENTITY for Developer ID signing.
#
#   apps/desktop/macos/build-app.sh            # release build for this Mac's architecture
#   PROFILE=debug apps/desktop/macos/build-app.sh
#   SIGN_IDENTITY='Developer ID Application: …' apps/desktop/macos/build-app.sh
#   NOTARY_PROFILE=bridge-release SIGN_IDENTITY='…' apps/desktop/macos/build-app.sh
#   NODE_TARBALL=/path/node-v22.23.3-darwin-arm64.tar.gz … (offline, checksum still verified)
#
# Output: target/desktop/macos/Silicon Bridge.app and Silicon-Bridge-<version>-macos-<arch>.zip
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${PROFILE:-release}"
NODE_VERSION="22.23.3"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
ARCH="$(uname -m)"; [[ "$ARCH" == "x86_64" ]] && NODE_ARCH=x64 || NODE_ARCH=arm64
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="$ROOT/target/desktop/macos"
APP="$OUT/Silicon Bridge.app"
CACHE="$ROOT/target/desktop/.cache"
AD="$ROOT/vendor/agent-device"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

step() { printf '\n== %s\n' "$*"; }

if [[ -n "${NOTARY_PROFILE:-}" && "$SIGN_IDENTITY" == "-" ]]; then
  echo "NOTARY_PROFILE requires a Developer ID SIGN_IDENTITY" >&2
  exit 1
fi

step "agent-device fork (pnpm install && pnpm build)"
(cd "$AD" && pnpm install --frozen-lockfile && pnpm build)
# The macOS helper, built once and shipped signed inside the app so permissions stick to it.
(cd "$AD" && pnpm build:macos-helper) >/dev/null
HELPER="$(find "$AD/apple/macos-helper/.build" -type f -name agent-device-macos-helper -perm -u+x -ipath '*release*' | head -1)"
[[ -n "$HELPER" ]] || { echo "agent-device-macos-helper didn't build"; exit 1; }

step "bridge-agent ($PROFILE)"
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release -p bridge-agent --manifest-path "$ROOT/Cargo.toml"
else
  cargo build -p bridge-agent --manifest-path "$ROOT/Cargo.toml"
fi
BIN="$TARGET_DIR/$PROFILE/bridge-agent"

step "Node $NODE_VERSION for darwin-$NODE_ARCH"
mkdir -p "$CACHE"
NAME="node-v$NODE_VERSION-darwin-$NODE_ARCH.tar.gz"
if [[ -z "${NODE_TARBALL:-}" ]]; then
  NODE_TARBALL="$CACHE/$NAME"
  if [[ ! -f "$NODE_TARBALL" ]]; then
    curl -fsSL "https://nodejs.org/dist/v$NODE_VERSION/$NAME" -o "$NODE_TARBALL.partial"
    mv "$NODE_TARBALL.partial" "$NODE_TARBALL"
  fi
fi
EXPECTED="$(awk -v name="$NAME" '$2 == name {print $1}' "$ROOT/apps/desktop/macos/node-sha256.txt")"
ACTUAL="$(shasum -a 256 "$NODE_TARBALL" | awk '{print $1}')"
[[ -n "$EXPECTED" && "$ACTUAL" == "$EXPECTED" ]] || { echo "Node checksum mismatch: $NODE_TARBALL" >&2; exit 1; }

step "Assemble $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/agent-device" "$APP/Contents/Resources/node"
sed "s/@VERSION@/$VERSION/g" "$ROOT/apps/desktop/macos/Info.plist.in" > "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/bridge-agent"
cp "$HELPER" "$APP/Contents/MacOS/agent-device-macos-helper"
# agent-device: its self-contained dist, plus the Apple sources it builds on first use
# (the UI testing runner for typing and recording, and the helpers).
tar -C "$AD" --exclude='.build' --exclude='.swiftpm' --exclude='DerivedData' --exclude='xcuserdata' \
  -cf - bin dist package.json LICENSE apple/runner apple/snapshot-presentation apple/macos-helper apple/snapshot-bridge apple/fold-helper \
  | tar -C "$APP/Contents/Resources/agent-device" -xf -
tar -xzf "$NODE_TARBALL" -C "$APP/Contents/Resources/node" --strip-components=1 \
  --include='*/bin/node' --include='*/LICENSE'
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
codesign "${SIGN_ARGS[@]}" --entitlements "$ROOT/apps/desktop/macos/app-entitlements.plist" --identifier com.teamofsilicons.bridge "$APP"
codesign --verify --deep --strict "$APP" && echo "signature ok"
"$APP/Contents/Resources/node/bin/node" -e 'if (new Function("return 42")() !== 42) process.exit(1)'

step "Zip"
ZIP="$OUT/Silicon-Bridge-$VERSION-macos-$ARCH.zip"
rm -f "$ZIP"
(cd "$OUT" && ditto -c -k --keepParent "Silicon Bridge.app" "$ZIP")
if [[ -n "${NOTARY_PROFILE:-}" ]]; then
  step "Notarization"
  RESULT="$OUT/notarization.json"
  xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json > "$RESULT"
  [[ "$(plutil -extract status raw "$RESULT")" == "Accepted" ]] || { cat "$RESULT"; exit 1; }
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  spctl --assess --type execute --verbose "$APP"
  rm -f "$ZIP"
  (cd "$OUT" && ditto -c -k --keepParent "Silicon Bridge.app" "$ZIP")
fi
du -sh "$APP" "$ZIP"
echo
echo "Built $APP"
if [[ -z "${NOTARY_PROFILE:-}" ]]; then echo "Not notarized; distribution verification is still pending."; fi
echo "Try it: \"$APP/Contents/MacOS/bridge-agent\" probe"
