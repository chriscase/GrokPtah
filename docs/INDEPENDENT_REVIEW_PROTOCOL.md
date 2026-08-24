# Independent Review Protocol

This protocol applies before a cross-product capability surface is treated as
ready for ContextDesk or another consumer.

## Model and lane requirements

- Use a separate Cursor or Claude Code review lane, not the implementer lane.
- Select the strongest available coding/review model and highest practical
  reasoning effort.
- Fast mode must be visibly off.
- Pin the exact candidate head and base revision in the review prompt.
- Use read-only review unless a same-scope correction is explicitly requested.
- Preserve all other local sessions, worktrees, and protected long-running
  qualification processes.

## Review scope for the current integration slice

The reviewer must inspect these files and their dependency boundaries:

- `crates/common/grokptah-agent-sdk/**`
- `crates/codegen/grokptah-agent-bridge/src/capability_contract.rs`
- `crates/codegen/grokptah-agent-bridge/src/mcp_control.rs`
- `crates/codegen/grokptah-agent-bridge/src/mcp_control_client.rs`
- `desktop/src/lib/capabilities.ts`
- `desktop/src/lib/grokptahClient.ts`
- `desktop/src/lib/grokptahOperations.ts`
- `desktop/src/lib/grokptahBrokerClient.ts`
- `docs/WEB_BROKER_PROTOCOL.md`
- `docs/schemas/grokptah-capabilities.v1.schema.json`

## Required findings

The handoff must answer each item with file/line evidence:

1. Does every run/Computer Use mutation retain the exact session, workspace,
   run, request-id, version, and approval/lease fence?
2. Can a browser obtain or reuse a GrokPtah bearer token, filesystem path,
   provider credential, raw prompt, or native Computer Use detail?
3. Do unknown capability versions, malformed descriptors, scope mismatches,
   stale cursors, duplicate requests, and partial SSE frames fail closed?
4. Are broker and MCP error categories share-safe, with privileged diagnostics
   retained only server-side?
5. Are the Rust SDK DTOs host-neutral and compatible with a ContextDesk backend
   adapter without importing desktop or provider authority?
6. Does the published/staging package boundary avoid Tauri and React runtime
   dependencies for transport and contract code?
7. Are promotion and Computer Use controls visibly gated rather than merely
   hidden by the consumer UI?
8. Do tests cover both successful replay and recovery/error paths?

## Completion rule

The reviewer must return `PASS`, `PASS_WITH_FINDINGS`, or `FAIL` with:

- exact candidate/base SHAs;
- changed-file allowlist;
- commands/tests actually run;
- every finding classified by severity;
- explicit statement of what remains unverified;
- no claim that ContextDesk is integrated until a disposable cross-repository
  adapter test passes.

## Prompt template

```text
Perform a read-only independent security/interoperability review of the exact
candidate head <HEAD> against base <BASE>.

Use the strongest available coding-review model at the highest practical
reasoning effort; Fast must be OFF. Do not edit files, run destructive commands,
or touch any other task. Inspect only the listed integration files and direct
dependencies. Verify every item in docs/INDEPENDENT_REVIEW_PROTOCOL.md with
file/line evidence. Pay special attention to browser-vs-MCP authority,
session/workspace/run scope, idempotency, cursor replay/recovery, redaction,
approval/lease fencing, and host-neutral SDK dependency direction.

Return PASS, PASS_WITH_FINDINGS, or FAIL. Include exact SHAs, changed-file
allowlist, commands actually run, severity-ranked findings, and explicit
unverified gates. Do not infer integration or packaged Computer Use from source
types alone.
```
