#!/usr/bin/env bash
# Builds the Linux tarball and .deb for Silicon Extend. Run it on Linux (or in the linux-e2e
# container: apps/desktop/linux/build-in-docker.sh). Not published anywhere.
#
# Layout (the same inside the tarball and under /usr in the .deb):
#   bin/extend-agent
#   lib/silicon-extend/agent-device/{bin,dist,linux,package.json}   Extend's agent-device fork, behind
#                                                                  Extend's entry (runtime-entry.mjs)
#   lib/silicon-extend/node/bin/node                               Node 22 for agent-device
#   share/applications/silicon-extend.desktop
#   share/doc/silicon-extend/README
# extend-agent finds agent-device and node through ../lib/silicon-extend next to its own binary.
#
#   PROFILE=debug …        use a debug build
#   EXTEND_AGENT_BIN=…     package this binary instead of building one
#   NODE_TARBALL=…         use this Node tarball instead of downloading (checksum still verified)
#
# With pnpm on PATH it rebuilds the agent-device fork first, like the macOS build. Without pnpm
# (the linux-e2e container, where build-in-docker.sh has just built it on the host) it packages
# vendor/agent-device/dist only if nothing the build reads is newer than it.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PROFILE="${PROFILE:-release}"
NODE_VERSION="22.23.3"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="${OUT:-$TARGET_DIR/desktop/linux}"
AD="$ROOT/vendor/agent-device"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
die() { printf '%s\n' "$*" >&2; exit 1; }
case "$(uname -m)" in x86_64) ARCH=x64; DEB_ARCH=amd64;; aarch64|arm64) ARCH=arm64; DEB_ARCH=arm64;; *) die "Silicon Extend packages Linux for x86_64 and aarch64 only, and this machine is $(uname -m). Build on one of those.";; esac

# Packaging a stale dist would ship old behaviour under a valid build identity, so the fork is
# rebuilt here, or its build must be newer than every file the build reads.
BUILT="$AD/dist/src/internal/bin.js"
REBUILD_HINT="(cd vendor/agent-device && pnpm install --frozen-lockfile && pnpm build)"
newer_agent_device_source() {
  local input inputs=()
  for input in "$AD/src" "$AD/packages" "$AD/package.json" "$AD/pnpm-lock.yaml" "$AD/pnpm-workspace.yaml" "$AD/tsdown.config.ts" "$AD"/tsconfig*.json; do
    if [[ -e "$input" ]]; then inputs+=("$input"); fi
  done
  find "${inputs[@]}" \( -name node_modules -o -name dist -o -name .build \) -prune -o -type f -newer "$BUILT" -print -quit
}
if command -v pnpm >/dev/null; then
  echo "== agent-device fork (pnpm install && pnpm build)"
  (cd "$AD" && pnpm install --frozen-lockfile && pnpm build)
elif [[ ! -f "$BUILT" ]]; then
  die "vendor/agent-device isn't built and pnpm isn't installed to build it, so there is no agent-device to package. Install pnpm, or build it where pnpm is: $REBUILD_HINT, then package again."
else
  STALE="$(newer_agent_device_source)"
  [[ -z "$STALE" ]] || die "vendor/agent-device/dist is older than ${STALE#"$AD/"}, so it may not contain the current code, and pnpm isn't installed to rebuild it. Rebuild it where pnpm is: $REBUILD_HINT, then package again."
fi

if [[ -z "${EXTEND_AGENT_BIN:-}" ]]; then
  if [[ "$PROFILE" == "release" ]]; then cargo build --release -p extend-agent --manifest-path "$ROOT/Cargo.toml"; else cargo build -p extend-agent --manifest-path "$ROOT/Cargo.toml"; fi
  EXTEND_AGENT_BIN="$TARGET_DIR/$PROFILE/extend-agent"
fi

mkdir -p "$OUT/.cache"
NAME="node-v$NODE_VERSION-linux-$ARCH.tar.xz"
NODE_URL="https://nodejs.org/dist/v$NODE_VERSION/$NAME"
SUMS="$ROOT/apps/desktop/linux/node-sha256.txt"
EXPECTED="$(awk -v name="$NAME" '$2 == name {print $1}' "$SUMS")"
[[ -n "$EXPECTED" ]] || die "No pinned SHA-256 for $NAME in $SUMS, so the Node download can't be verified. Add its line from https://nodejs.org/dist/v$NODE_VERSION/SHASUMS256.txt and package again."
sha256_of() { sha256sum "$1" | awk '{print $1}'; }
# Verify before caching: a download that fails the check is never kept, and a cached file that
# fails it (damaged or altered earlier) is replaced once rather than failing every build.
if [[ -n "${NODE_TARBALL:-}" ]]; then
  ACTUAL="$(sha256_of "$NODE_TARBALL")"
  [[ "$ACTUAL" == "$EXPECTED" ]] || die "NODE_TARBALL=$NODE_TARBALL has SHA-256 $ACTUAL, not the pinned $EXPECTED for $NAME, so it is the wrong file or it is damaged. Download $NODE_URL again, or unset NODE_TARBALL to let the build fetch it."
else
  NODE_TARBALL="$OUT/.cache/$NAME"
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
      die "Downloaded $NODE_URL, but its SHA-256 is $ACTUAL, not the pinned $EXPECTED. Something between this machine and nodejs.org (a proxy or captive portal) may have changed it; nothing was cached. Check the network and package again, or fetch the file another way, check it against $SUMS and pass NODE_TARBALL=<path>."
    fi
    mv "$NODE_TARBALL.partial" "$NODE_TARBALL"
  fi
fi

NAME="silicon-extend-$VERSION-linux-$ARCH"
STAGE="$OUT/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib/silicon-extend/agent-device" "$STAGE/lib/silicon-extend/node" "$STAGE/share/applications" "$STAGE/share/doc/silicon-extend"
install -m 0755 "$EXTEND_AGENT_BIN" "$STAGE/bin/extend-agent"
tar -C "$AD" -cf - bin dist linux package.json LICENSE | tar -C "$STAGE/lib/silicon-extend/agent-device" -xf -
tar -xJf "$NODE_TARBALL" -C "$STAGE/lib/silicon-extend/node" --strip-components=1 --wildcards '*/bin/node' '*/LICENSE'
BUNDLED_NODE="$STAGE/lib/silicon-extend/node/bin/node"
RUNTIME="$STAGE/lib/silicon-extend/agent-device"
# Extend's entry (runtime-entry.mjs) runs in front of agent-device's own: it replaces a daemon that
# a copy of Silicon Extend at another location started (an unpacked tarball, say), which fails once
# that copy is moved or deleted.
mv "$RUNTIME/bin/agent-device.mjs" "$RUNTIME/bin/agent-device-cli.mjs"
install -m 0755 "$ROOT/apps/desktop/runtime-entry.mjs" "$RUNTIME/bin/agent-device.mjs"
# The stamp must be printed and in the staged manifest: an unstamped runtime would keep reusing an
# older daemon of the same upstream version after an update.
STAMPED="$("$BUNDLED_NODE" "$ROOT/apps/desktop/stamp-runtime.mjs" "$RUNTIME")"
# Read back independently of the stamp's own code.
MANIFEST_VERSION="$("$BUNDLED_NODE" -p 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).version' "$RUNTIME/package.json")"
MANIFEST_DIGEST="$("$BUNDLED_NODE" -p 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).extendRuntime?.sha256 ?? ""' "$RUNTIME/package.json")"
STAMP_PATTERN='^[^[:space:]]+[+.]extend\.[0-9a-f]{64}$'
if [[ ! "$STAMPED" =~ $STAMP_PATTERN || "$MANIFEST_VERSION" != "$STAMPED" || "$STAMPED" != *"extend.$MANIFEST_DIGEST" ]]; then
  die "agent-device in $RUNTIME wasn't stamped with a build identity (stamp-runtime.mjs printed '$STAMPED'; package.json has '$MANIFEST_VERSION'). Unstamped, the package would reuse an older agent-device daemon after an update. Fix the cause stamp-runtime.mjs reported above, if any, and package again."
fi
echo "agent-device runtime $STAMPED"
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
