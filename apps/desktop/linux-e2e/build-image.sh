#!/usr/bin/env bash
# Builds the silicon-extend-linux-e2e image every Linux lane runs in: Xvfb, openbox, the AT-SPI
# bus, GTK, xdotool, ffmpeg, Node 22 and the Rust toolchain (see Dockerfile). run.sh,
# record-e2e.sh (via the README command), record-service-e2e.py and
# apps/desktop/linux/build-in-docker.sh all use this tag.
#
#   apps/desktop/linux-e2e/build-image.sh            # build when missing or built from another Dockerfile
#   apps/desktop/linux-e2e/build-image.sh --rebuild  # build even when the tag is current
#   IMAGE=my-tag apps/desktop/linux-e2e/build-image.sh
#
# The image carries the SHA-256 of the Dockerfile it was built from, so an image built by hand
# or from an older Dockerfile (for example one without ffmpeg) is rebuilt instead of reused.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
IMAGE="${IMAGE:-silicon-extend-linux-e2e}"
LABEL=org.teamofsilicons.extend.linux-e2e.dockerfile-sha256
if command -v sha256sum >/dev/null; then
  WANT="$(sha256sum "$HERE/Dockerfile" | awk '{print $1}')"
else
  WANT="$(shasum -a 256 "$HERE/Dockerfile" | awk '{print $1}')"
fi
HAVE="$(docker image inspect "$IMAGE" --format "{{ index .Config.Labels \"$LABEL\" }}" 2>/dev/null || true)"
if [[ "${1:-}" != "--rebuild" && "$HAVE" == "$WANT" ]]; then
  echo "$IMAGE is current (Dockerfile $WANT)"
  exit 0
fi
if [[ -n "$HAVE" || "$(docker image inspect "$IMAGE" --format ok 2>/dev/null || true)" == ok ]]; then
  echo "Rebuilding $IMAGE: it was built from ${HAVE:-an unrecorded Dockerfile}, not $WANT"
fi
docker build --label "$LABEL=$WANT" -t "$IMAGE" "$HERE"
