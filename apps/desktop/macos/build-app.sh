#!/usr/bin/env bash
# Builds "Silicon Bridge.app": the bridge-agent binary, Bridge's agent-device fork, a Node runtime
# and agent-device's macOS helper, ad-hoc signed. Not notarized, not published.
#
#   apps/desktop/macos/build-app.sh            # release build for this Mac's architecture
#   PROFILE=debug apps/desktop/macos/build-app.sh
#   NODE_TARBALL=/path/node-v22.x-darwin-arm64.tar.gz …   (offline: use this Node instead of downloading)
#
# Output: target/desktop/macos/Silicon Bridge.app and Silicon-Bridge-<version>-macos-<arch>.zip
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${PROFILE:-release}"
NODE_MAJOR="${NODE_MAJOR:-22}"
ARCH="$(uname -m)"; [[ "$ARCH" == "x86_64" ]] && NODE_ARCH=x64 || NODE_ARCH=arm64
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="$ROOT/target/desktop/macos"
APP="$OUT/Silicon Bridge.app"
CACHE="$ROOT/target/desktop/.cache"
AD="$ROOT/vendor/agent-device"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

step() { printf '\n== %s\n' "$*"; }

step "agent-device fork (pnpm install && pnpm build)"
if [[ ! -f "$AD/dist/src/internal/bin.js" ]]; then
  (cd "$AD" && pnpm install --frozen-lockfile && pnpm build)
fi
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

step "Node $NODE_MAJOR for darwin-$NODE_ARCH"
mkdir -p "$CACHE"
if [[ -z "${NODE_TARBALL:-}" ]]; then
  NAME="$(curl -fsSL "https://nodejs.org/dist/latest-v${NODE_MAJOR}.x/" | grep -o "node-v${NODE_MAJOR}[.0-9]*-darwin-${NODE_ARCH}.tar.gz" | head -1)"
  NODE_TARBALL="$CACHE/$NAME"
  [[ -f "$NODE_TARBALL" ]] || curl -fsSL "https://nodejs.org/dist/latest-v${NODE_MAJOR}.x/$NAME" -o "$NODE_TARBALL"
fi

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

step "Ad-hoc code signature"
codesign --force --sign - --timestamp=none "$APP/Contents/Resources/node/bin/node"
codesign --force --sign - --timestamp=none "$APP/Contents/MacOS/agent-device-macos-helper"
codesign --force --sign - --timestamp=none --identifier com.teamofsilicons.bridge "$APP"
codesign --verify --deep --strict "$APP" && echo "signature ok"

step "Zip"
ZIP="$OUT/Silicon-Bridge-$VERSION-macos-$ARCH.zip"
rm -f "$ZIP"
(cd "$OUT" && ditto -c -k --keepParent "Silicon Bridge.app" "$ZIP")
du -sh "$APP" "$ZIP"
echo
echo "Built $APP"
echo "Try it: \"$APP/Contents/MacOS/bridge-agent\" probe"
