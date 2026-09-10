#!/usr/bin/env bash
# Enforces the bloom-petal-development architectural rules:
# shared crate code must not dispatch on route identity, route files must keep
# behavior local, and secrets must stay out of route files. Run before pushing:
#   bash scripts/check-route-architecture.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

failed=0

if command -v rg >/dev/null 2>&1; then
  search_lines() { rg -n -- "$@"; }
  search_quiet() { rg -q -- "$@"; }
  search_files() { rg -l -- "$@"; }
else
  search_lines() {
    local pattern="$1"
    shift
    grep -R -n -E -- "$pattern" "$@"
  }
  search_quiet() {
    local pattern="$1"
    shift
    grep -R -q -E -- "$pattern" "$@"
  }
  search_files() {
    local pattern="$1"
    shift
    grep -R -l -E -- "$pattern" "$@"
  }
fi

# Shared crate code and route files must not define catch-all dispatchers that
# many routes would route through. Each route file owns its behavior.
if search_lines \
  'crate::(read|write|dispatch)[[:space:]]*\(|(pub[[:space:]]+)?fn[[:space:]]+(read|write|dispatch)[[:space:]]*\(' \
  route/files route/src; then
  echo "route architecture check: catch-all read/write/dispatch is forbidden; keep behavior in route files" >&2
  failed=1
fi

# The canonical published Petal SDK must be used; a vendored copy hides the
# pinned contract from the toolchain.
if search_lines 'petal[[:space:]]*=[[:space:]]*\{[^}]*path[[:space:]]*=[[:space:]]*"\.\./sdk"' route/Cargo.toml \
  || search_lines '^path[[:space:]]*=[[:space:]]*"sdk"$' petal-build.toml; then
  echo "route architecture check: use the canonical published Petal SDK" >&2
  failed=1
fi

# Route files must not touch the secret namespace; only shared write paths may.
if search_files 'secret_key|load_secret_bytes|load_secret_json|"secrets"' route/files >/dev/null; then
  echo "route architecture check: route files must not reference secret-namespace accessors" >&2
  failed=1
fi

# Host fact (Bloom v0.2.1, bloom-mount/src/adapter.rs `should_render_for_attrs`):
# a side-effecting read reports st_size 0 on the NFS mount and `cat` reads 0
# bytes; only `bloom vfs cat` returns the body. The SDK's `chain_read_spec()`
# sets side_effecting_read(true), so no route may use it.
if search_lines 'chain_read_spec' route/files; then
  echo "route architecture check: chain_read_spec renders as an empty file on the mount; use a non-side-effecting spec (account_read_spec / http_read_spec / store specs)" >&2
  failed=1
fi

# Host fact (bloom-daemon/src/lib.rs `tx_inspect`): outbox inspection is bound
# to the execution origin that staged the entry (petal id, package hash AND
# route id). The operation record route therefore cannot inspect: it is a pure
# store projection under the 5 s account cache, and reconciliation lives in
# the read handlers of the routes that stage (buy/sell/launch).
record_route='route/files/wallets/[wallet]/operations/[id].json.rs'
if [[ ! -f "$record_route" ]]; then
  echo "route architecture check: missing $record_route" >&2
  failed=1
else
  if ! search_quiet 'petal::account_read_spec\(\)\.caps\(&\["bloom:store"\]\)' "$record_route"; then
    echo "route architecture check: $record_route must use petal::account_read_spec().caps(&[\"bloom:store\"]) (store only; no outbox, chain or http)" >&2
    failed=1
  fi
  if search_lines 'bloom:tx\.outbox|bloom:chain|bloom:http|bloom:vfs|tx_inspect\(|::reconcile|route_read_side' "$record_route"; then
    echo "route architecture check: the operation record route must not inspect, read the chain, fetch or reconcile" >&2
    failed=1
  fi
fi

# A writable route should also define a local read handler so the file is
# discoverable and safe to read for instructions.
while IFS= read -r route_file; do
  if ! search_quiet 'read:' "$route_file"; then
    echo "route architecture check: writable route needs a local read handler: $route_file" >&2
    failed=1
  fi
done < <(search_files 'petal::write_spec' route/files)

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "route architecture check passed"
