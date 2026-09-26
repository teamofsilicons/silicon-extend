#!/usr/bin/env bash
# Inside the linux-e2e image (with ffmpeg installed), /src read-only and /tmp/out writable.
set -euo pipefail
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >/tmp/record-xvfb.log 2>&1 &
xvfb_pid=$!
trap 'kill "$xvfb_pid" 2>/dev/null || true; wait "$xvfb_pid" 2>/dev/null || true' EXIT
export DISPLAY=:99
unset WAYLAND_DISPLAY
export XDG_SESSION_TYPE=x11
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep .1; done
python3 /src/apps/desktop/linux-e2e/record-e2e.py
