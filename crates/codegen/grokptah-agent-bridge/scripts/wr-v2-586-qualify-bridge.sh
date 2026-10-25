#!/usr/bin/env bash
set -o pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/../../.." && pwd)"
HEAD="$(git -C "$REPO_ROOT" rev-parse HEAD)"
LOGDIR="/opt/cursor/artifacts/wr-v2-586-ci-evidence-${HEAD:0:8}"
mkdir -p "$LOGDIR"
cd "$ROOT"

summary() {
  echo "$1" | tee -a "$LOGDIR/summary.txt"
}

run_logged() {
  local name="$1"
  shift
  summary "=== $name: $* ==="
  "$@" >"$LOGDIR/$name.log" 2>&1
  local ec=$?
  summary "$name EXIT=$ec"
  sha256sum "$LOGDIR/$name.log" >>"$LOGDIR/sha256sums.txt"
  return "$ec"
}

: >"$LOGDIR/sha256sums.txt"
: >"$LOGDIR/summary.txt"

run_logged fmt-check cargo fmt --check
run_logged clippy-all-targets cargo clippy --locked --all-targets -- -D warnings
run_logged focused-prune-recovery cargo test --locked --test manager_mutation_recovery manager_post_commit_prune pre_intent_manager_process_death_has_no_mutation_effect -- --test-threads=1
run_logged focused-585 cargo test --locked --test work_creation_recovery legacy_schema2_585_producer_cut_reopens_on_candidate -- --test-threads=1
run_logged cargo-test-threads1 cargo test --locked -- --test-threads=1
run_logged cargo-check-release-cached cargo check --locked --release
cargo clean -p grokptah-agent-bridge -q
run_logged cargo-check-release-clean cargo check --locked --release

git -C "$REPO_ROOT" rev-parse HEAD | tee "$LOGDIR/head.txt"
git -C "$REPO_ROOT" rev-parse 'HEAD^{tree}' | tee "$LOGDIR/tree.txt"
git -C "$REPO_ROOT" status --porcelain | tee "$LOGDIR/status.txt"
sha256sum "$LOGDIR/head.txt" "$LOGDIR/tree.txt" "$LOGDIR/status.txt" >>"$LOGDIR/sha256sums.txt"

summary "LOGDIR=$LOGDIR"
