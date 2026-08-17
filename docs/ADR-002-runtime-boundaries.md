# ADR-002: Runtime boundaries and continuity triggers

**Status:** Accepted
**Date:** 2026-08-16
**Issues:** #300, #301

## Context

GrokPtah already has durable bounded runs, an event journal with cursor replay,
prompt queues, isolated worktrees, completion evidence, and an authenticated
loopback MCP coordinator. The verified continuity and coordinator surfaces are
still anchored by the desktop process: the bridge owns the instance lock,
credentials, permissions, PTY, and the authoritative host state.

Several possible next boundaries have been proposed—durable agent identity,
continuation context, scheduling, a headless service, a published SDK, and a
generic provider abstraction. This ADR makes the conditions for those changes
observable and testable. It does not authorize any of them by itself.

## Decision

### 1. The desktop remains the authority anchor

The in-process bridge remains the runtime authority behind the desktop. The
desktop is the place where credentials, trust, permission decisions, Computer
Use grants, PTY ownership, stop/takeover controls, and review-gated promotion
are anchored.

The authenticated loopback MCP server is a bounded coordinator surface over
that authority. It is not a second host, a public service, or an ownership
override.

### 2. Service and process-boundary triggers

Do not introduce a headless service or a second writer to `~/.grokptah` until
one of these conditions is observed and recorded:

1. Work must survive desktop UI exit or laptop sleep rather than ending with
   the current process.
2. Two processes require concurrent write authority to the same
   `~/.grokptah` home.
3. An off-box client must originate work, requiring a new identity,
   authentication, and authority model.

The exclusive lock in `src/instance_lock.rs` is the concrete boundary for the
second condition. A future service proposal must explain how ownership,
recovery, and migration change before changing that lock. A read-only observer
does not satisfy any service trigger.

### 3. Client authority tiers

The tiers are deliberately asymmetric:

| Tier | May do | May never do |
| --- | --- | --- |
| Desktop authority anchor | Own credentials, local permissions, PTY, Computer Use grants, stop/takeover, and human-gated promotion | Silently widen an agent or coordinator grant |
| Loopback coordinator | Use bounded, authenticated, idempotent session/run reads and mutations, including queue/steering and explicitly scoped run control | Bypass workspace/session ownership, expose secrets, grant Computer Use, or promote without the human review path |
| Read-only observer | Read bounded redacted projections with explicit scope and cursor recovery | Mutate runs, queues, workspaces, permissions, credentials, or promotion state |
| Off-box originator | Nothing by default; permitted only after separate identity and security work | Reuse the loopback token model as remote authority or gain ambient desktop access |

Every new surface must name its tier, scope, mutation authority, redaction
contract, idempotency behavior, and recovery behavior.

### 4. Provider abstraction trigger

The existing OpenAI-compatible `ProviderProfile` shape remains the provider
boundary. Do not introduce a generic `ProviderAdapter` trait while provider
selection, credentials, model IDs, effort, capabilities, and streaming tool
calls can be represented by that profile.

The trigger for a provider trait is a concrete provider API shape that cannot
be expressed without provider-specific lifecycle, transport, authentication,
or capability semantics. The proposal must include an example provider,
the smallest incompatible operation, and conformance tests proving that a
profile cannot represent it. A desire for cleaner abstraction is not a
trigger.

### 5. Extraction thresholds

Keep implementation in the bridge until there is a second real consumer and
evidence that an extraction reduces coupling. The ordering is:

1. stabilize the internal module and protocol;
2. write a protocol specification and conformance tests;
3. extract to a separate crate only when the second consumer exists;
4. publish a library only when versioning and compatibility have a real need.

ContextDesk is not present in this repository and is not a second consumer for
this decision. No ContextDesk-specific integration, SDK, or crate extraction
is planned from this ADR.

### 6. Durable dependencies are by ID

Durable relationships must be stored as explicit IDs on durable records and
resolved from the durable store. They must not be inferred from focused UI
state, a live process, a current session, or the current working directory.

The existing `retry_of` field is the model. Future agent identity and
continuation work must apply the same rule to agent, parent-run, chain, and
dependency references.

### 7. Held seams and their triggers

The following boundaries remain deliberately unfiled until their trigger is
observed:

- **`RunExecutor` seam:** file when continuation-context determinism tests
  cannot be written against `AgentHostHandle`, or when a service trigger from
  this ADR fires.
- **Scheduling/activation:** file only after durable identity exists and the
  service/authority question is decided. It must include bounded admission,
  backoff, cancellation, and a circuit breaker.
- **Observer notifications:** file when a real read-only observer needs
  outbound notification; do not add a remote originator merely to justify
  push delivery.
- **Unattended Computer Use:** remains outside this continuity program and
  follows the Computer Use threat model and release gates under #267/#274.

## Consequences

- Long-horizon work can first be falsified against the existing contract,
  avoiding runtime state that evidence does not justify.
- The desktop remains the visible, stoppable authority anchor.
- Durable identity and continuation may be added without implicitly authorizing
  scheduling, auto-resume, auto-promotion, or permission widening.
- A future boundary proposal must cite the observable trigger it satisfies and
  include the security and recovery changes at that boundary.

## Relationship to ADR-001

ADR-001 remains the decision for the hybrid thin in-process agent loop and its
optional path toward upstream embedding. This ADR adds runtime-boundary and
continuity conditions; it does not supersede or reopen ADR-001.
