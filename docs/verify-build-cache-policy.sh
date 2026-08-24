#!/bin/sh
set -eu

if [ "$#" -gt 1 ]; then
  echo "usage: verify-build-cache-policy.sh [checkout]" >&2
  exit 64
fi

checkout=${1:-.}
checkout=$(unset CDPATH; cd -- "$checkout" && pwd -P)
expected_wrapper=/opt/homebrew/bin/sccache
expected_cache=/Users/chriscase/Library/Caches/grokptah/sccache
expected_target=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default

test "${RUSTC_WRAPPER:-}" = "$expected_wrapper" || {
  echo "RUSTC_WRAPPER must be $expected_wrapper" >&2
  exit 65
}
test -x "$RUSTC_WRAPPER" || {
  echo "RUSTC_WRAPPER is not executable" >&2
  exit 65
}
test "${SCCACHE_DIR:-}" = "$expected_cache" || {
  echo "SCCACHE_DIR must be $expected_cache" >&2
  exit 65
}
test "${CARGO_TARGET_DIR:-}" = "$expected_target" || {
  echo "CARGO_TARGET_DIR must be $expected_target" >&2
  exit 65
}

case "$CARGO_TARGET_DIR" in
  "$checkout"|"$checkout"/*)
    echo "refusing a checkout-local Cargo target" >&2
    exit 66
    ;;
esac

test -x "$(command -v sccache)" || {
  echo "sccache is not available on PATH" >&2
  exit 67
}

printf 'build_cache_checkout=%s\n' "$checkout"
printf 'build_cache_wrapper=%s\n' "$RUSTC_WRAPPER"
printf 'build_cache_sccache_dir=%s\n' "$SCCACHE_DIR"
printf 'build_cache_target_dir=%s\n' "$CARGO_TARGET_DIR"
printf 'build_cache_owner=%s:%s\n' "$(id -u)" "$(id -g)"
printf 'build_cache_disk=\n'
df -h "$checkout" "$SCCACHE_DIR" 2>/dev/null || df -h "$checkout"

printf 'build_cache_processes=\n'
ps -axo pid,ppid,etime,command | grep -E '[c]argo|[r]ustc|[s]ccache' || true

if [ -e "$CARGO_TARGET_DIR" ]; then
  printf 'build_cache_target_size=\n'
  du -sh -- "$CARGO_TARGET_DIR"
  printf 'build_cache_target_open_handles=%s\n' \
    "$(lsof -t -- "$CARGO_TARGET_DIR" 2>/dev/null | sort -u | wc -l | tr -d ' ')"
else
  printf 'build_cache_target_size=absent\n'
  printf 'build_cache_target_open_handles=0\n'
fi

printf 'build_cache_sccache_stats=\n'
sccache --show-stats
