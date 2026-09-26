#!/usr/bin/env bash
# Installs the Linux package into a pristine debian:trixie container the way a Carbon would, with
# apt resolving its dependencies, and proves the installed programs start. The linux-e2e image
# already holds every -dev package and the whole toolchain, so an install there cannot show that
# the package's own Depends line is enough; this check can. record-service-e2e.py runs it twice:
#   PHASE=depends     apt install --no-install-recommends: Depends alone must be enough for
#                     extend-agent, the bundled Node and agent-device to start
#   PHASE=recommends  a default apt install: Recommends must also bring every tool and library
#                     X11 recording needs (whole screen and single app)
# Mounts: the package at /tmp/extend-package.deb. Needs network access for apt.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
PHASE="${PHASE:?set PHASE=depends or PHASE=recommends}"
flags=()
[[ "$PHASE" == depends ]] && flags=(--no-install-recommends)
apt-get update -qq >/dev/null
if ! apt-get install -y -qq "${flags[@]}" /tmp/extend-package.deb >/tmp/apt.log 2>&1; then
  tail -40 /tmp/apt.log
  echo "FAILED: apt could not install the package into a pristine debian:trixie ($PHASE)"
  exit 1
fi
for binary in /usr/bin/extend-agent /usr/lib/silicon-extend/node/bin/node; do
  missing="$(ldd "$binary" | grep 'not found' || true)"
  if [[ -n "$missing" ]]; then
    echo "FAILED: $binary is missing libraries that the package does not depend on:"
    echo "$missing"
    exit 1
  fi
done
SILICON_HOME=/tmp/h extend-agent --version
/usr/lib/silicon-extend/node/bin/node /usr/lib/silicon-extend/agent-device/bin/agent-device.mjs --version
env -u DISPLAY SILICON_HOME=/tmp/h extend-agent probe >/tmp/probe.txt
python3 -c 'import gi; gi.require_version("Atspi", "2.0"); from gi.repository import Atspi'
if [[ "$PHASE" == recommends ]]; then
  for tool in ffmpeg ffprobe xwininfo xdotool; do
    command -v "$tool" >/dev/null || { echo "FAILED: the package's Recommends do not bring $tool"; exit 1; }
  done
  # The app-recording worker loads these through ctypes, so dpkg-shlibdeps cannot see them.
  python3 - <<'EOF'
import ctypes
for name, package in [('libX11.so.6', 'libx11-6'), ('libXcomposite.so.1', 'libxcomposite1'),
                      ('libXdamage.so.1', 'libxdamage1'), ('libXfixes.so.3', 'libxfixes3')]:
    try:
        ctypes.CDLL(name)
    except OSError:
        raise SystemExit(f'FAILED: the package does not bring {name} ({package}), which app recording loads')
EOF
fi
echo "PASS $PHASE: apt installed the package into a pristine debian:trixie and it starts"
