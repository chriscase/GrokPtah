#!/bin/sh
# Verify every committed Cargo.lock still resolves from the exact working tree.
#
# Why this exists: this repository holds several independent Cargo workspace
# roots, each with its own committed Cargo.lock. Adding a path dependency to
# one crate silently invalidates the lockfile of every *other* workspace that
# already depends on that crate. Nothing in CI compiled those workspaces, so a
# stale lockfile could reach a PR head undetected and only fail later for
# whoever ran `--locked` next.
#
# The check is mechanical: it discovers lockfiles with `git ls-files` rather
# than from a hardcoded list, so a workspace added later is covered on day one.
#
# Usage: scripts/check-committed-lockfiles.sh
# Exit:  0 = every in-scope lockfile resolves under --locked; 1 = at least one is stale.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$root"

# Lockfiles knowingly stale for reasons unrelated to the crate graph under test.
# Each entry must name the reason; entries are expected to be removed, not grown.
#
#   crates/codegen/xai-grok-markdown/fuzz/Cargo.lock
#     Stale on main independently of any Computer Use work: resolving it pulls
#     34 unrelated packages (html-escape, linkify, icu_*). Refreshing those here
#     would be an unrelated dependency bump, so it is reported and not gated.
known_stale='crates/codegen/xai-grok-markdown/fuzz/Cargo.lock'

status=0
skipped=0
checked=0

for lock in $(git ls-files '*Cargo.lock' | sort); do
  manifest="$(dirname "$lock")/Cargo.toml"

  case " $known_stale " in
    *" $lock "*)
      printf 'SKIP    %s (known stale, unrelated to this crate graph)\n' "$lock"
      skipped=$((skipped + 1))
      continue
      ;;
  esac

  if [ ! -f "$manifest" ]; then
    printf 'FAIL    %s (no manifest at %s)\n' "$lock" "$manifest"
    status=1
    continue
  fi

  if cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null 2>&1; then
    printf 'ok      %s\n' "$lock"
    checked=$((checked + 1))
  else
    printf 'FAIL    %s needs updating; run:\n' "$lock"
    printf '          cargo metadata --format-version 1 --manifest-path %s\n' "$manifest"
    status=1
  fi
done

printf '\n%d lockfile(s) verified under --locked, %d skipped as known stale\n' "$checked" "$skipped"

if [ "$status" -ne 0 ]; then
  printf 'committed lockfile check FAILED\n' >&2
fi

exit "$status"
