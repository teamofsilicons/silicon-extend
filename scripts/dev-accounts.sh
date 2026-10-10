#!/usr/bin/env bash
# Start Extend on this machine against a local Silicon Accounts stack (idempotent):
#
#   EXTEND_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build] [--check-webhook]
#
# Creates and migrates the database extend_e2e on 127.0.0.1:5460, starts a Briefcase stand-in that verifies
# Extend's proofs at Silicon Accounts (127.0.0.1:4222) and extend-service (127.0.0.1:4221), and points Extend's app
# webhook at Silicon Accounts to http://127.0.0.1:4221/webhooks/accounts, proving it with a test ping.
# Stop with scripts/dev-accounts-stop.sh. Configuration and details: python3 scripts/dev_accounts.py --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" up "$@"
