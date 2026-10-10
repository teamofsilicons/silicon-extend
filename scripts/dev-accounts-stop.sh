#!/usr/bin/env bash
# Stop what scripts/dev-accounts.sh started and take Extend's webhook back off the stack:
#
#   EXTEND_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts-stop.sh [--drop] [--keep-webhook]
#
# --drop also drops the development database. Details: python3 scripts/dev_accounts.py down --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" down "$@"
