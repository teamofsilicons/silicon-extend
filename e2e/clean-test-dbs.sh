#!/usr/bin/env bash
# Drops the throwaway databases the service suites create: `extend_v11_*` and `extend_v11s_*`
# (tests/common, used by most suites), `extend_core_*` (core_gaps.rs), `extend_contracts_*`
# (contracts.rs), `extend_mig_*` (migration.rs), `extend_acmig_*` (accounts_migration.rs), and the
# older `extend_e2e_*` and `extend_gaps_*`. The development database `extend` and anything else are
# left alone.
#
# It connects with psql to the same admin URL the tests use (EXTEND_TEST_ADMIN_URL, default
# postgres://extend:extend@127.0.0.1:5440/postgres); set PSQL to use another psql binary.
set -euo pipefail
admin="${EXTEND_TEST_ADMIN_URL:-postgres://extend:extend@127.0.0.1:5440/postgres}"
psql_bin="${PSQL:-psql}"
"$psql_bin" "$admin" -tAc \
  "SELECT datname FROM pg_database WHERE datname ~ '^extend_(e2e|core|gaps|contracts|v11|v11s|mig|acmig)_[0-9a-f]{32}\$'" |
  while read -r db; do [ -n "$db" ] && "$psql_bin" "$admin" -qc "DROP DATABASE IF EXISTS \"$db\" WITH (FORCE)"; done
echo "dropped test databases"
