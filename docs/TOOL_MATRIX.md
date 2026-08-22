# Build agent tool matrix (GrokPtah vs Grok Build / xai-grok-tools)

Status as of Phase 16. Bridge implements a **thin** tool loop (ADR-001); full upstream embed is non-goal.

This table is the **Build agent tool loop** inventory. Product capability
status — including Computer Use tiers and Computer Run MCP — lives in
[`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md). A deferred **Build-loop**
browser/computer tool is not proof that the native Computer Use adapter or
read-only Computer Run MCP tools are absent.

## Residual policy (#160)

Every Build-loop capability is **shipped**, **via shell**, or **explicitly deferred**. Native Computer Use and Computer Run MCP reads are listed so this file cannot contradict [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md); they use **on main (not Build loop)**.

| Status | Meaning |
|--------|---------|
| **shipped** | Wired into the Build tool loop and tested |
| **via shell** | Use `run_terminal_cmd` (no dedicated tool) |
| **deferred** | Explicit non-goal this phase; reopen only with a dedicated issue |
| **on main (not Build loop)** | Present on `origin/main` outside the thin Build loop. Product status (Supported / Experimental / Planned) is [`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md) |

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
| memory | `memory_write` / `memory_read` | **shipped** | Explicit project/agent-private/team descriptor keyed by durable source workspace; team is policy-gated. See [Durable memory scopes](MEMORY_SCOPES.md). |
| web_fetch / WebFetch | `web_fetch` | **shipped** | Offline stub; live HTTP when online; **SSRF preflight** (#179) |
| explore subagent | `spawn_explore` | **shipped** | Read-only survey |
| general-purpose / plan subagent | `spawn_general_purpose` / `spawn_subagent` | **shipped** | Parallel GP; mutating children get separate worktrees/copies by default, plan mode shares cwd read-only ([details](SUBAGENT_ISOLATION.md)) |
| MCP tools | `mcp__server__tool` | **shipped** | Stdio servers only |
| kill_task / task_output | background task cancel + shell cancel | **shipped** (partial) | Via Tasks panel / `cancel_background_task` / turn cancel — not full Build IDs |
| notifications | desktop OS notifications | **deferred** | Track under residual; no ship issue yet |
| notebook | — | **deferred** | Not planned in bridge |
| browser / computer use (Grok Build tool loop) | — | **deferred** | No `computer` / `browser` tool in the thin Build loop. This is **not** the native Computer Use adapter. |
| Computer Use — native semantic adapter + cockpit | (not a Build-loop tool) | **on main (not Build loop)** | Consented semantic **foreground** Computer Use exists on main (Experimental in the capability matrix). Not isolated or background-safe. Design: [COMPUTER_USE.md](COMPUTER_USE.md). Isolated visual ([#288](https://github.com/chriscase/GrokPtah/issues/288)) is a mandatory unmet product exit. Raw global input is Explicitly unsupported. |
| Computer Run MCP **reads** | `ptah_list_computer_runs`, `ptah_get_computer_run`, `ptah_get_computer_run_events`, `ptah_get_computer_capacity` | **on main (not Build loop)** | Read-only Computer Run surfaces on main. MCP **mutations** remain unsupported ([#271](https://github.com/chriscase/GrokPtah/issues/271) **open**). |
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
| read/search/todo | No prompt | File tools use execution cwd |
| memory_read | No prompt | Explicit authorized scope under the Lane's durable source workspace, never an isolated execution cwd |
| memory_write | Prompt unless YOLO; explicit deny always wins | Durable mutation; denied in `read-only`, denied to plan agents, and subject to PreToolUse hooks |

## Offline test hooks

Deterministic offline prompts (`GROKPTAH_AGENT_OFFLINE=1`):

- `list files` → `list_dir`
- `write path: content` → `write_file`
- `run <cmd>` → shell
- `todo <text|json>` → `todo_write`
- `remember <fact>` / `recall <q>` → memory tools
- `patch <json|Update File>` → `apply_patch`
- `web_fetch <url>` → offline stub (still SSRF-checked)
