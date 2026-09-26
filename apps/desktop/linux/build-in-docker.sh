#!/usr/bin/env bash
# Builds the Linux tarball and .deb inside the linux-e2e image (host architecture), then installs
# the .deb in a clean copy of that container and runs `bridge-agent probe` from it.
# Output: target/desktop/linux/
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
IMAGE="${IMAGE:-silicon-bridge-linux-e2e}"
# Bundle the current source, even when an older dist directory already exists.
(cd "$ROOT/vendor/agent-device" && pnpm install --frozen-lockfile && pnpm build)
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -t "$IMAGE" "$ROOT/apps/desktop/linux-e2e"
mkdir -p "$ROOT/target/desktop/linux"
docker run --rm \
  -v "$ROOT":/src:ro -v "$ROOT/target/desktop/linux":/out \
  -v silicon-bridge-linux-target:/target -v silicon-bridge-cargo-registry:/usr/local/cargo/registry \
  -e PROFILE="${PROFILE:-release}" \
  "$IMAGE" bash -euo pipefail -c '
    mkdir -p /work && tar -C /src --exclude=./target --exclude=node_modules --exclude=./.git -cf - . | tar -C /work -xf -
    cd /work && CARGO_TARGET_DIR=/target OUT=/out bash apps/desktop/linux/build-package.sh
    echo "== install the .deb and run it"
    apt-get install -y -qq /out/silicon-bridge_*.deb >/dev/null
    rm -rf /work /target/debug/bridge-agent.d
    command -v bridge-agent
    SILICON_HOME=/tmp/h bridge-agent --version
    env -u DISPLAY PATH=/usr/bin:/bin SILICON_HOME=/tmp/h bridge-agent probe
    SILICON_HOME=/tmp/h bridge-agent exec terminal run "echo installed-ok"
  '
ls -la "$ROOT/target/desktop/linux"
