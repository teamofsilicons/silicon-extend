#!/usr/bin/env bash
# Runs inside the silicon-extend-linux-e2e container (see run.sh). Builds extend-agent, starts a
# real X11 desktop with the AT-SPI bus and GNOME Calculator, and drives it through the agent's own
# Linux driver (agent-device underneath): probe, open, snapshot, click, type, get, screenshot,
# clipboard, apps, terminal, close. With EXTEND_E2E_SERVICE set, it also runs the full agent
# against that Extend service: pair, session, commands with uploads, stop, remove.
set -euo pipefail

log() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() { printf '\n\033[31mFAILED: %s\033[0m\n' "$*"; exit 1; }

export CARGO_TARGET_DIR=/target
export SILICON_HOME=/tmp/home
export EXTEND_AGENT_CREDENTIAL_STORE=file
mkdir -p "$SILICON_HOME" /tmp/out
rm -rf /tmp/out/*

log "Copy the source (read-only mount) and agent-device's runtime files"
mkdir -p /work /opt/agent-device
tar -C /src --exclude=./target --exclude=node_modules --exclude=./.git --exclude=./vendor/agent-device -cf - . | tar -C /work -xf -
# agent-device's dist is self-contained: bin/, dist/, linux/atspi-dump.py and package.json are all it needs.
tar -C /src/vendor/agent-device -cf - bin dist linux package.json | tar -C /opt/agent-device -xf -
export EXTEND_AGENT_DEVICE=/opt/agent-device/bin/agent-device.mjs
node "$EXTEND_AGENT_DEVICE" --version

log "Build extend-agent (with the tray, to prove the Linux UI compiles)"
cd /work
cargo build -p extend-agent 2>&1 | tail -3
BA=/target/debug/extend-agent
"$BA" --version

if [[ "${RUN_TESTS:-1}" == "1" ]]; then
  log "Unit and integration tests on Linux"
  cargo test -p extend-agent > /tmp/cargo-test.log 2>&1 || { grep -E "FAILED|panicked" /tmp/cargo-test.log; fail "cargo test"; }
  grep -E "^test result" /tmp/cargo-test.log
fi

log "Headless probe (no DISPLAY): a server gets only the terminal"
# Its own home: agent-device's daemon keeps the environment of the run that started it, so the
# desktop runs below must not share a daemon started without a display.
env -u DISPLAY SILICON_HOME=/tmp/server-home "$BA" probe --json | jq -c '{capabilities, setup: .setup.state, missing: [.missing[] | select(.capability == "apps.launch")]}'
[[ "$(env -u DISPLAY SILICON_HOME=/tmp/server-home "$BA" probe --json | jq -c .capabilities)" == '["terminal"]' ]] || fail "headless capabilities"
env -u DISPLAY SILICON_HOME=/tmp/server-home "$BA" probe --json | jq -e '[.missing[] | select(.capability == "replay" and (.reason | test("only use the terminal")))] | length == 1' >/dev/null || fail "headless: replay's reason"

log "Start the desktop: Xvfb, D-Bus, AT-SPI, openbox, GNOME Calculator"
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
eval "$(dbus-launch --sh-syntax)"
export DBUS_SESSION_BUS_ADDRESS
/usr/libexec/at-spi-bus-launcher --launch-immediately >/tmp/atspi.log 2>&1 &
sleep 1
openbox >/tmp/openbox.log 2>&1 &
sleep 1
export GTK_A11Y=atspi NO_AT_BRIDGE=0  # GTK's AT-SPI bridge switch, not a product name
gnome-calculator >/tmp/calculator.log 2>&1 &
for _ in $(seq 100); do wmctrl -l 2>/dev/null | grep -qi calculator && break; sleep 0.2; done
wmctrl -l
# GTK4 on X11 reports its window at (0,0) to AT-SPI whatever the window manager chose, so
# agent-device's screen coordinates are only right for a window at the top-left corner.
wmctrl -r Calculator -e 0,0,0,-1,-1
sleep 0.5
xwininfo -name Calculator | grep -E "Absolute upper-left|Width|Height"

log "Probe with a screen"
"$BA" probe
"$BA" probe --json | jq -c .capabilities

run() { # run <step> <command...>: prints the result, keeps it in /tmp/out/<step>.json
  local step="$1"; shift
  "$BA" exec --session e2e --timeout-ms 60000 --out "/tmp/out/$step" "$@" > "/tmp/out/$step.json" || true
  jq -c '{ok, text: ((.text // "") | .[0:300]), error, files: [.files[] | {name, kind, size_bytes}]}' "/tmp/out/$step.json"
}
ok() { [[ "$(jq -r .ok "/tmp/out/$1.json")" == "true" ]] || fail "$1: $(jq -c .error "/tmp/out/$1.json")"; }

log "open gnome-calculator"
run 01-open open gnome-calculator; ok 01-open
sleep 1

log "snapshot -i"
run 02-snapshot snapshot -i; ok 02-snapshot
jq -r .text /tmp/out/02-snapshot.json | head -40
ref_for() { # ref_for <label>: the @ref of the first node with exactly that label
  jq -r --arg l "$1" '[.output.nodes[] | select((.label // "") == $l)][0].ref // empty' /tmp/out/02-snapshot.json
}
SEVEN=$(ref_for 7); PLUS=$(ref_for "+"); FIVE=$(ref_for 5); EQUALS=$(ref_for "=")
echo "refs: 7=@$SEVEN +=@$PLUS 5=@$FIVE ==@$EQUALS"
[[ -n "$SEVEN" && -n "$PLUS" && -n "$FIVE" && -n "$EQUALS" ]] || fail "calculator buttons not found in the snapshot"

log "click @ref 7, then selectors + 5 = (an action expires earlier refs, as in agent-device)"
run 03-click-ref click "@$SEVEN"; ok 03-click-ref
i=0
for l in "+" "5" "="; do i=$((i+1)); run "03-click-$i" click "role=\"button\" label=\"$l\""; ok "03-click-$i"; done
sleep 0.5
run 04-after snapshot; ok 04-after
display() { jq -r .text "/tmp/out/$1.json" | grep 'text-field' | head -2; }
display 04-after
display 04-after | grep -q '"12"' || fail "the calculator doesn't show 12 after 7 + 5 ="

log "type 3*4 and Enter"
run 05-type type "3*4"; ok 05-type
xdotool key Return
sleep 0.5
run 06-after snapshot; ok 06-after
display 06-after
display 06-after | grep -q '"12"' || fail "no 12 after typing 3*4"

log "screenshot"
run 07-screenshot screenshot calc; ok 07-screenshot
[[ -s /tmp/out/07-screenshot/calc.png ]] || fail "no screenshot file"
file_type=$(head -c 8 /tmp/out/07-screenshot/calc.png | od -An -c | tr -d ' ')
echo "png header: $file_type"

log "clipboard write / read"
run 08-clip-write clipboard write "extend-e2e"; ok 08-clip-write
run 09-clip-read clipboard read; ok 09-clip-read
jq -r .text /tmp/out/09-clip-read.json | grep -q "extend-e2e" || fail "clipboard read"

log "apps / appstate"
# agent-device has no app inventory on Linux; the probe reports apps.list as missing, and the
# command answers unsupported_on_device.
run 10-apps apps
[[ "$(jq -r .error.code /tmp/out/10-apps.json)" == "unsupported_on_device" ]] || jq -e .ok /tmp/out/10-apps.json >/dev/null || fail "apps"
run 11-appstate appstate
[[ "$(jq -r .error.code /tmp/out/11-appstate.json)" == "unsupported_on_device" ]] || jq -e .ok /tmp/out/11-appstate.json >/dev/null || fail "appstate"

log "terminal"
run 12-terminal terminal run "echo hello-from-linux; uname -m; echo warn >&2" --env E2E=1; ok 12-terminal
jq -c .output /tmp/out/12-terminal.json
run 13-terminal-fail terminal run "exit 5"
[[ "$(jq -r .error.code /tmp/out/13-terminal-fail.json)" == "command_failed" && "$(jq -r .output.exit_code /tmp/out/13-terminal-fail.json)" == "5" ]] || fail "exit 5"

log "reserved flag is refused"
run 14-reserved snapshot --platform ios
[[ "$(jq -r .error.code /tmp/out/14-reserved.json)" == "invalid_args" ]] || fail "reserved flag"

log "close"
run 15-close close; ok 15-close

if [[ -n "${EXTEND_E2E_SERVICE:-}" ]]; then
  log "Full agent against $EXTEND_E2E_SERVICE"
  S="$EXTEND_E2E_SERVICE"
  curl -fsS "$S/api/version" >/dev/null || fail "service unreachable at $S"
  "$BA" run --headless --service-url "$S" >/tmp/agent.out 2>/tmp/agent.err &
  AGENT=$!
  for _ in $(seq 100); do jq -e .pairing.code "$SILICON_HOME/.extend-agent/status.json" >/dev/null 2>&1 && break; sleep 0.1; done
  CODE=$(jq -r .pairing.code "$SILICON_HOME/.extend-agent/status.json")
  echo "pairing code $CODE"
  login() { curl -fsS -X POST "$S/api/v1/auth/login" -H 'content-type: application/json' -d "{\"type\":\"login\",\"data\":{\"slt\":\"$1\"}}" | jq -r .data.access_token; }
  CT=$(login c:alice); ST=$(login si:chef)
  H=(-H "x-org-id: acme" -H 'content-type: application/json')
  DEV=$(curl -fsS -X POST "$S/api/v1/pairings" -H "authorization: Bearer $CT" "${H[@]}" \
        -d "{\"type\":\"pairing\",\"data\":{\"pairing_code\":\"$CODE\",\"name\":\"Linux e2e\",\"silicon_ids\":[\"si:chef\"]}}" | jq -r .data.device_id)
  echo "paired as $DEV"
  for _ in $(seq 100); do [[ "$(jq -r .phase "$SILICON_HOME/.extend-agent/status.json")" == "online" ]] && break; sleep 0.1; done
  # The service marks the device ready once it has processed hello.
  for _ in $(seq 100); do
    [[ "$(curl -fsS "$S/api/v1/devices/$DEV" -H "authorization: Bearer $CT" -H "x-org-id: acme" | jq -r .data.state)" == "ready" ]] && break; sleep 0.1
  done
  curl -fsS "$S/api/v1/devices/$DEV" -H "authorization: Bearer $CT" "${H[@]}" | jq -c '.data | {online, state, capabilities}'
  SID=$(curl -fsS -X POST "$S/api/v1/sessions" -H "authorization: Bearer $ST" "${H[@]}" -d "{\"type\":\"session\",\"data\":{\"device_id\":\"$DEV\"}}" | jq -r .data.session_id)
  echo "session $SID"
  cmd() { curl -sS -m 120 -X POST "$S/api/v1/sessions/$SID/commands" -H "authorization: Bearer $ST" "${H[@]}" -d "$1"; }
  cmd '{"type":"command","data":{"command":"open","args":["gnome-calculator"]}}' | jq -c '.data | {ok, text, error}'
  cmd '{"type":"command","data":{"command":"snapshot","args":["-i"]}}' | jq -r '.data.text' | head -12
  cmd '{"type":"command","data":{"command":"click","args":["role=\"button\" label=\"7\""]}}' | jq -c '.data | {ok, text, error}'
  cmd '{"type":"command","data":{"command":"screenshot","args":["linux"]}}' | jq -c '.data | {ok, text, files: [.files[] | {name, kind, size_bytes, url}], error}'
  cmd '{"type":"command","data":{"command":"terminal","args":["run","uname -sm"]}}' | jq -c '.data | {ok, output}'
  grep -q "si:chef is using this computer" /tmp/agent.out || fail "in-use indicator"
  "$BA" --service-url "$S" stop
  sleep 1
  curl -fsS "$S/api/v1/sessions/$SID" -H "authorization: Bearer $ST" "${H[@]}" | jq -c '.data | {state, end_reason}'
  curl -fsS -o /dev/null -w "remove device: %{http_code}\n" -X DELETE "$S/api/v1/devices/$DEV" -H "authorization: Bearer $CT" "${H[@]}" -d "{\"type\":\"confirmation\",\"data\":{\"confirm\":\"$DEV\"}}"
  sleep 2
  cat /tmp/agent.out
  [[ ! -e "$SILICON_HOME/.extend-agent/credential.json" ]] || fail "credential kept after unpair"
  kill -TERM "$AGENT"; wait "$AGENT" || true
fi

log "Linux e2e passed"
