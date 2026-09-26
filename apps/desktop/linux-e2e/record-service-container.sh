#!/usr/bin/env bash
# Owned test container only. The installed package must supply both runtime and Node.
set -euo pipefail
dpkg -i /tmp/bridge-package.deb >/tmp/package-install.log 2>&1
test -f /usr/lib/silicon-bridge/agent-device/linux/x11_composite.py
exec runuser -u carbon -- env PATH=/tmp/out:/usr/bin:/bin \
  SILICON_HOME=/tmp/out/agent-home BRIDGE_AGENT_CREDENTIAL_STORE=file \
  BRIDGE_API_URL="$BRIDGE_API_URL" BRIDGE_TELEMETRY=off \
  bash /harness/record-service-desktop.sh
