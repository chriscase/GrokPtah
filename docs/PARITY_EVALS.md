# Build agent parity evals

Phase 15 (#93) measures GrokPtah **Build** sessions against Grok Build CLI quality.

## Offline smoke (CI-friendly / gating)

From the bridge crate:

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --lib --tests -- --test-threads=1
# Smoke-only filter:
cargo test --test parity_eval -- --test-threads=1
```

Smoke fixtures (`tests/parity_eval.rs`):

| Task | What it proves |
|------|----------------|
| `smoke_search_lists_and_reads` | Offline list + write path |
| `smoke_edit_apply_patch_region` | Structured `apply_patch` multi-region-safe edit |
| `smoke_refuse_unsafe_write_in_readonly` | Sandbox refuse path |
| `smoke_todo_and_memory_tools` | todo + project memory across sessions |
| `compact_never_shrinks_local_transcript` | Non-destructive compact |

Also:

```sh
cargo test --lib project_context
cargo test --lib memory
cargo test --lib todo_list
cargo test --test bridge_lifecycle -- --test-threads=1
```

## Phase 15 P1 (streaming / skills / MCP / diffs)

- `project_context` skills inject (`load_skills_context`)
- `mcp_runtime` function name shape
- Offline Build turn emits `FileEdit` after `write …`

Live (manual): Build mode should stream assistant tokens mid-step; Stop cancels HTTP within ~1s; enabled stdio MCP tools appear as `mcp__server__tool`.

## Phase 15 P2 (plan / hooks / slash / sandbox / explore)

- Plan propose → **Accept & execute** runs plan as context
- PreToolUse deny hooks (`hooks.json`)
- Slash: `/model` `/effort` `/clear` `/context` `/mcp` `/skills` `/sandbox` `/explore` `/compact`
- Sandbox `read-only` blocks writes; `/explore` emits `SubagentSpawned`

## Phase 15 remainder (#79 #84 #85 #93 #94 #95)

- Tool matrix: `docs/TOOL_MATRIX.md`
- Compact: extractive offline + LLM summary when online; auto-compact when window > 40
- Memory: `memory_write` / `memory_read` + inject into Build system context
- Observability: `AgentProgress`, `RateLimited` events; export transcript API
- Resilience: cancel mid-shell (lifecycle test); 429 → rate-limit message

## Full / live harness (manual, network)

1. `grok login` so `~/.grok/auth.json` is valid.
2. Launch desktop, **Builds** mode, cwd = fixture with `AGENTS.md`.
3. Prompts:
   - Multi-round: list → read → apply_patch → todo_write
   - Force long session then `/compact` and continue
4. Optional: compare same prompts against Grok Build CLI; record rounds/time manually.

Live network is **not** a CI gate (ADR / non-goals).

## Tool inventory

See **[TOOL_MATRIX.md](./TOOL_MATRIX.md)** for upstream → GrokPtah status.
