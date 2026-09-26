#!/usr/bin/env bash
# Drops the throwaway databases `cargo test -p bridge-service` creates (bridge_e2e_*).
set -euo pipefail
docker exec silicon-bridge-postgres psql -U bridge -d postgres -tAc "SELECT datname FROM pg_database WHERE datname LIKE 'bridge_e2e_%'" |
  while read -r db; do [ -n "$db" ] && docker exec silicon-bridge-postgres psql -U bridge -d postgres -qc "DROP DATABASE IF EXISTS $db WITH (FORCE)"; done
echo "dropped test databases"
