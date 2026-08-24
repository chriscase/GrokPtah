#!/bin/sh
set -eu

BUNDLE=${STAGE6_BUNDLE:-/private/tmp/grokptah-stage6-evidence-hardening-v1-exact-984ff9a.bundle}
SOURCE=${STAGE6_SOURCE:-/private/tmp/grokptah-stage6-evidence-hardening}
EXPECTED_SHA=cfa741b67c51bc9804b566440a855348727d398c7d97279b2e61e5cbeb12b91b
EXPECTED_HEAD=984ff9a4b13a6f2eb2054c84d5880abd5a0d4e1a
EXPECTED_PARENT=5406bbea059371392b0d77d58cca083640244a6c

test -f "$BUNDLE"
actual_sha=$(shasum -a 256 "$BUNDLE" | awk '{print $1}')
test "$actual_sha" = "$EXPECTED_SHA"
git -C "$SOURCE" bundle verify "$BUNDLE" >/dev/null
bundle_head=$(git -C "$SOURCE" bundle list-heads "$BUNDLE" | awk '$2 == "HEAD" {print $1}')
test "$bundle_head" = "$EXPECTED_HEAD"

source_head=$(git -C "$SOURCE" rev-parse HEAD)
test "$source_head" = "$EXPECTED_HEAD"
test -z "$(git -C "$SOURCE" status --porcelain)"
git -C "$SOURCE" merge-base --is-ancestor "$EXPECTED_PARENT" "$EXPECTED_HEAD"

printf '%s\n' \
  "stage6_bundle=verified" \
  "stage6_bundle_sha=$actual_sha" \
  "stage6_bundle_head=$bundle_head" \
  "stage6_source_head=$source_head" \
  "stage6_source_worktree=clean" \
  "stage6_parent_ancestor=verified"
