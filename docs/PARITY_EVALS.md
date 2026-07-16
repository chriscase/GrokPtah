# Build agent parity evals

Phase 15 (#93) measures GrokPtah **Build** sessions against Grok Build CLI quality.

## Offline (CI-friendly)

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --lib project_context
cargo test --test agent_tools
cargo test --test bridge_lifecycle -- --test-threads=1
```

These cover glob/patch/instructions helpers and host lifecycle without requiring network.

## Live agent loop (manual / network)

1. `grok login` (or API key) so `~/.grok/auth.json` is valid.
2. Launch desktop, **Builds** mode, set cwd to a fixture repo with `AGENTS.md`.
3. Prompt examples:
   - “Using tools, list `src/`, read `src/lib.rs`, and apply a small patch adding a comment.”
   - “Create `hello.txt` with contents hi and confirm with list_dir.”
4. Confirm multi-round thoughts (`agent round N/24`), tool cards, and a final summary.

## Phase 15 P1 (streaming / skills / MCP / diffs)

Covered by unit + lifecycle tests offline:

- `project_context` skills inject (`load_skills_context`)
- `mcp_runtime` function name shape
- Offline Build turn emits `FileEdit` after `write …`

Live (manual): Build mode should stream assistant tokens mid-step; Stop cancels HTTP within ~1s; enabled stdio MCP tools appear as `mcp__server__tool`.

## Phase 15 P2 (plan / hooks / slash / sandbox / explore)

- Plan propose → **Accept & execute** runs plan as context (offline plan steps always)
- PreToolUse deny hooks (`hooks.json`)
- Slash: `/model` `/effort` `/clear` `/context` `/mcp` `/skills` `/sandbox` `/explore` `/compact`
- Sandbox `read-only` blocks writes; `/explore` emits `SubagentSpawned`

## Full harness (TODO)

Automate fixture tasks vs CLI in CI once #78/#79 stabilize. Track progress on #93.
