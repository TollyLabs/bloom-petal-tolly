#!/usr/bin/env bash
# Build every route component and check declared capabilities against the
# compiled imports. The Petal CLI must be built from the same SDK revision the
# route crate pins (PETAL_REV); an ambient `petal` on PATH is used only when
# PETAL_BIN points at it explicitly, otherwise the pinned CLI is installed
# under target/petal-tool.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PETAL_REV="73c5b06a77599368fbc79fb7947a629b5b4c630e"

if [[ -n "${PETAL_BIN:-}" ]]; then
  PETAL="$PETAL_BIN"
else
  tool_root="$ROOT/target/petal-tool"
  if [[ ! -x "$tool_root/bin/petal" ]]; then
    cargo install \
      --git https://github.com/bloom-directory/petal \
      --rev "$PETAL_REV" \
      --locked \
      --root "$tool_root" \
      bloom-petal-cli
  fi
  PETAL="$tool_root/bin/petal"
fi

# The three pins must agree.
for file in "$ROOT/petal-build.toml" "$ROOT/route/Cargo.toml"; do
  grep -q "$PETAL_REV" "$file" || { echo "SDK rev drift in $file (expected $PETAL_REV)" >&2; exit 1; }
done

"$PETAL" build --root "$ROOT"
"$PETAL" check --root "$ROOT"
