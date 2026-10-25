#!/usr/bin/env bash
# Produce a real schema-2 work-creation crash cut from exact #585 base (25d40262).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../../.." && pwd)"
BRIDGE="$REPO/crates/codegen/grokptah-agent-bridge"
PRODUCER_SHA="25d40262a3b530227af850f26974c19dfc2935fd"
OUT="$BRIDGE/tests/fixtures/wr-v2-585-schema2-creation-cut-25d40262"
WORKTREE="$(mktemp -d /tmp/grokptah-585-cut.XXXXXX)"

git -C "$REPO" worktree add --detach "$WORKTREE" "$PRODUCER_SHA" >/dev/null
cd "$WORKTREE/crates/codegen/grokptah-agent-bridge"
rm -rf "$OUT"
mkdir -p "$OUT"
{
  echo "git worktree: $WORKTREE"
  echo "producer_sha: $PRODUCER_SHA"
  echo "cwd: $(pwd)"
  echo "build: cargo test --locked --test work_creation_recovery --no-run"
} >"$OUT/PRODUCE_COMMAND.txt"
cargo test --locked --test work_creation_recovery --no-run 2>&1 | tee "$OUT/build.log"
BIN="$(find "$WORKTREE/crates/codegen/grokptah-agent-bridge/target/debug/deps" -name 'work_creation_recovery-*' -executable -type f | head -1)"
CUT_ROOT="$(mktemp -d /tmp/grokptah-585-cut-root.XXXXXX)"
export WORK_CREATION_CHILD_ROOT="$CUT_ROOT"
export WORK_CREATION_PHASE=create
export WORK_CREATION_STAGE=lifecycle_after_intent
export GROKPTAH_CRASH_STAGE=lifecycle_after_intent
export GROKPTAH_AGENT_OFFLINE=1
set +e
"$BIN" --exact creation_service_child --nocapture >"$OUT/cut.log" 2>&1
CUT_EXIT=$?
set -e
test "$CUT_EXIT" -eq 86
cp -a "$CUT_ROOT/ledger" "$OUT/ledger-tree"
cp -a "$CUT_ROOT/home" "$OUT/home-tree"
cp "$CUT_ROOT/fixture.json" "$OUT/fixture.json"
printf '%s\n' "$PRODUCER_SHA" >"$OUT/PRODUCER_SHA"
RECEIPT="$(find "$CUT_ROOT/ledger/idempotency" -name '*.json' -type f | head -1)"
test -n "$RECEIPT"
cp "$RECEIPT" "$OUT/receipt.json"
sha256sum "$OUT/receipt.json" >"$OUT/receipt.sha256"
find "$CUT_ROOT/ledger/work-lifecycle-intents" -name '*.json' -type f | head -1 | xargs -I{} cp {} "$OUT/intent.json"
sha256sum "$OUT/intent.json" >"$OUT/intent.sha256"
sha256sum "$OUT/build.log" >"$OUT/build.log.sha256"
sha256sum "$OUT/cut.log" >"$OUT/cut.log.sha256"
{
  echo "capture_kind=producer_rerun"
  echo "captured_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "producer_sha=$PRODUCER_SHA"
  sha256sum "$OUT/receipt.json" "$OUT/intent.json" "$OUT/build.log" "$OUT/cut.log"
} >"$OUT/CAPTURE.meta"
find "$OUT" -type f | sort | xargs sha256sum >"$OUT/MANIFEST.sha256"
mkdir -p /opt/cursor/artifacts
rm -rf /opt/cursor/artifacts/wr-v2-585-schema2-creation-cut-25d40262
rm -rf /opt/cursor/artifacts/wr-v2-585-schema2-creation-cut-25d40262
cp -r --no-preserve=mode,ownership "$OUT" /opt/cursor/artifacts/wr-v2-585-schema2-creation-cut-25d40262
tar -czf /opt/cursor/artifacts/wr-v2-585-schema2-creation-cut-25d40262.tar.gz -C "$(dirname "$OUT")" "$(basename "$OUT")"

git -C "$REPO" worktree remove --force "$WORKTREE"
echo "Producer: $PRODUCER_SHA"
echo "Fixture: $OUT"
echo "Artifacts: /opt/cursor/artifacts/wr-v2-585-schema2-creation-cut-25d40262"
