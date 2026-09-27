#!/usr/bin/env bash
# Drops the throwaway databases the service suites create: `extend_e2e_*` (e2e.rs, testenv_gaps.rs),
# `extend_core_*` (core_gaps.rs), `extend_gaps_*` (devices_gaps.rs), `extend_contracts_*`
# (contracts.rs), and 1.1's `extend_v11_*`, `extend_v11s_*` and `extend_mig_*` (tests/common). The
# development database `extend` and anything else are left alone.
set -euo pipefail
docker exec silicon-extend-postgres psql -U extend -d postgres -tAc \
  "SELECT datname FROM pg_database WHERE datname ~ '^extend_(e2e|core|gaps|contracts|v11|v11s|mig)_[0-9a-f]{32}\$'" |
  while read -r db; do [ -n "$db" ] && docker exec silicon-extend-postgres psql -U extend -d postgres -qc "DROP DATABASE IF EXISTS \"$db\" WITH (FORCE)"; done
echo "dropped test databases"
