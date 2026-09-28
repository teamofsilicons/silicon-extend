# Shared by macos/build-app.sh and linux/build-package.sh (sourced, not run). Needs `die`.

# stamp_runtime <node> <staged engine> [native helper …]
# Stamps the staged runtime with its build identity (stamp-runtime.mjs) and sets STAMPED to it.
# A stamp that fails, or is killed, stops the build with why and what to do: unstamped, the
# package would keep reusing an older device engine daemon after an update.
stamp_runtime() {
  local node="$1" runtime="$2" status=0 why
  shift 2
  STAMPED="$("$node" "$(dirname "${BASH_SOURCE[0]}")/stamp-runtime.mjs" "$runtime" "$@")" || status=$?
  if ((status != 0)); then
    if ((status > 128)); then
      why="it was killed by signal $((status - 128))$(kill -l "$((status - 128))" 2>/dev/null | sed 's/^/ (SIG/; s/$/)/'), for example by running out of memory or being interrupted"
    else
      why="it exited with status $status; its reason is above"
    fi
    die "Stamping the device engine runtime in $runtime with its build identity failed: $why. Unstamped, the package would reuse an older engine daemon after an update, so nothing was packaged. Fix the cause and package again."
  fi
}

# require_fresh_dist <engine>
# Stops the build unless <engine>/dist was built from the source that is there now (see
# dist-manifest.mjs), naming what changed since, including deleted files.
require_fresh_dist() {
  local root="$1" reason
  command -v node >/dev/null || die "node isn't installed, so there is no way to check that $root/dist matches its source before packaging it. Install Node 22, or build where pnpm is, then package again."
  reason="$(node "$(dirname "${BASH_SOURCE[0]}")/dist-manifest.mjs" check "$root" 2>&1)" || die "$reason"
}
