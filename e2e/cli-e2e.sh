#!/usr/bin/env bash
# End-to-end test of the `extend` CLI against a running Extend service (local IAM) and a fake device.
#
#   e2e/cli-e2e.sh [api_url]      (default http://127.0.0.1:8480; the service must use EXTEND_IAM_MODE=local
#                                  with c:alice, si:chef, si:sous in team acme, and EXTEND_HONEYCOMB_SERVICE_TOKEN=hck_local_dev_token)
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
trap 'kill $(jobs -p) 2>/dev/null || true; [ -z "$ENV" ] || lifecycle purge >/dev/null || true; rm -rf "$WORK"' EXIT
export EXTEND_API_URL=$API EXTEND_TELEMETRY=off

pass=0
ok() { pass=$((pass+1)); echo "  ✓ $1"; }
die() { echo "  ✗ $1"; exit 1; }
as() { local who=$1; shift; mkdir -p "$WORK/$who"; SILICON_HOME="$WORK/$who" "$EXTEND" "$@"; }
expect_exit() { local want=$1; shift; set +e; "$@" >"$WORK/out" 2>"$WORK/err"; local got=$?; set -e; [ "$got" = "$want" ] || { cat "$WORK/out" "$WORK/err"; die "expected exit $want, got $got: $*"; }; }
jq_() { python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }

start_fake() { # os [secret] -> sets CODE, FAKE_LOG
  FAKE_LOG="$WORK/fake-$RANDOM.log"
  "$FAKE" "$API" "$@" >"$FAKE_LOG" 2>&1 &
  for _ in $(seq 50); do CODE=$(grep -m1 PAIRING_CODE "$FAKE_LOG" | awk '{print $2}' || true); [ -n "$CODE" ] && return; sleep 0.2; done
  cat "$FAKE_LOG"; die "fake device never showed a pairing code"
}

echo "CLI end-to-end against $API"

# ── Help and discovery ──
as nobody --help | grep -q "Getting started" && ok "extend --help is a documentation tree"
as nobody device --help | grep -q "extend device pair" && ok "extend device --help lists what's under it"
as nobody iam --json | jq_ 'd["data"]["app_id"]' | grep -q extend && ok "extend iam --json gives app_id"
expect_exit 3 as nobody login status --json; grep -q '"authenticated": *false' "$WORK/out" && ok "login status --json reports authenticated:false (exit 3)"
expect_exit 2 as nobody frobnicate; grep -q "not an extend command" "$WORK/err" && ok "unknown command explains itself (exit 2)"
expect_exit 2 as nobody boot; grep -q "doesn't relay" "$WORK/err" && ok "agent-device developer command is refused with a reason"
expect_exit 2 as nobody config home /definitely/not/here; grep -q "not a directory" "$WORK/err" && ok "config home rejects a non-directory"

# ── Carbon pairs a device ──
as alice login c:alice >/dev/null
as alice login status --json | jq_ 'd["data"]["member"]["id"]' | grep -q c:alice && ok "Carbon logged in with an SLT; login status says authenticated"
start_fake linux
expect_exit 5 as alice device pair 000000 --name nope
ok "wrong pairing code → exit 5"
DEV=$(as alice device pair "$(echo "$CODE" | tr A-F a-f)" --name "CLI box" --access si:chef --json | jq_ 'd["data"]["device_id"]')
[ ${#DEV} = 8 ] && ok "paired device $DEV (lowercase code accepted)"
for _ in $(seq 30); do grep -q PAIRED "$FAKE_LOG" && break; sleep 0.2; done
sleep 0.5
as alice device ls | grep -q "CLI box" && ok "Carbon sees the device in device ls"
as alice device ttl "$DEV" 30 | grep -q "30 days" && ok "pair lifetime set to 30 days"
expect_exit 2 as alice device ttl "$DEV" 31
ok "31 days refused (exit 2)"
as alice device access ls "$DEV" | grep -q si:chef && ok "access list shows si:chef"

# ── Silicon uses it ──
as chef login si:chef >/dev/null
as chef device ls | grep -q "$DEV" && ok "Silicon lists the device it has access to"
as chef device show "$DEV" | grep -q "terminal" && ok "device show lists terminal for a computer"
SID=$(as chef session new "$DEV" --connect 2>/dev/null)
[ ${#SID} -ge 3 ] && ok "session $SID started and connected"
as chef --help | grep -q "connected to session $SID" && ok "--help narrows to the connected device's commands"
as chef --help | grep -q "tv-remote" && die "--help shows TV commands on a computer" || ok "--help hides commands the device can't run"
as chef snapshot -i | grep -q "snapshot -i" && ok "snapshot relayed to the device"
as chef fill @e3 "secret words" >/dev/null && ok "fill relayed"
expect_exit 1 as chef is visible 'label="Nope"'
ok "a failed assertion exits 1"
expect_exit 10 as chef tv-remote press up
ok "a command the device can't do exits 10"
as chef screenshot --ttl 2h --out "$WORK/shot.png" >/dev/null
[ -s "$WORK/shot.png" ] && ok "screenshot stored, linked, and saved locally with --out"
FILE=$(as chef file ls --json | jq_ 'd["data"]["items"][0]["file_id"]')
as chef file keep "$FILE" | grep -q permanent && ok "file kept permanently"

# ── One Silicon at a time ──
as sous login si:sous >/dev/null
as alice device access grant "$DEV" si:sous >/dev/null
expect_exit 6 as sous session new "$DEV"
grep -q "extend request send $DEV" "$WORK/err" && ok "second Silicon gets exit 6 and the request command"
as sous request send "$DEV" --reason "Need two minutes for an OTP" | grep -q "si:chef" && ok "request delivered to si:chef"
curl -s "$API/dev/ting" | grep -q "Need two minutes for an OTP" && ok "Ting received the reason verbatim"

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

# ── Report and version ──
as chef report "fill loses the last character" --pr https://github.com/teamofsilicons/silicon-extend/pull/1 | grep -q "sent" && ok "bug report sent"
as chef version | grep -q "API v1" && ok "version shows the negotiated API"

# ── Remove ──
expect_exit 2 as alice device rm "$DEV"
grep -q -- "--yes" "$WORK/err" && ok "rm without --yes explains and does nothing"
as alice device rm "$DEV" --yes | grep -q Removed && ok "device removed"
sleep 0.5; grep -q "UNPAIRED device_removed" "$FAKE_LOG" && ok "device was told it's unpaired"

# ── Test environment ──
ENV=$(python3 -c 'import uuid;print(uuid.uuid4())')
SECRET=ask_$(python3 -c 'import secrets,base64;print(base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("="))')
lifecycle prepare cli-e2e >"$WORK/prepare" || { cat "$WORK/prepare"; echo; die "couldn't prepare a test environment (the service's reason is above)"; }
curl -sf -XPOST "$API/dev/iam/test-apps" -H 'content-type: application/json' -d "{\"type\":\"t\",\"data\":{\"secret\":\"$SECRET\",\"environment_id\":\"$ENV\"}}"
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

as alice logout >/dev/null && expect_exit 3 as alice login status
ok "logout clears the login (exit 3 after)"

echo "All $pass checks passed."
