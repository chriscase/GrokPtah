#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
crate="$root/crates/codegen/grokptah-isolated-visual/Cargo.toml"
target="${CARGO_TARGET_DIR:-/tmp/grokptah-isolated-visual-target}"
export CARGO_TARGET_DIR="$target"

echo "isolated visual fixture: simulator/source only; ineligible for VM qualification" >&2

if [ "${GROKPTAH_ISOLATED_VISUAL_ALLOW_VIRTUALIZATION:-}" = "1" ]; then
  echo "GROKPTAH_ISOLATED_VISUAL_ALLOW_VIRTUALIZATION=1 is set; still fail-closed unless preflight allows launch" >&2
  extra="--allow-virtualization"
else
  extra=""
fi

out="${GROKPTAH_ISOLATED_VISUAL_EVIDENCE:-/tmp/isolated-visual-evidence.json}"
cargo test --locked --manifest-path "$crate" -- --test-threads=1
cargo run --locked --manifest-path "$crate" --bin grokptah-isolated-visual-qualify -- \
  --out "$out" $extra

echo "isolated visual fixture: ok (simulator/source; VF not launched)" >&2
