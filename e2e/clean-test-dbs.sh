#!/usr/bin/env bash
# Drops the throwaway databases `cargo test -p extend-service` creates (extend_e2e_*).
set -euo pipefail
docker exec silicon-extend-postgres psql -U extend -d postgres -tAc "SELECT datname FROM pg_database WHERE datname LIKE 'extend_e2e_%'" |
  while read -r db; do [ -n "$db" ] && docker exec silicon-extend-postgres psql -U extend -d postgres -qc "DROP DATABASE IF EXISTS $db WITH (FORCE)"; done
echo "dropped test databases"
