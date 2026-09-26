#!/usr/bin/env bash
# Runs every automated suite that can run on this machine, in order, and prints a summary.
#
#   e2e/run-all.sh            # needs the Postgres container on 127.0.0.1:5440 (see docs/development.md)
#
# Suites that need hardware or tools this machine lacks are reported as skipped, with why.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
LOG=e2e/.state/run-all; mkdir -p "$LOG"
declare -a SUMMARY
run() { # name, command...
  local name=$1; shift
  printf '▶ %s … ' "$name"
  if "$@" >"$LOG/$name.log" 2>&1; then SUMMARY+=("✓ $name"); echo ok; else SUMMARY+=("✗ $name (see $LOG/$name.log)"); echo FAILED; fi
}
skip() { SUMMARY+=("– $1: skipped, $2"); echo "– $1 skipped: $2"; }

run rust-unit-and-service-e2e cargo test --workspace --exclude bridge-agent
run bridge-agent-tests cargo test -p bridge-agent
run clippy cargo clippy --workspace --all-targets -- -D warnings

# The CLI suite needs a running service with the local stand-ins.
cargo build -q -p bridge-service -p bridge-cli --example fake_device
if curl -fsS http://127.0.0.1:8480/ready >/dev/null 2>&1; then
  run cli-e2e bash e2e/cli-e2e.sh
else
  (set -a; . e2e/dev.env; set +a; ./target/debug/bridge-service >"$LOG/service.log" 2>&1) & SVC=$!
  sleep 3
  run cli-e2e bash e2e/cli-e2e.sh
  kill $SVC 2>/dev/null
fi

if command -v pnpm >/dev/null; then
  run web-unit bash -c "cd web && pnpm test"
  run web-e2e-mock bash -c "cd web && pnpm test:e2e"
  run web-build bash -c "cd web && pnpm build"
else
  skip web "pnpm not installed"
fi

if [ -x apps/android/gradlew ]; then
  # No system Java on the build Mac: use Homebrew's JDK 17 when JAVA_HOME isn't set (apps/android/README.md).
  : "${JAVA_HOME:=$( [ -d /opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home ] && echo /opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home || /usr/libexec/java_home 2>/dev/null )}"
  export JAVA_HOME
  run android-unit bash -c "cd apps/android && ./gradlew --quiet testDebugUnitTest"
else
  skip android "apps/android not present"
fi

if command -v npx >/dev/null; then
  run contract-lint npx -y @redocly/cli@2.49.0 lint understanding/api.yaml --skip-rule no-path-trailing-slash
fi

echo; echo "Summary:"; printf '  %s\n' "${SUMMARY[@]}"
printf '%s\n' "${SUMMARY[@]}" | grep -q '^✗' && exit 1 || exit 0
