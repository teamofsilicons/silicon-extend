#!/usr/bin/env bash
# Builds the Linux tarball and .deb for Silicon Extend. Run it on Linux (or in the linux-e2e
# container: apps/desktop/linux/build-in-docker.sh). Not published anywhere.
#
# Layout (the same inside the tarball and under /usr in the .deb):
#   bin/extend-agent
#   lib/silicon-extend/agent-device/{bin,dist,linux,package.json}   Extend's agent-device fork
#   lib/silicon-extend/node/bin/node                               Node 22 for agent-device
#   share/applications/silicon-extend.desktop
#   share/doc/silicon-extend/README
# extend-agent finds agent-device and node through ../lib/silicon-extend next to its own binary.
#
#   PROFILE=debug …        use a debug build
#   EXTEND_AGENT_BIN=…     package this binary instead of building one
#   NODE_TARBALL=…         use this Node tarball instead of downloading
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${PROFILE:-release}"
NODE_VERSION="22.23.3"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="${OUT:-$TARGET_DIR/desktop/linux}"
AD="$ROOT/vendor/agent-device"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
case "$(uname -m)" in x86_64) ARCH=x64; DEB_ARCH=amd64;; aarch64|arm64) ARCH=arm64; DEB_ARCH=arm64;; *) echo "unsupported $(uname -m)"; exit 1;; esac
[[ -f "$AD/dist/src/internal/bin.js" ]] || { echo "Build agent-device first: (cd vendor/agent-device && pnpm install && pnpm build)"; exit 1; }

if [[ -z "${EXTEND_AGENT_BIN:-}" ]]; then
  if [[ "$PROFILE" == "release" ]]; then cargo build --release -p extend-agent --manifest-path "$ROOT/Cargo.toml"; else cargo build -p extend-agent --manifest-path "$ROOT/Cargo.toml"; fi
  EXTEND_AGENT_BIN="$TARGET_DIR/$PROFILE/extend-agent"
fi

mkdir -p "$OUT/.cache"
NAME="node-v$NODE_VERSION-linux-$ARCH.tar.xz"
if [[ -z "${NODE_TARBALL:-}" ]]; then
  NODE_TARBALL="$OUT/.cache/$NAME"
  if [[ ! -f "$NODE_TARBALL" ]]; then
    curl -fsSL "https://nodejs.org/dist/v$NODE_VERSION/$NAME" -o "$NODE_TARBALL.partial"
    mv "$NODE_TARBALL.partial" "$NODE_TARBALL"
  fi
fi
EXPECTED="$(awk -v name="$NAME" '$2 == name {print $1}' "$ROOT/apps/desktop/linux/node-sha256.txt")"
ACTUAL="$(sha256sum "$NODE_TARBALL" | awk '{print $1}')"
[[ -n "$EXPECTED" && "$ACTUAL" == "$EXPECTED" ]] || { echo "Node checksum mismatch: $NODE_TARBALL" >&2; exit 1; }

NAME="silicon-extend-$VERSION-linux-$ARCH"
STAGE="$OUT/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib/silicon-extend/agent-device" "$STAGE/lib/silicon-extend/node" "$STAGE/share/applications" "$STAGE/share/doc/silicon-extend"
install -m 0755 "$EXTEND_AGENT_BIN" "$STAGE/bin/extend-agent"
tar -C "$AD" -cf - bin dist linux package.json LICENSE | tar -C "$STAGE/lib/silicon-extend/agent-device" -xf -
tar -xJf "$NODE_TARBALL" -C "$STAGE/lib/silicon-extend/node" --strip-components=1 --wildcards '*/bin/node' '*/LICENSE'
"$STAGE/lib/silicon-extend/node/bin/node" "$ROOT/apps/desktop/stamp-runtime.mjs" "$STAGE/lib/silicon-extend/agent-device"
cat > "$STAGE/share/applications/silicon-extend.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Silicon Extend
Comment=Lets the Silicons you choose use this computer
Exec=extend-agent run
Terminal=false
Categories=Utility;
EOF
cat > "$STAGE/share/doc/silicon-extend/README" <<EOF
Silicon Extend $VERSION for Linux

  extend-agent run                 start it (tray icon; shows the pairing code)
  extend-agent run --headless      servers and CI: status on stdout
  extend-agent install-autostart   start at login (add --systemd for a user service)
  extend-agent status              what it is doing
  extend-agent probe               what this computer can do right now

Screen reading needs the AT-SPI bus (at-spi2-core, python3-gi, gir1.2-atspi-2.0); clicking and
typing need xdotool (X11) or ydotool (Wayland); screenshots need gnome-screenshot, scrot or
ImageMagick (grim on Wayland); the clipboard needs xclip or xsel (wl-clipboard on Wayland).
Whole-screen X11 recording needs ffmpeg (with ffprobe) and x11-utils (xwininfo). App-only
recording also needs xdotool and libxcomposite1: the named app must match exactly one mapped
WM_CLASS. Covered windows are captured; resizing or unmapping ends capture. Wayland
ScreenCast portal support remains under development.
EOF

tar -C "$OUT" -czf "$OUT/$NAME.tar.gz" "$NAME"
echo "tarball: $OUT/$NAME.tar.gz"

if command -v dpkg-deb >/dev/null; then
  command -v dpkg-shlibdeps >/dev/null || { echo "Install dpkg-dev to derive package library requirements" >&2; exit 1; }
  DEB="$OUT/deb/debian/silicon-extend"
  rm -rf "$DEB"; mkdir -p "$DEB/DEBIAN" "$DEB/usr"
  cp -a "$STAGE/." "$DEB/usr/"
  cat > "$OUT/deb/debian/control" <<EOF
Source: silicon-extend

Package: silicon-extend
Architecture: any
EOF
  SHLIBS="$(cd "$OUT/deb" && dpkg-shlibdeps -O -e"$DEB/usr/bin/extend-agent" -e"$DEB/usr/lib/silicon-extend/node/bin/node")"
  DEPENDS="$(printf '%s\n' "$SHLIBS" | sed -n 's/^shlibs:Depends=//p')"
  [[ -n "$DEPENDS" ]] || { echo "Could not derive native library dependencies" >&2; exit 1; }
  cat > "$DEB/DEBIAN/control" <<EOF
Package: silicon-extend
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: Team of Silicons <team@teamofsilicons.com>
Section: utils
Priority: optional
Depends: $DEPENDS, python3, python3-gi, gir1.2-atspi-2.0, at-spi2-core
Recommends: xdotool, xclip, imagemagick, xdg-utils, ffmpeg, x11-utils, libxcomposite1, libayatana-appindicator3-1
Description: Silicon Extend for Linux
 Lets the Silicons a Carbon chooses use this computer, with an always-visible
 indicator and a Stop button.
EOF
  dpkg-deb --root-owner-group --build "$DEB" "$OUT/silicon-extend_${VERSION}_${DEB_ARCH}.deb"
  echo "deb: $OUT/silicon-extend_${VERSION}_${DEB_ARCH}.deb"
fi
