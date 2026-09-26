#!/usr/bin/env bash
set -euo pipefail
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >/tmp/out/xvfb.log 2>&1 &
xvfb_pid=$!
agent_pid=''
cleanup() {
  if [[ -n "$agent_pid" ]]; then kill "$agent_pid" 2>/dev/null || true; wait "$agent_pid" || true; fi
  kill "$xvfb_pid" 2>/dev/null || true
  wait "$xvfb_pid" || true
}
trap cleanup EXIT
trap 'exit 0' TERM INT
export DISPLAY=:99 XDG_SESSION_TYPE=x11
unset WAYLAND_DISPLAY
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep .1; done
bridge-agent run --headless >/tmp/out/agent.log 2>&1 &
agent_pid=$!
wait "$agent_pid"
