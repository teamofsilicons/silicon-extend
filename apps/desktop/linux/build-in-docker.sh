#!/usr/bin/env bash
# Builds the Linux tarball and .deb inside the linux-e2e image (host architecture), then installs
# the .deb in a clean copy of that container and runs `extend-agent probe` from it.
# Output: target/desktop/linux/
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
IMAGE="${IMAGE:-silicon-extend-linux-e2e}"
# Bundle the current source, even when an older dist directory already exists, and record what it
# was built from: the container has no pnpm, and build-package.sh checks the dist against it.
(cd "$ROOT/vendor/extend-engine" && pnpm install --frozen-lockfile && pnpm build)
node "$ROOT/apps/desktop/dist-manifest.mjs" record "$ROOT/vendor/extend-engine"
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -t "$IMAGE" "$ROOT/apps/desktop/linux-e2e"
mkdir -p "$ROOT/target/desktop/linux"
docker run --rm \
  -v "$ROOT":/src:ro -v "$ROOT/target/desktop/linux":/out \
  -v silicon-extend-linux-target:/target -v silicon-extend-cargo-registry:/usr/local/cargo/registry \
  -e PROFILE="${PROFILE:-release}" \
  "$IMAGE" bash -euo pipefail -c '
    mkdir -p /work && tar -C /src --exclude=./target --exclude=node_modules --exclude=./.git -cf - . | tar -C /work -xf -
    cd /work && CARGO_TARGET_DIR=/target OUT=/out bash apps/desktop/linux/build-package.sh
    echo "== install the .deb and run it"
    apt-get install -y -qq /out/silicon-extend_*.deb >/dev/null
    rm -rf /work /target/debug/extend-agent.d
    command -v extend-agent
    SILICON_HOME=/tmp/h extend-agent --version
    env -u DISPLAY PATH=/usr/bin:/bin SILICON_HOME=/tmp/h extend-agent probe
    SILICON_HOME=/tmp/h extend-agent exec terminal run "echo installed-ok"
  '
ls -la "$ROOT/target/desktop/linux"
