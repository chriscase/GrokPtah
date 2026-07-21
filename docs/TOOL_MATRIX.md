# Build agent tool matrix (GrokPtah vs Grok Build / xai-grok-tools)

Status as of Phase 16. Bridge implements a **thin** tool loop (ADR-001); full upstream embed is non-goal.

## Residual policy (#160)

Every capability is **shipped**, **via shell**, or **explicitly deferred**. Nothing may be silently missing.

| Status | Meaning |
|--------|---------|
| **shipped** | Wired into the Build tool loop and tested |
| **via shell** | Use `run_terminal_cmd` (no dedicated tool) |
| **deferred** | Explicit non-goal this phase; reopen only with a dedicated issue |

## Matrix

| Upstream / Grok Build concept | GrokPtah tool | Status | Notes |
|------------------------------|---------------|--------|-------|
| list_dir / LS | `list_dir` | **shipped** | Relative to project cwd |
| read_file / Read | `read_file` | **shipped** | Size-capped |
| grep / Grep | `grep` | **shipped** | Regex under path |
| write / Write | `write_file` | **shipped** | Permission + soft tool-safety profile |
| shell / Bash | `run_terminal_cmd` | **shipped** | Streamed; cancel kills child; **exec-risk preflight** (#155) |
| glob / Glob | `glob_files` | **shipped** | Simple globs |
| apply_patch / Edit | `apply_patch` | **shipped** | Multi-hunk Update File + JSON search/replace |
| todo_write / TodoWrite | `todo_write` | **shipped** | Session-local list |
| memory | `memory_write` / `memory_read` | **shipped** | Project-scoped under `~/.grokptah/memory/` |
| web_fetch / WebFetch | `web_fetch` | **shipped** | Offline stub; live HTTP when online; **SSRF preflight** (#179) |
| explore subagent | `spawn_explore` | **shipped** | Read-only survey |
| general-purpose / plan subagent | `spawn_general_purpose` / `spawn_subagent` | **shipped** | Parallel GP; plan mode blocks mutators (#161) |
| MCP tools | `mcp__server__tool` | **shipped** | Stdio servers only |
| kill_task / task_output | background task cancel + shell cancel | **shipped** (partial) | Via Tasks panel / `cancel_background_task` / turn cancel — not full Build IDs |
| notifications | desktop OS notifications | **deferred** | Track under residual; no ship issue yet |
| notebook | — | **deferred** | Not planned in bridge |
| browser / computer use | — | **deferred** | Out of scope for thin loop |
| image_gen | — | **deferred** | Desktop may use separate Imagine path |
| semantic search | — | **deferred** | Grep + glob only |
| git specialized tools | shell | **via shell** | Use `run_terminal_cmd` |
| workflows (`xai-workflow`) | — | **deferred** | #176 closed not-planned for Phase 16 |
| OS sandbox / Landlock | — | **deferred** | Soft profile + exec-risk only; **not** parity |

## Permission detail

| Tool | Permission | Soft profile |
|------|------------|--------------|
| `write_file`, `apply_patch` | Prompt unless YOLO | Denied in `read-only` |
| `run_terminal_cmd` | Prompt unless YOLO; exec-risk Deny/Ask | Mutators blocked in `read-only`; Deny-tier risk blocked unless `full`+YOLO |
| `web_fetch` | Prompt (via tool path) | Denied in `read-only`; SSRF blocks localhost/private |
| `mcp__*` | Prompt unless YOLO | N/A |
| read/search/todo/memory_read | No prompt | Always under cwd |

## Offline test hooks

Deterministic offline prompts (`GROKPTAH_AGENT_OFFLINE=1`):

- `list files` → `list_dir`
- `write path: content` → `write_file`
- `run <cmd>` → shell
- `todo <text|json>` → `todo_write`
- `remember <fact>` / `recall <q>` → memory tools
- `patch <json|Update File>` → `apply_patch`
- `web_fetch <url>` → offline stub (still SSRF-checked)
