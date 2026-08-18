# Persistent agent protocol

This document defines the transport-neutral identity, policy, and continuity
contract for GrokPtah Build agents.

## Boundary

The persistent-agent contract is split into four replaceable concerns:

1. `orchestration::types` defines durable identities, runs, checkpoints, and
   resume validation.
2. `OrchStore` provides atomic filesystem persistence, crash recovery, and
   idempotency receipts.
3. `AgentHostHandle` is a runtime adapter. It binds Lanes to an Agent, applies
   the intersection of captured Agent authority and current host policy,
   records run lineage, creates checkpoints, and exposes explicit resume.
4. Desktop, service, and MCP commands are adapters over the same host seam.
   They do not own
   identity, checkpoint validation, or lifecycle policy.

A future VM/service adapter can implement the same domain contract with a
different transport and storage backend. It must preserve the validation
rules and the explicit-operator-resume boundary.

## Durable records

- `AgentSpec` is an immutable, attributable revision of an Agent's display
  identity, role, durable source workspace, qualified provider/model route,
  default finite `RunBounds`, maximum tool authority, and memory memberships.
  Revisions are append-only under `agent-specs/<agent>/<revision>.json`.
- `AgentRecord` points at the current specification and stores mutable runtime
  state: active/last Run references, latest checkpoint, continuation ordinal,
  and attributable Lane associations. `session_id`, `lane_ids`, `workspace`,
  and `model` remain compatibility projections during migration.
- `AgentLaneAssociation` is independent of Lane archival. Archiving a Lane
  never retires its Agent or deletes identity, memory, Runs, or checkpoints.
- `RunRecord.agent_id` links a run to its agent.
- `RunRecord.parent_run_id` links a continuation to the run that produced its
  verified checkpoint. It is separate from `retry_of`, which means an explicit
  replacement of an interrupted run.
- `ContinuationCheckpoint` stores a bounded redacted context summary, event
  sequence, ordinal, parent checkpoint, and a hash over its identity and
  context. A tampered checkpoint is rejected before it can be resumed.

The full session transcript remains the durable conversation source. A
checkpoint is a verified continuation boundary and audit aid, not a second
transcript.

Older flat Agent records migrate deterministically to specification revision
1 without changing `agent_id`. Migration preserves their source workspace and
provider/model selection, requires approval for guarded mutations, grants no
new automatic tools, and records `legacy_migration` attribution.

## Authority and model rules

- An Agent executes with its captured provider/model selection. Changing the
  focused desktop model does not change an existing Agent.
- Effective authority is the intersection of the current host policy and the
  current Agent specification. A deny on either side denies; automatic
  approval requires both sides; otherwise the operator is prompted.
- The specification captures the known built-in tool IDs and enabled MCP
  server IDs. Tools or servers introduced later are denied until an explicit
  Agent-spec revision adds them.
- A captured read-only sandbox remains read-only even if the desktop later
  selects a broader profile. A host may always narrow an Agent further.
- Computer Use is denied by default for persistent Agents and requires a
  separate explicit specification revision in addition to the normal
  qualification, grant, observation, and approval checks.
- Model, role, bounds, memory membership, or authority changes require a new
  attributable specification revision. Existing revisions are immutable.
- Unknown or malformed Agent policy and missing Agent records fail closed.

## Lifecycle rules

- Opening the store converts queued/running runs to `interrupted` and marks
  their bound agents interrupted. The latest verified checkpoint is retained.
- A terminal desktop Build run emits one new checkpoint and returns its agent
  to `waiting` (or `failed` for a failed run).
- Resume is manual and always creates a new finite Run. The caller supplies a
  fresh prompt. The host validates Agent/Lane/source-workspace/checkpoint
  identity, injects the bounded checkpoint
  context as auditable system context, and links the new run with
  `parent_run_id`.
- Any currently associated Lane in the Agent's source workspace may be the
  resume target. The legacy primary `session_id` is not an authorization
  boundary.
- An optional request id is protected by the existing durable idempotency
  ledger. Exact retries replay the response; changed payloads cannot create a
  second run.

## Explicit non-goals

This protocol does not add a scheduler, unattended auto-resume, auto-approval,
auto-promotion, or broader Computer Use permissions. A local or hosted service
may host the same records and finite Run contract, but deployment does not
create authority. Those boundaries are described in
[ADR-002](ADR-002-runtime-boundaries.md).
