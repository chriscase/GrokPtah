# Build agent tool matrix (GrokPtah vs Grok Build / xai-grok-tools)

Status as of Phase 15 remainder. Bridge implements a **thin** tool loop (ADR-001); full upstream embed is non-goal.

| Upstream / Grok Build concept | GrokPtah tool | Status | Notes |
|------------------------------|---------------|--------|-------|
| list_dir / LS | `list_dir` | **shipped** | Relative to project cwd |
| read_file / Read | `read_file` | **shipped** | Size-capped |
| grep / Grep | `grep` | **shipped** | Regex under path |
| write / Write | `write_file` | **shipped** | Permission + sandbox |
| shell / Bash | `run_terminal_cmd` | **shipped** | Streamed; cancel kills child |
| glob / Glob | `glob_files` | **shipped** | Simple globs |
| apply_patch / Edit | `apply_patch` | **shipped** | Multi-hunk Update File + JSON search/replace |
| todo_write / TodoWrite | `todo_write` | **shipped** | Session-local list |
| memory | `memory_write` / `memory_read` | **shipped** | Project-scoped under `~/.grokptah/memory/` |
| web_fetch / WebFetch | `web_fetch` | **shipped** | Offline stub; live HTTP when online |
| explore subagent | `spawn_explore` | **shipped** | Read-only survey |
| MCP tools | `mcp__server__tool` | **shipped** | Stdio servers only |
| notebook | — | **gap** | Not planned in bridge |
| browser / computer use | — | **gap** | Out of scope for thin loop |
| image_gen | — | **gap** | Desktop may use separate Imagine path |
| subagent general-purpose | — | **partial** | Explore only; full GP spawn tracked in #151 (GUI #152) |
| semantic search | — | **gap** | Grep + glob only |
| git specialized tools | shell | **via shell** | Use `run_terminal_cmd` |

## Permission detail

| Tool | Permission | Sandbox |
|------|------------|---------|
| `write_file`, `apply_patch` | Prompt unless YOLO | Denied in `read-only` |
| `run_terminal_cmd` | Prompt unless YOLO | Mutators blocked in `read-only` |
| `web_fetch` | Prompt (via tool path) | Denied in `read-only` |
| `mcp__*` | Prompt unless YOLO | N/A |
| read/search/todo/memory_read | No prompt | Always under cwd |

## Offline test hooks

Deterministic offline prompts ( `GROKPTAH_AGENT_OFFLINE=1` ):

- `list files` → `list_dir`
- `write path: content` → `write_file`
- `run <cmd>` → shell
- `todo <text|json>` → `todo_write`
- `remember <fact>` / `recall <q>` → memory tools
- `patch <json|Update File>` → `apply_patch`
- `web_fetch <url>` → offline stub
