# Persistent agent protocol

This document defines the transport-neutral identity, policy, and continuity
contract for GrokPtah Build agents.

## Boundary

The persistent-agent contract is split into four replaceable concerns:

1. `orchestration::types` and `orchestration::continuation` define durable
   identities, finite runs, checkpoints, deterministic continuation inputs,
   fidelity, and resume validation.
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
- A continued Run also freezes `agent_spec_revision`, `checkpoint_id`, and the
  content-addressed continuation context ID/hash/fidelity used at admission.
- `ContinuationCheckpoint` stores a bounded redacted context summary, event
  sequence, ordinal, parent checkpoint, and a hash over its identity and
  context. A tampered checkpoint is rejected before it can be resumed.
- `ContinuationInputSnapshot` captures only durable Agent specifications,
  checkpoint provenance, bounded Run lineage and aggregates, scoped memory
  facts, workload references when available, target Lane, effective bounds,
  and the new instruction's byte length/hash. It is sealed by a canonical
  input hash and persisted append-only under `continuation-inputs/`.
- `ContinuationContext` is the exact model-facing UTF-8 byte string assembled
  from that snapshot. It records `complete` or `degraded` fidelity, stable
  reason codes, and a bounded omission ledger. It is content-addressed and
  persisted append-only under `continuation-contexts/`. Failed assembly emits
  no context and creates no Run.

The full session transcript remains a conversation record, but it is not an
input to continuation assembly. A checkpoint is a verified boundary and audit
aid; a continuation snapshot/context is the deterministic finite resume input.
These records have different identities and must not be conflated.

## Deterministic continuation rules

- Assembly is pure after snapshot capture. It does not read the clock,
  network, focused session/model, active tab, ambient working directory,
  desktop permission state, live transcript tail, or live Git state.
- Struct field order and compact JSON serialization are fixed. Dynamic maps
  are excluded from the hashed/rendered schema. Lineage, changes, tests,
  memory scopes/facts, reasons, omissions, and workload references have stable
  byte-order tie breakers.
- Lineage is bounded to eight terminal Runs and follows `parent_run_id` only;
  `retry_of` is never continuation ancestry. Cycles, cross-Agent/workspace
  edges, a missing source Run, or a tampered checkpoint fail closed. A missing
  older retained ancestor degrades with an explicit reason.
- Context is limited to 16 KiB and to the bytes remaining after the new
  instruction under `max_prompt_bytes`. UTF-8 strings are truncated only at
  code-point boundaries, whole low-priority records are evicted
  deterministically, and every omission is counted and hashed. If required
  identity/checkpoint/specification core cannot fit, no Run is created.
- Enabled memory scopes are captured from the Agent specification. An empty
  readable scope is complete; an unreadable, corrupt, or invalid scope is
  omitted as a whole and degrades fidelity. Disabled scopes are absent without
  degradation.
- Reassembling the same persisted snapshot before or after process restart
  produces byte-identical context and hashes.

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

- Run creation and Agent activation are serialized through a durable recovery
  intent. The intent is removed only after both records commit, or after a
  failed admission durably rolls back the Run. Store startup reconciles any
  remaining intent before applying interrupted-run recovery.
- Opening the store converts queued/running runs to `interrupted` and marks
  their bound agents interrupted. The latest verified checkpoint is retained.
- A terminal desktop Build run emits one new checkpoint and returns its agent
  to `waiting` (or `failed` for a failed run).
- Resume is manual and always creates a new finite Run. The caller supplies a
  fresh prompt. The host validates Agent/Lane/source-workspace/checkpoint
  identity, captures and persists a deterministic bounded continuation,
  injects those exact bytes as auditable system context, and links the new Run
  with `parent_run_id`.
- Any currently associated Lane in the Agent's source workspace may be the
  resume target. The legacy primary `session_id` is not an authorization
  boundary.
- An optional request id is protected by the existing durable idempotency
  ledger. The receipt is owned by the stable authenticated principal and bound
  to the target Lane plus canonical workspace; rotating that principal's
  credential preserves replay, but another principal receives an independent
  request namespace. Its request identity includes Agent, target Lane, instruction
  hash/length, and requested round narrowing, so an exact retry still replays
  after the first Run advances the Agent checkpoint. The sealed continuation
  input separately binds source workspace, checkpoint/hash, execution-spec
  revision, effective bounds, assembler version, and input hash. The receipt
  records the actual finite Run ID. Changed request payloads cannot create a
  second Run.
- Direct local embedders retain the compatibility resume methods, which use an
  explicit installation-local owner. Multi-user/service embedders must use the
  scoped variants and supply a stable owner identity across credential rotation.

## Explicit non-goals

Routine activation (#306) may create eligible Work for an Agent. It does not
resume an interrupted model invocation, approve tools, grant Computer Use, or
promote code. See [DURABLE_ROUTINES.md](DURABLE_ROUTINES.md).

This protocol does not add unattended auto-resume, auto-approval,
auto-promotion, or broader Computer Use permissions. A local or hosted service
may host the same records and finite Run contract, but deployment does not
create authority. Those boundaries are described in
[ADR-002](ADR-002-runtime-boundaries.md).
