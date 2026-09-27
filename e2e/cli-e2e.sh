#!/usr/bin/env bash
# End-to-end test of the `extend` CLI against a running Extend service (local IAM) and a fake device.
#
#   e2e/cli-e2e.sh [api_url]      (default http://127.0.0.1:8480; the service must use EXTEND_IAM_MODE=local
#                                  with c:alice, c:bob, si:chef, si:sous in team acme, and EXTEND_HONEYCOMB_SERVICE_TOKEN=hck_local_dev_token)
#
# `--json` prints the data itself on stdout (`extend iam --json` → {"app_id": ...}); a failure prints
# {"error": {...}} on stderr.
set -euo pipefail
API=${1:-http://127.0.0.1:8480}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
EXTEND="$ROOT/target/debug/extend"
FAKE="$ROOT/target/debug/examples/fake_device"
WORK=$(mktemp -d)
ENV=
lifecycle() { # action [name]: a Honeycomb lifecycle instruction for the test environment $ENV
  local op; op=$(python3 -c 'import uuid;print(uuid.uuid4())')
  curl -sS --fail-with-body -XPUT "$API/internal/honeycomb/organizations/acme/testing-environments/$ENV/operations/$op" \
    -H 'authorization: Bearer hck_local_dev_token' -H 'content-type: application/json' \
    -d "{\"operation_id\":\"$op\",\"environment_id\":\"$ENV\",\"org_id\":\"acme\",\"app_id\":\"extend\",\"environment_revision\":1,\"generation\":1,\"key_version\":1,\"action\":\"$1\",\"testing_key\":\"abcdefghijklmnopqrstuvwxyz012345\"${2:+,\"name\":\"$2\"}}"
}
# The service allows 10 test environments across all of Extend, so each run purges the one it made.
trap '{ kill $(jobs -p) || true; wait; } 2>/dev/null; [ -z "$ENV" ] || lifecycle purge >/dev/null || true; rm -rf "$WORK"' EXIT
export EXTEND_API_URL=$API EXTEND_TELEMETRY=off
unset EXTEND_TEST_SECRET EXTEND_SESSION NO_COLOR

pass=0
ok() { pass=$((pass+1)); echo "  ✓ $1"; printf '%s\n' "$1" >>"$WORK/passed"; }
die() { echo "  ✗ $1"; exit 1; }
as() { local who=$1; shift; mkdir -p "$WORK/$who"; SILICON_HOME="$WORK/$who" "$EXTEND" "$@"; }
expect_exit() { local want=$1; shift; set +e; "$@" >"$WORK/out" 2>"$WORK/err"; local got=$?; set -e; [ "$got" = "$want" ] || { cat "$WORK/out" "$WORK/err"; die "expected exit $want, got $got: $*"; }; }
jq_() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$@"; }

start_fake() { # os [secret] -> sets CODE, FAKE_LOG
  FAKE_LOG="$WORK/fake-$RANDOM.log"
  "$FAKE" "$API" "$@" >"$FAKE_LOG" 2>&1 &
  for _ in $(seq 50); do CODE=$(grep -m1 PAIRING_CODE "$FAKE_LOG" | awk '{print $2}' || true); [ -n "$CODE" ] && return; sleep 0.2; done
  cat "$FAKE_LOG"; die "fake device never showed a pairing code"
}

echo "CLI end-to-end against $API"

# ── Help, discovery and the JSON convention ──
as nobody --help | grep -q "Getting started" && ok "extend --help is a documentation tree"
as nobody device --help | grep -q "extend device pair" && ok "extend device --help lists what's under it"
[ "$(as nobody iam --json | jq_ 'd["app_id"]')" = extend ] && ok "extend iam --json gives app_id at the top level"
expect_exit 0 as nobody login status --json
[ "$(jq_ 'd["authenticated"]' <"$WORK/out")" = False ] && ok "login status --json reports authenticated:false at the top level (exit 0)"
expect_exit 2 as nobody frobnicate; grep -q "not an extend command" "$WORK/err" && ok "unknown command explains itself (exit 2)"
expect_exit 2 as nobody --json frobnicate
[ ! -s "$WORK/out" ] && [ "$(jq_ 'd["error"]["code"]' <"$WORK/err")" = unknown_command ] && ok "--json failure: {\"error\": {...}} on stderr, nothing on stdout"
expect_exit 2 as nobody boot; grep -q "doesn't relay" "$WORK/err" && ok "agent-device developer command is refused with a reason"
expect_exit 2 as nobody config home /definitely/not/here; grep -q "not a directory" "$WORK/err" && grep -q "hint:" "$WORK/err" && ok "config home rejects a non-directory, with a hint"
expect_exit 2 as nobody device ls --onlinee; grep -q "Did you mean --online?" "$WORK/err" && ok "an unknown flag is refused with the valid choices"
expect_exit 2 as nobody config set color purple; grep -q "one of auto, always, never" "$WORK/err" && ok "config values are checked"
expect_exit 2 as nobody config get bogus; grep -q "Settings: api_url" "$WORK/err" && ok "an unknown setting lists the settings"
as nobody version | grep -q "Status: current" && ok "version reads the compatibility matrix: current"

# ── Carbon pairs a device ──
as alice login c:alice >/dev/null
[ "$(as alice login status --json | jq_ 'd["member"]["id"]')" = c:alice ] && ok "Carbon logged in with an SLT; login status says authenticated"
[ "$(as alice login status --json | jq_ '(d["authenticated"], d["team"], "team_role" in d)')" = "(True, 'acme', True)" ] && ok "login status --json: authenticated, team and team_role at the top level"
start_fake linux
expect_exit 5 as alice device pair 000000 --name nope
ok "wrong pairing code → exit 5"
DEV=$(as alice device pair "$(echo "$CODE" | tr A-F a-f)" --name "CLI box" --access si:chef --json | jq_ 'd["device_id"]')
[ ${#DEV} = 8 ] && ok "paired device $DEV (lowercase code accepted)"
for _ in $(seq 30); do grep -q PAIRED "$FAKE_LOG" && break; sleep 0.2; done
sleep 0.5
as alice device ls | grep -q "CLI box" && ok "Carbon sees the device in device ls"
as alice device ls --json | jq_ 'd["next_cursor"]' | grep -q None && ok "device ls read every page (no cursor left)"
as alice device ttl "$DEV" 30 | grep -q "30 days" && ok "pair lifetime set to 30 days"
expect_exit 2 as alice device ttl "$DEV" 31
ok "31 days refused (exit 2)"
as alice device access ls "$DEV" | grep -q si:chef && ok "access list shows si:chef"

# ── Silicon uses it ──
as chef login si:chef >/dev/null
as chef device ls | grep -q "$DEV" && ok "Silicon lists the device it has access to"
as chef -v device ls 2>&1 >/dev/null | grep -q "GET /api/v1/devices: ok in" && ok "-v shows each call and its time on stderr"
as chef device show "$DEV" | grep -q "terminal" && ok "device show lists terminal for a computer"
SID=$(as chef session new "$DEV" --connect 2>/dev/null)
[ ${#SID} -ge 3 ] && ok "session $SID started and connected"
as chef --help | grep -q "connected to session $SID" && ok "--help narrows to the connected device's commands"
as chef --help | grep -q "tv-remote" && die "--help shows TV commands on a computer" || ok "--help hides commands the device can't run"
as chef tv-remote --help | grep -q "Not available on CLI box (linux) in session $SID" && ok "a device command's help says it doesn't apply here"
as chef session status | grep -q "command(s) work there now" && ok "session status refreshes the command list"
as chef snapshot -i | grep -q "snapshot -i" && ok "snapshot relayed to the device"
as chef fill @e3 "secret words" >/dev/null && ok "fill relayed"
expect_exit 1 as chef is visible 'label="Nope"'
ok "a failed assertion exits 1"
expect_exit 10 as chef tv-remote press up
ok "a command the device can't do exits 10"
as chef screenshot --ttl 2h --out "$WORK/shot.png" >"$WORK/shot.out"
[ -s "$WORK/shot.png" ] && ok "screenshot stored and saved locally with --out, through Extend's download route"
python3 - "$WORK/shot.out" <<'EOF' && ok "the file's link is printed before it is saved"
import sys
t = open(sys.argv[1]).read()
assert "screenshot screenshot.png" in t and t.index("screenshot screenshot.png") < t.index("Saved screenshot.png to"), t
EOF
FILE=$(as chef file ls --json | jq_ 'd["items"][0]["file_id"]')
(cd "$WORK" && as chef file get "$FILE" --out got.png >/dev/null) && cmp -s "$WORK/got.png" "$WORK/shot.png" && ok "file get downloads the same bytes through Extend"
as alice file get "$FILE" --out "$WORK/carbon.png" >/dev/null && cmp -s "$WORK/carbon.png" "$WORK/shot.png" && ok "the device's Carbon downloads it too"
# What scripts read (e2e/real-iam, the recording lanes): the CommandResult itself, not {"ok", "data": {"result"}}.
[ "$(as chef --json screenshot --out "$WORK/json-shot.png" | jq_ '(d["ok"], d["command"], len(d["files"]), d["saved_to"][0].endswith("json-shot.png"), "data" in d, d["warnings"])')" = "(True, 'screenshot', 1, True, False, [])" ] && [ -s "$WORK/json-shot.png" ] && ok "--json on a device command prints the CommandResult itself, with saved_to"
as chef file keep "$FILE" | grep -q permanent && ok "file kept permanently"
expect_exit 6 as chef file keep "$FILE"; grep -q "already permanent" "$WORK/err" && ok "keeping it again says it's already permanent (exit 6)"
FILE2=$(as chef file ls --json | jq_ '[f["file_id"] for f in d["items"] if not f["permanent"]][0]')
[ "$(as chef --json file keep "$FILE2" | jq_ '(d["file_id"] == sys.argv[2], d["permanent"], d["self_destruct_at"])' "$FILE2")" = "(True, True, None)" ] && ok "file keep --json prints the file itself"

# ── One Silicon at a time ──
as sous login si:sous >/dev/null
as alice device access grant "$DEV" si:sous >/dev/null
expect_exit 6 as sous session new "$DEV"
grep -q "extend request send $DEV" "$WORK/err" && ok "second Silicon gets exit 6 and the request command"
as sous request send "$DEV" --reason "Need two minutes for an OTP" | grep -q "si:chef" && ok "request delivered to si:chef"
curl -s "$API/dev/ting" | grep -q "Need two minutes for an OTP" && ok "Ting received the reason verbatim"
[ "$(as sous --json request send "$DEV" --reason "Need two minutes for an OTP" | jq_ '(d["to"], bool(d["request_id"]), d["delivery"])')" = "('si:chef', True, 'delivered')" ] && ok "request send --json prints the request itself (a repeat within 60 s returns it)"

# ── Takeover ──
as chef takeover --reason "Please approve the admin prompt" >/dev/null
expect_exit 8 as chef snapshot
ok "commands wait during a takeover (exit 8)"
as chef takeover release >/dev/null && as chef snapshot >/dev/null && ok "takeover released; commands run again"

# ── Carbon sees and stops it ──
as alice device activity "$DEV" | grep -q "redacted 12 chars" && ok "activity log redacts typed text"
as alice device requests "$DEV" | grep -q "OTP" && ok "Carbon sees the request and reason"
as alice device stop "$DEV" | grep -q "Stopped si:chef" && ok "Carbon stops the session"
expect_exit 6 as chef snapshot
grep -q "stopped" "$WORK/err" && ok "Silicon learns the session ended and why"
as chef --help | grep -q "connected to session" && die "--help still says connected after the session ended" || ok "--help forgets the ended session"

# ── Report and version ──
as chef report "fill loses the last character" --pr https://github.com/teamofsilicons/silicon-extend/pull/1 | grep -q "sent" && ok "bug report sent"
as chef version | grep -q "API v1" && ok "version shows the negotiated API"

# ── Remove ──
expect_exit 2 as alice device rm "$DEV"
grep -q -- "--yes" "$WORK/err" && ok "rm without --yes explains and does nothing"
as alice device rm "$DEV" --yes | grep -q Removed && ok "device removed"
sleep 0.5; grep -q "UNPAIRED device_removed" "$FAKE_LOG" && ok "device was told it's unpaired"

# ── State directory ──
as mover login si:chef >/dev/null
as mover config set telemetry off >/dev/null
mkdir -p "$WORK/mover-new"
as mover config home "$WORK/mover-new" | grep -q "Moved the login for si:chef" && ok "config home moves the state"
[ "$(as mover login status --json | jq_ 'd["authenticated"]')" = True ] && ok "the login survived the move"
[ "$(as mover config get telemetry)" = off ] && ok "settings survived the move"
[ ! -e "$WORK/mover/.extend/auth.json" ] && ok "nothing is left behind"

# ── Test environment ──
ENV=$(python3 -c 'import uuid;print(uuid.uuid4())')
SECRET=ask_$(python3 -c 'import secrets,base64;print(base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("="))')
expect_exit 11 as alice --test "$ENV" device ls
tail -1 "$WORK/err" | grep -q "^\[test environment: unknown ($ENV) as not signed in; nothing ran\]$" && ok "--test names the environment on stderr even when nothing ran"
lifecycle prepare cli-e2e >"$WORK/prepare" || { cat "$WORK/prepare"; echo; die "couldn't prepare a test environment (the service's reason is above)"; }
curl -sf -XPOST "$API/dev/iam/test-apps" -H 'content-type: application/json' -d "{\"type\":\"t\",\"data\":{\"secret\":\"$SECRET\",\"environment_id\":\"$ENV\"}}"
OTHER=$(python3 -c 'import uuid;print(uuid.uuid4())')
set +e; printf %s "$SECRET" | as alice config test add "$OTHER" >"$WORK/out" 2>"$WORK/err"; got=$?; set -e
[ "$got" = 11 ] && grep -q "belongs to test environment \"cli-e2e\" ($ENV), not $OTHER" "$WORK/err" && ok "config test add checks the secret belongs to the id"
printf %s "$SECRET" | as alice config test add "$ENV" | grep -q cli-e2e && ok "test environment added from stdin"
expect_exit 11 as alice env show
ok "test-only command without --test exits 11"
as alice --test "$ENV" login c:alice >/dev/null 2>"$WORK/err"
grep -q "test environment: cli-e2e" "$WORK/err" && ok "--test prints the environment on stderr"
as alice --test "$ENV" env show 2>/dev/null | grep -q "0 of 5" && ok "env show: 0 of 5 devices"
start_fake linux "$SECRET"
as alice --test "$ENV" device pair "$CODE" --name "Isolated box $ENV" >/dev/null 2>&1 && ok "device paired into the test environment"
as alice device ls | grep -q "Isolated box" && die "test device visible in production" || ok "production doesn't see the test device"
as alice --test "$ENV" device ls --json 2>/dev/null | grep -q "Isolated box" && ok "test environment sees it"
as alice --test "$ENV" --json login status 2>/dev/null | grep -q '"authenticated": *true' && ok "separate login state per environment"
EXTEND_TEST_SECRET=$SECRET as bob --test "$ENV" login c:bob 2>"$WORK/err" >/dev/null && tail -1 "$WORK/err" | grep -q "cli-e2e ($ENV) as c:bob" && ok "EXTEND_TEST_SECRET selects the environment without config test add"
EXTEND_TEST_SECRET=$SECRET as bob --test "$ENV" device ls 2>/dev/null | grep -q "No devices" && ok "…and keeps that environment's login"
grep -rq "$SECRET" "$WORK/bob" && die "EXTEND_TEST_SECRET was written to disk" || ok "…without writing the secret to disk"

as alice logout >/dev/null && as alice login status | grep -q "^Not signed in" && ok "logout clears the login"

# A check that fails prints nothing (its `&& ok` is skipped and `set -e` doesn't fire inside `&&`),
# so count them against every check in this file and name the ones that didn't pass.
want=$(grep -o '\bok "' "${BASH_SOURCE[0]}" | wc -l | tr -d ' ')
if [ "$pass" != "$want" ]; then
  touch "$WORK/passed"
  python3 - "${BASH_SOURCE[0]}" "$WORK/passed" <<'EOF'
import re, sys
script, passed = open(sys.argv[1]).read(), open(sys.argv[2]).read().splitlines()
for name in re.findall(r'\bok "((?:[^"\\]|\\.)*)"', script):
    pattern = re.sub(r'\\\$[A-Za-z_]+', '.*', re.escape(name.replace('\\"', '"')))
    if not any(re.fullmatch(pattern, p) for p in passed):
        print("  ✗ " + name)
EOF
  die "$pass of $want checks passed; each one marked ✗ above failed (run its command by hand to see why)"
fi
echo "All $pass checks passed."
