#!/usr/bin/env bash
# End-to-end test of the `extend` CLI against a running Extend service and a fake device.
#
#   e2e/cli-e2e.sh [api_url]      (default http://127.0.0.1:8480)
#
# The service must run with its local stand-ins, as e2e/dev.env sets them up
# (EXTEND_ACCOUNTS_MODE=local, EXTEND_FILES_MODE=local, EXTEND_TING_MODE=local). Every account
# signs in the way it signs in to Silicon Accounts: the stand-in mints a short-lived token
# (POST /dev/accounts/slt, as `silicon-accounts login --app extend -q` would) and
# `extend login --slt-stdin` exchanges it, with ACCOUNTS_URL pointing at the stand-in. The accounts:
# the Carbons c:alice and c:bob; si:chef and si:sous, whose custodian is c:alice; si:scout, whose
# custodian is c:bob.
#
# `--json` prints the data itself on stdout (`extend accounts --json` → {"app_id": ...}); a failure
# prints {"error": {...}} on stderr.
set -euo pipefail
API=${1:-http://127.0.0.1:8480}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
# $ROOT/target/debug/extend and $ROOT/target/debug/examples/fake_device, from `cargo build -p silicon-extend-cli` and
# `cargo build -p extend-service --example fake_device`; under $CARGO_TARGET_DIR when that is set.
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
EXTEND="$TARGET/debug/extend"
FAKE="$TARGET/debug/examples/fake_device"
WORK=$(mktemp -d)
trap '{ kill $(jobs -p) || true; wait; } 2>/dev/null; rm -rf "$WORK"' EXIT
export EXTEND_API_URL=$API ACCOUNTS_URL=$API/dev/accounts EXTEND_TELEMETRY=off
unset EXTEND_TEST_SECRET EXTEND_SESSION NO_COLOR

pass=0
ok() { pass=$((pass+1)); echo "  ✓ $1"; printf '%s\n' "$1" >>"$WORK/passed"; }
die() { echo "  ✗ $1"; exit 1; }
as() { local who=$1; shift; mkdir -p "$WORK/$who"; SILICON_HOME="$WORK/$who" "$EXTEND" "$@"; }
expect_exit() { local want=$1; shift; set +e; "$@" >"$WORK/out" 2>"$WORK/err"; local got=$?; set -e; [ "$got" = "$want" ] || { cat "$WORK/out" "$WORK/err"; die "expected exit $want, got $got: $*"; }; }
jq_() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$@"; }
# A short-lived token for Extend from the local Silicon Accounts stand-in: id [custodian].
slt() { curl -sS --fail-with-body -XPOST "$API/dev/accounts/slt" -H 'content-type: application/json' \
  -d "{\"type\":\"slt\",\"data\":{\"id\":\"$1\"${2:+,\"custodian\":\"$2\"}}}" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["slt"])'; } # the service's envelope, not the CLI's output
signin() { slt "$2" "${3:-}" | as "$1" login --slt-stdin >/dev/null; } # who id [custodian]

start_fake() { # os [secret] -> sets CODE, FAKE_LOG. FAKE_ENV="K=V ..." sets the fake's options (examples/fake_device.rs).
  FAKE_LOG="$WORK/fake-$RANDOM.log"
  env ${FAKE_ENV:-} "$FAKE" "$API" "$@" >"$FAKE_LOG" 2>&1 &
  for _ in $(seq 50); do CODE=$(grep -m1 PAIRING_CODE "$FAKE_LOG" | awk '{print $2}' || true); [ -n "$CODE" ] && return; sleep 0.2; done
  cat "$FAKE_LOG"; die "fake device never showed a pairing code"
}

echo "CLI end-to-end against $API"

# ── Help, discovery and the JSON convention ──
as nobody --help | grep -q "Getting started" && ok "extend --help is a documentation tree"
as nobody --help | grep -q "silicon-apps install extend" && ok "--help says how to install with Silicon Apps"
as nobody device --help | grep -q "extend device pair" && ok "extend device --help lists what's under it"
[ "$(as nobody accounts --json | jq_ '(d["app_id"], d["accounts_url"], d["install"])')" = "('extend', '$API/dev/accounts', 'silicon-apps install extend')" ] && ok "accounts --json names the app, the Silicon Accounts it signs in at and the install command"
[ "$(as nobody iam --json)" = "$(as nobody accounts --json)" ] && ok "the hidden iam --json prints the same object (the Silicon runtime still runs it)"
expect_exit 0 as nobody login status --json
[ "$(cat "$WORK/out")" = '{"authenticated":false}' ] && ok "login status --json is exactly {\"authenticated\":false} when signed out (exit 0)"
expect_exit 2 as nobody frobnicate; grep -q "not an extend command" "$WORK/err" && ok "unknown command explains itself (exit 2)"
expect_exit 2 as nobody --json frobnicate
[ ! -s "$WORK/out" ] && [ "$(jq_ 'd["error"]["code"]' <"$WORK/err")" = unknown_command ] && ok "--json failure: {\"error\": {...}} on stderr, nothing on stdout"
expect_exit 2 as nobody boot; grep -q "doesn't relay" "$WORK/err" && ok "a device engine developer command is refused with a reason"
expect_exit 2 as nobody config home /definitely/not/here; grep -q "not a directory" "$WORK/err" && grep -q "hint:" "$WORK/err" && ok "config home rejects a non-directory, with a hint"
expect_exit 2 as nobody device ls --onlinee; grep -q "Did you mean --online?" "$WORK/err" && ok "an unknown flag is refused with the valid choices"
expect_exit 2 as nobody config set color purple; grep -q "one of auto, always, never" "$WORK/err" && ok "config values are checked"
expect_exit 2 as nobody config get bogus; grep -q "Settings: api_url, accounts_url" "$WORK/err" && ok "an unknown setting lists the settings"
as nobody version | grep -q "Status: current" && ok "version reads the compatibility matrix: current"
expect_exit 2 as nobody --team acme device ls; grep -q -- "--team" "$WORK/err" && ok "a removed flag (--team) exits 2 and says what replaced it"
expect_exit 2 as nobody team ls; ok "a removed command (team) exits 2"

# ── Signing in ──
signin alice c:alice
[ "$(as alice login status --json | jq_ '(d["authenticated"], d["id"], d["kind"], d["method"], d["verified"])')" = "(True, 'c:alice', 'carbon', 'slt', True)" ] && ok "Carbon signed in with a short-lived token; login status --json says who, checked with Extend"
[ "$(stat -f %Lp "$WORK/alice/.extend/auth.json" 2>/dev/null || stat -c %a "$WORK/alice/.extend/auth.json")" = 600 ] && ok "the saved sign-in is readable by its owner only (0600)"
SLT=$(slt si:chef c:alice)
printf %s "$SLT" | as chef login --slt-stdin | grep -q "a Silicon looked after by c:alice" && ok "Silicon signed in with --slt-stdin; the CLI names its custodian"
grep -rq "$SLT" "$WORK/chef" && die "the short-lived token was written to disk" || ok "the short-lived token is never written to disk"
expect_exit 3 as other login --slt "$SLT"; grep -q "already used" "$WORK/err" && ok "a short-lived token works once (exit 3, says why)"
as runtime login "$(slt si:chef c:alice)" >/dev/null && [ "$(as runtime login status --json | jq_ 'd["id"]')" = si:chef ] && ok "extend login <token> (positional, the Silicon runtime's form) still signs in"

# ── Carbon pairs a device ──
start_fake linux
expect_exit 5 as alice device pair 000000 --name nope
ok "wrong pairing code → exit 5"
DEV=$(as alice device pair "$(echo "$CODE" | tr A-F a-f)" --name "CLI box" --access si:chef --json | jq_ 'd["device_id"]')
[ ${#DEV} = 8 ] && ok "paired device $DEV (lowercase code accepted)"
for _ in $(seq 30); do grep -q PAIRED "$FAKE_LOG" && break; sleep 0.2; done
sleep 0.5
as alice device ls | grep -q "CLI box" && ok "Carbon sees the device in device ls"
as alice device ls --json | jq_ 'd["next_cursor"]' | grep -q None && ok "device ls read every page (no cursor left)"
expect_exit 2 as alice device pair 123456 --name x --visibility team; grep -q -- "--visibility" "$WORK/err" && ok "--visibility is gone: a device is always its Carbon's own (exit 2)"
as alice device ttl "$DEV" 30 | grep -q "30 days" && ok "pair lifetime set to 30 days"
expect_exit 2 as alice device ttl "$DEV" 31
ok "31 days refused (exit 2)"
as alice device access ls "$DEV" | grep -q si:chef && ok "access list shows si:chef"

# ── Silicon uses it ──
as chef device ls | grep -q "$DEV" && ok "Silicon lists the device it has access to"
as chef -v device ls 2>&1 >/dev/null | grep -q "GET /api/v2/devices: ok in" && ok "-v shows each call and its time on stderr"
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
python3 - "$WORK/shot.out" <<'PY' && ok "the file's link is printed before it is saved"
import sys
t = open(sys.argv[1]).read()
assert "screenshot screenshot.png" in t and t.index("screenshot screenshot.png") < t.index("Saved screenshot.png to"), t
PY
FILE=$(as chef file ls --json | jq_ 'd["items"][0]["file_id"]')
(cd "$WORK" && as chef file get "$FILE" --out got.png >/dev/null) && cmp -s "$WORK/got.png" "$WORK/shot.png" && ok "file get downloads the same bytes through Extend"
as alice file get "$FILE" --out "$WORK/carbon.png" >/dev/null && cmp -s "$WORK/carbon.png" "$WORK/shot.png" && ok "the device's Carbon downloads it too"
# What scripts read (the recording lanes): the CommandResult itself, not {"ok", "data": {"result"}}.
[ "$(as chef --json screenshot --out "$WORK/json-shot.png" | jq_ '(d["ok"], d["command"], len(d["files"]), d["saved_to"][0].endswith("json-shot.png"), "data" in d, d["warnings"])')" = "(True, 'screenshot', 1, True, False, [])" ] && [ -s "$WORK/json-shot.png" ] && ok "--json on a device command prints the CommandResult itself, with saved_to"
as chef file keep "$FILE" | grep -q permanent && ok "file kept permanently"
expect_exit 6 as chef file keep "$FILE"; grep -q "already permanent" "$WORK/err" && ok "keeping it again says it's already permanent (exit 6)"
FILE2=$(as chef file ls --json | jq_ '[f["file_id"] for f in d["items"] if not f["permanent"]][0]')
[ "$(as chef --json file keep "$FILE2" | jq_ '(d["file_id"] == sys.argv[2], d["permanent"], d["self_destruct_at"])' "$FILE2")" = "(True, True, None)" ] && ok "file keep --json prints the file itself"

# ── One Silicon at a time ──
signin sous si:sous c:alice
as alice device access grant "$DEV" si:sous >/dev/null
expect_exit 6 as sous session new "$DEV"
grep -q "extend request send $DEV" "$WORK/err" && ok "second Silicon gets exit 6 and the request command"
as sous request send "$DEV" --reason "Need two minutes for an OTP" | grep -q "si:chef" && ok "request delivered to si:chef (same Carbon's access, same custodian)"
curl -s "$API/dev/ting" | grep -q "Need two minutes for an OTP" && ok "Ting received the reason verbatim"
[ "$(as sous --json request send "$DEV" --reason "Need two minutes for an OTP" | jq_ '(d["to"], bool(d["request_id"]), d["delivery"])')" = "('si:chef', True, 'delivered')" ] && ok "request send --json prints the request itself (a repeat within 60 s returns it)"

# ── Takeover ──
as chef takeover --reason "Please approve the admin prompt" >/dev/null
expect_exit 8 as chef snapshot
ok "commands wait during a takeover (exit 8)"
as chef takeover release >/dev/null && as chef snapshot >/dev/null && ok "takeover released; commands run again"

# ── The Carbon sees it and stops it ──
as alice device activity "$DEV" | grep -q "redacted 12 chars" && ok "activity log redacts typed text"
as alice device requests "$DEV" | grep -q "OTP" && ok "Carbon sees the request and reason"
as alice silicon ls | grep -q "si:chef" && as alice silicon ls | grep -q "si:sous" && ok "silicon ls: the Silicons the Carbon looks after"
as alice silicon show si:chef | grep -q "$DEV" && ok "silicon show lists the devices a Silicon it looks after can use"
as alice session ls --silicon si:chef --state active | grep -q "$SID" && ok "the custodian sees its Silicon's running session (--silicon)"
as alice device stop "$DEV" | grep -q "Stopped si:chef" && ok "Carbon stops the session"
expect_exit 6 as chef snapshot
grep -q "stopped" "$WORK/err" && ok "Silicon learns the session ended and why"
as chef --help | grep -q "connected to session" && die "--help still says connected after the session ended" || ok "--help forgets the ended session"

# ── Report and version ──
# The dev service has no Postmark token, so a report is stored and the CLI must not claim it was emailed.
out=$(as chef report "fill loses the last character" --pr https://github.com/teamofsilicons/silicon-extend/pull/1)
grep -q "saved, but not emailed" <<<"$out" && ! grep -q "sent to" <<<"$out" && ok "bug report stored, and not called sent"
as chef report "the keyboard hides the send button" | grep -q "open a pull request at https://github.com/teamofsilicons/silicon-extend" && ok "a report without --pr invites a pull request"
as chef version | grep -q "API v2" && ok "version shows the negotiated API"

# ── Remove ──
expect_exit 2 as alice device rm "$DEV"
grep -q -- "--yes" "$WORK/err" && ok "rm without --yes explains and does nothing"
as alice device rm "$DEV" --yes | grep -q Removed && ok "device removed"
for _ in $(seq 30); do grep -q "UNPAIRED device_removed" "$FAKE_LOG" && break; sleep 0.2; done
grep -q "UNPAIRED device_removed" "$FAKE_LOG" && ok "removing a device unpairs it from the Carbon's account"
as chef device ls --json | grep -q "$DEV" && die "a removed device is still listed for its Silicon" || ok "the Silicon no longer sees it"

# ── A Carbon's own devices, access by id, and Silicons with different custodians ──
signin bob c:bob
signin scout si:scout c:bob
# A 1.1 app that reports itself not awake (standby), and shows a code for another Carbon.
FAKE_ENV="FAKE_APP_VERSION=1.1.0 FAKE_AWAKE=false FAKE_SLEEP_STATE=standby FAKE_PAIR_ANOTHER=1" start_fake android_tv
TV_LOG=$FAKE_LOG
TV=$(as alice device pair "$CODE" --name "Family TV" --json | jq_ 'd["device_id"]')
[ ${#TV} = 8 ] && ok "a 1.1 device paired: $TV"
for _ in $(seq 30); do grep -q PAIRED "$TV_LOG" && break; sleep 0.2; done; sleep 0.5
[ "$(as alice device show "$TV" --json | jq_ 'd["visibility"]')" = personal ] && ok "a new pair is its Carbon's own (personal)"
expect_exit 5 as bob device show "$TV"; ok "another Carbon can't open it, even by id"
expect_exit 5 as scout device show "$TV"; ok "a Silicon without access can't open it"
as alice device access grant "$TV" si:sous si:chef >/dev/null
as alice device access grant "$TV" si:scout | grep -q "si:scout" && ok "a Carbon gives access to any Silicon by id, another Carbon's included"
as scout device show "$TV" >/dev/null; ok "the Silicon given access sees the device"
expect_exit 2 as alice device access grant "$TV" si:nobody-has-this-id; grep -q "si:nobody-has-this-id" "$WORK/err" && ok "an unknown Silicon id is refused with the id"
as alice device ls | grep "$TV" | grep -q "no (standby)" && ok "device ls says the device isn't awake, and why"

# Not awake is no gate; the note says what won't work and how to ask.
SID=$(as sous session new "$TV" 2>"$WORK/err")
[ ${#SID} -ge 3 ] && grep -q "Family TV isn't awake (standby)" "$WORK/err" && grep -q "Ask: extend device wake $TV" "$WORK/err" && ok "session new on a device that isn't awake starts, with the wake note"

# Asking for it: a Silicon with the same custodian, given access by the same Carbon, is told who
# has it; a Silicon of another custodian only that it is in use, and the Carbon gets the request.
as chef request send "$TV" --reason "Two minutes for an OTP" | grep -q "Sent to si:sous (using $TV in session $SID)" && ok "request send from a Silicon with the same custodian names the Silicon using it"
as scout request send "$TV" --reason "Need the TV for the demo" | grep -q "Sent to the Carbon who gave access to the Silicon using it; it's in use by a Silicon you can't see." && ok "request send from another custodian's Silicon goes to the Carbon, naming no one"
as alice device requests "$TV" | grep "si:scout" | grep -q " you " && ok "the Carbon sees the request routed to them"
expect_exit 6 as scout device wake "$TV" --reason "Need it"; ok "wake while another Silicon uses it: exit 6"

# Another Carbon pairs the same physical device and can't stop the first Carbon's session.
for _ in $(seq 50); do CODE2=$(grep -m1 '^PAIRING_CODE_2 ' "$TV_LOG" | awk '{print $2}' || true); [ -n "$CODE2" ] && break; sleep 0.2; done
# Needs a fake device that shows a code for another Carbon (FAKE_PAIR_ANOTHER); without one these
# checks are reported as failed below, and the rest still runs.
if [ -n "$CODE2" ]; then
  TV2=$(as bob device pair "$CODE2" --name "Bob's TV" --json | jq_ 'd["device_id"]')
  [ ${#TV2} = 8 ] && [ "$TV2" != "$TV" ] && ok "a second Carbon pairs the same device, as their own pair"
  as bob device ls | grep "$TV2" | grep -q "Bob's TV (shared)" && ok "each Carbon sees it as shared"
  as bob device ls | grep "$TV2" | grep -q "yes (another Carbon's Silicon)" && ok "the other Carbon sees only that it is in use"
  expect_exit 6 as bob device stop "$TV2"; ok "another pair can't stop the owner's session"
  as sous session ls --json | grep -q "$SID" && ok "the Silicon's session survives the other Carbon's stop"
  as alice device stop "$TV" >/dev/null
  expect_exit 6 as sous --session "$SID" snapshot; ok "the owner can still stop their own session"
else
  echo "  (the fake device showed no code for another Carbon)"
  as sous session end "$SID" >/dev/null
fi

# Waking it.
[ "$(as scout --json device wake "$TV" --reason "Need the TV on for the demo" | jq_ '(d["state"], d["asks"], "team" in d)')" = "('open', 1, False)" ] && ok "device wake --json prints the wake request"
expect_exit 12 as scout device wake "$TV" --reason "Again"; ok "asking again within 5 minutes: exit 12"
as chef device wake "$TV" --reason "Need the TV on" >"$WORK/out" || true
grep -q "Asked c:alice to wake Family TV ($TV); the request expires at" "$WORK/out" && grep -q "then run: extend session new $TV" "$WORK/out" && ok "device wake says who was asked, and what to run once it wakes"
grep -Eq "Its Carbon (was|will be|was already) told through Ting" "$WORK/out" && ok "device wake says how the Carbon hears of it"
as alice device wake-requests ls "$TV" --open | grep -q "si:scout" && ok "the Carbon lists the open wake requests"
as alice device show "$TV" | grep -q "Open wake requests:" && ok "device show lists the open wake requests"
as alice device wake-requests answer "$TV" woken | grep -q "is awake: every open request to wake it has ended" && ok "answer woken ends every open request"
as alice device wake-requests mute "$TV" --silicon si:chef | grep -q "are off" && ok "wake requests muted for one Silicon"
expect_exit 6 as chef device wake "$TV" --reason "Please"; ok "a muted Silicon's ask: exit 6"
as alice device wake-requests unmute "$TV" --silicon si:chef | grep -q "are on" && ok "…and on again"
as alice ting on | grep -qi "on" && ok "ting on turns Extend's notifications on for the account"
# A 1.0 app can't tell Extend when it wakes. Its Carbon's Ting is refused here: the local Ting
# answers 404 for the type, as Ting does for a type nobody registered.
FAKE_ENV= start_fake linux
OLD=$(as alice device pair "$CODE" --name "Old box" --access si:chef --json | jq_ 'd["device_id"]')
for _ in $(seq 30); do grep -q PAIRED "$FAKE_LOG" && break; sleep 0.2; done; sleep 0.5
curl -sS --fail-with-body -XPOST "$API/dev/ting/missing" -H 'content-type: application/json' \
  -d '{"type":"missing","data":{"event":"device.wake_requested","missing":true}}' >/dev/null
as chef device wake "$OLD" --reason "Need the screen" | grep -q "Extend can't tell when Old box wakes; its Carbon will say so." && ok "device wake says when Extend can't tell"
as alice ting status | grep -q "extend.device.wake_requested" && ok "ting status shows a type Ting doesn't know yet"
curl -sS --fail-with-body -XPOST "$API/dev/ting/missing" -H 'content-type: application/json' \
  -d '{"type":"missing","data":{"event":"device.wake_requested","missing":false}}' >/dev/null
as chef device wake "$OLD" --cancel | grep -q "Withdrew your request to wake $OLD" && ok "device wake --cancel withdraws it"

# Setup retry.
expect_exit 6 as alice device setup "$OLD" --retry; grep -q "Nothing to retry" "$WORK/err" && ok "setup --retry with nothing failed: exit 6"
FAKE_ENV="FAKE_APP_VERSION=1.1.0 FAKE_FAILED_STEP=fake_step" start_fake android
SB=$(as alice device pair "$CODE" --name "Setup box" --json | jq_ 'd["device_id"]')
for _ in $(seq 30); do grep -q PAIRED "$FAKE_LOG" && break; sleep 0.2; done; sleep 0.5
as alice device setup "$SB" | grep -q "Retry: extend device setup $SB --retry --step fake_step" && ok "a failed setup step says how to retry it"
as alice device setup "$SB" --retry >"$WORK/out" || true; grep -q "^Retrying .* on Setup box." "$WORK/out" && grep -q "^Done: " "$WORK/out" && ok "setup --retry reruns the failed step and follows it"

# Signing out ends the running sessions of the Silicons the Carbon gave access to.
as chef session new "$OLD" >/dev/null 2>&1 || true
as alice logout | grep -q "si:chef" && ok "a Carbon's logout names the sessions it ended"
[ "$(as alice login status --json)" = '{"authenticated":false}' ] && ok "after logout, login status --json says signed out"
signin alice c:alice

# ── State directory ──
signin mover si:chef c:alice
as mover config set telemetry off >/dev/null
mkdir -p "$WORK/mover-new"
as mover config home "$WORK/mover-new" | grep -q "si:chef" && ok "config home moves the state"
[ "$(as mover login status --json | jq_ 'd["authenticated"]')" = True ] && ok "the sign-in survived the move"
[ "$(as mover config get telemetry)" = off ] && ok "settings survived the move"
[ ! -e "$WORK/mover/.extend/auth.json" ] && ok "nothing is left behind"

# ── No test environments ──
expect_exit 2 as alice --test 0b0e5c3e-7f43-4a4c-9a0e-5c7b2b3d1f00 device ls; ok "--test is gone (exit 2)"
expect_exit 2 as alice config test ls; ok "config test is gone (exit 2)"
set +e; EXTEND_TEST_SECRET=ask_not-a-real-secret as alice device ls >"$WORK/out" 2>"$WORK/err"; got=$?; set -e
[ "$got" = 2 ] && grep -q "EXTEND_TEST_SECRET" "$WORK/err" && ok "EXTEND_TEST_SECRET is refused before anything is sent"

as alice logout >/dev/null
expect_exit 1 as alice login status; grep -q "^Not signed in" "$WORK/out" && ok "logout clears the sign-in (login status exits 1 in text mode)"

# A check that fails prints nothing (its `&& ok` is skipped and `set -e` doesn't fire inside `&&`),
# so count them against every check in this file and name the ones that didn't pass.
want=$(grep -o '\bok "' "${BASH_SOURCE[0]}" | wc -l | tr -d ' ')
if [ "$pass" != "$want" ]; then
  touch "$WORK/passed"
  python3 - "${BASH_SOURCE[0]}" "$WORK/passed" <<'PY'
import re, sys
script, passed = open(sys.argv[1]).read(), open(sys.argv[2]).read().splitlines()
for name in re.findall(r'\bok "((?:[^"\\]|\\.)*)"', script):
    pattern = re.sub(r'\\\$[A-Za-z_]+', '.*', re.escape(name.replace('\\"', '"')))
    if not any(re.fullmatch(pattern, p) for p in passed):
        print("  ✗ " + name)
PY
  die "$pass of $want checks passed; each one marked ✗ above failed (run its command by hand to see why)"
fi
echo "All $pass checks passed."
