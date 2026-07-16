# ADR-001: Build agent runtime strategy

**Status:** Accepted  
**Date:** 2026-07-16  
**Issues:** #77, #78

## Context

GrokPtah forked Grok Build to ship a **desktop** coding agent. Phases 0–14 delivered chrome (sessions, UI, permissions UI, panes). Build turns initially used keyword tools + one-shot chat; then a **thin OpenAI-style tool loop** in `grokptah-agent-bridge` (#76).

Upstream still has a full stack (`xai-grok-shell`, `xai-grok-tools`, `xai-grok-mcp`, `xai-grok-compaction`, …). The bridge is an **independent Cargo workspace** and cannot cheaply depend on the monorepo workspace crates without a large packaging restructure.

## Decision

**Hybrid (option B), with a path to full embed (option A):**

1. **Near term (Phase 15 P0–P1):** Deepen the in-process thin loop in `grokptah-agent-bridge`:
   - Real multi-round tool calling (done)
   - Richer local tools (glob, apply_patch, …)
   - Project instructions, effort on the wire, retries, sandbox gates
   - Keep ACP-shaped `SessionUpdate` events for the desktop UI

2. **Medium term:** Prefer **lifting** individual upstream modules (tool schemas, compaction helpers, MCP client) behind thin adapters rather than rewriting forever.

3. **Long term (optional):** Re-home the bridge into the monorepo workspace and embed `xai-grok-shell` / agent builder if packaging allows; ADR revision required.

**Reject for now:** Pure reimplementation forever (option C) without evals.

## Consequences

- Faster iteration and fewer dependency graph failures.
- Temporary tool/behavior drift vs CLI until eval harness (#93) and tool matrix (#79) close gaps.
- Desktop can stay ahead on multi-session UX while agent quality climbs.

## Spike result

- cli-chat-proxy accepts `tools` / `tool_calls` on chat completions (verified).
- Build path runs multi-round loop with local tools + permissions.
- P0: instructions, effort, glob/apply_patch, retries, sandbox gates, evals docs.
- P1: SSE token streaming during model steps, skill body inject, cancel mid-HTTP,
  basic stdio MCP tool advertise/dispatch (`mcp__server__tool`), live `FileEdit`
  events into the git/diff pane.
- P2: real plan propose/accept→execute, PreToolUse hooks, agent-critical slash
  commands, sandbox profiles (read-only / workspace-write / full), explore
  subagent (`/explore` + `spawn_explore`).
- Remainder: tool matrix + todo/memory/web_fetch; LLM/extractive compact +
  auto-compact; project memory; parity smoke tests; AgentProgress/RateLimited
  + export transcript / last-edit UI.
