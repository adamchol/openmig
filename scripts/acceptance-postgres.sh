#!/usr/bin/env bash
# End-to-end acceptance against a real-world migration history.
#
#   scripts/acceptance-postgres.sh <migration dir> [openmig binary]
#
# The directory holds versioned `<14 digits>[_name].sql` files, e.g. a project's existing migration
# history. Set NO_TRANSACTION_MARKER to a comment line that marks files to run outside a
# transaction (it is rewritten to `-- openmig:no-transaction`). The script starts a private
# PostgreSQL cluster from the binaries on PATH, then:
#   1. deploys the history into database `replayed`,
#   2. inspects it into HCL,
#   3. syncs that HCL into an empty database and requires no differences,
#   4. requires a second sync to be a no-op,
#   5. generates a baseline migration from the HCL, checks it, deploys it and requires no
#      differences and no drift.
set -euo pipefail

src=${1:?usage: $0 <migration dir> [openmig binary]}
openmig=$(command -v "${2:-openmig}")
case $openmig in /*) ;; *) openmig=$(realpath "$openmig") ;; esac
work=$(mktemp -d)
sock=$(mktemp -d /tmp/omacc.XXXXXX)
run_as=()
if [ "$(id -u)" = 0 ]; then
  chown nobody "$sock"
  run_as=(runuser -u nobody --)
fi
"${run_as[@]}" initdb -D "$sock/data" -U postgres --auth=trust -N >/dev/null
"${run_as[@]}" postgres -D "$sock/data" -k "$sock" -c listen_addresses= -c port=5432 -c fsync=off \
  -c unix_socket_permissions=0777 >"$sock/log" 2>&1 &
pg=$!
trap 'kill $pg 2>/dev/null; wait $pg 2>/dev/null; rm -rf "$sock" "$work"' EXIT
for _ in $(seq 100); do [ -S "$sock/.s.PGSQL.5432" ] && break; sleep 0.1; done
url() { echo "postgresql:///$1?host=$sock&port=5432&user=postgres"; }
for db in replayed synced deployed; do psql "$(url postgres)" -qc "create database $db"; done

step() { printf '\n== %s\n' "$*"; }
expect_no_diff() {
  out=$("$openmig" diff --from "url:$(url "$1")" --to "url:$(url "$2")" 2>&1)
  if [ -n "$(echo "$out" | grep -v 'no differences')" ]; then
    echo "$out"
    echo "FAIL: $1 and $2 differ" >&2
    exit 1
  fi
  echo "no differences between $1 and $2"
}

mkdir -p "$work/history" "$work/project"
cp "$src"/*.sql "$work/history/"
if [ -n "${NO_TRANSACTION_MARKER:-}" ]; then
  sed -i "s|^${NO_TRANSACTION_MARKER}\$|-- openmig:no-transaction|" "$work/history"/*.sql
fi
"$openmig" --engine postgres --migrations "$work/history" rehash

step "deploy the history"
time "$openmig" --engine postgres --migrations "$work/history" --url "$(url replayed)" deploy

cat >"$work/project/openmig.toml" <<TOML
[databases.app]
engine = "postgres"
schema = ["schema"]
migrations = "migrations"
url = "$(url synced)"
manage_roles = true
TOML
cd "$work/project"

step "inspect into HCL"
time "$openmig" --url "$(url replayed)" inspect --split-by schema -o schema
wc -l schema/*.hcl | tail -1

step "sync the HCL into an empty database"
time "$openmig" -q sync --yes --allow-destructive >/dev/null
expect_no_diff replayed synced

step "second sync is a no-op"
time "$openmig" sync --yes

step "generate, check and deploy a baseline"
time "$openmig" generate baseline
time "$openmig" check
time "$openmig" --url "$(url deployed)" deploy
expect_no_diff replayed deployed
"$openmig" --url "$(url deployed)" status --drift

printf '\nPASS\n'
