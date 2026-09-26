#!/usr/bin/env bash
# Builds the image (when missing or stale, see build-image.sh) and runs e2e.sh in it with the repository mounted read-only.
#   apps/desktop/linux-e2e/run.sh                       # driver run on a real X11 desktop
#   EXTEND_E2E_SERVICE=http://host.docker.internal:8480 apps/desktop/linux-e2e/run.sh
#                                                       # plus the full agent against a service
#   RUN_TESTS=0 …                                       # skip cargo test inside the container
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
IMAGE=silicon-extend-linux-e2e
mkdir -p "$ROOT/target/desktop/linux-e2e-out"
IMAGE="$IMAGE" bash "$ROOT/apps/desktop/linux-e2e/build-image.sh"
exec docker run --rm \
  --add-host=host.docker.internal:host-gateway \
  -v "$ROOT":/src:ro \
  -v silicon-extend-linux-target:/target \
  -v "$ROOT/target/desktop/linux-e2e-out":/tmp/out \
  -v silicon-extend-cargo-registry:/usr/local/cargo/registry \
  -e EXTEND_E2E_SERVICE="${EXTEND_E2E_SERVICE:-}" \
  -e RUN_TESTS="${RUN_TESTS:-1}" \
  "$IMAGE" bash /src/apps/desktop/linux-e2e/e2e.sh
