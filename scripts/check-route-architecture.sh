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
