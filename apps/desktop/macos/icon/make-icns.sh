#!/usr/bin/env bash
# Rebuilds AppIcon.png (1024 px) and AppIcon.icns from AppIcon.svg. Both outputs are committed, so
# build-app.sh never needs this; run it after changing AppIcon.svg. Needs rsvg-convert (brew install
# librsvg) and iconutil (part of macOS).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
command -v rsvg-convert >/dev/null || { echo "rsvg-convert isn't installed, so AppIcon.svg can't be rendered. Install it with: brew install librsvg" >&2; exit 1; }
SET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$SET"
render() { rsvg-convert -w "$1" -h "$1" "$HERE/AppIcon.svg" -o "$SET/$2"; }
for s in 16 32 128 256 512; do
  render "$s" "icon_${s}x${s}.png"
  render "$((s * 2))" "icon_${s}x${s}@2x.png"
done
cp "$SET/icon_512x512@2x.png" "$HERE/AppIcon.png"
iconutil -c icns "$SET" -o "$HERE/AppIcon.icns"
rm -rf "$(dirname "$SET")"
echo "Wrote $HERE/AppIcon.png and $HERE/AppIcon.icns"
