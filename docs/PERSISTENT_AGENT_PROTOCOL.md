# Persistent agent protocol

This document defines the first transport-neutral continuity slice for
GrokPtah Build agents.

## Boundary

The persistent-agent contract is split into four replaceable concerns:

1. `orchestration::types` defines durable identities, runs, checkpoints, and
   resume validation.
2. `OrchStore` provides atomic filesystem persistence, crash recovery, and
   idempotency receipts.
3. `AgentHostHandle` is the current desktop runtime adapter. It binds a Build
   session to an agent, records run lineage, creates redacted checkpoints, and
   exposes an explicit resume operation.
4. Tauri commands are only an IPC adapter over the host seam. They do not own
   identity, checkpoint validation, or lifecycle policy.

A future VM/service adapter can implement the same domain contract with a
different transport and storage backend. It must preserve the validation
rules and the explicit-operator-resume boundary.

## Durable records

- `AgentRecord` identifies one Build agent by explicit `agent_id`, session,
  workspace, model, lifecycle state, and latest checkpoint.
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

## Lifecycle rules

- Opening the store converts queued/running runs to `interrupted` and marks
  their bound agents interrupted. The latest verified checkpoint is retained.
- A terminal desktop Build run emits one new checkpoint and returns its agent
  to `waiting` (or `failed` for a failed run).
- Resume is manual. The caller supplies a fresh prompt. The host validates
  agent/session/workspace/checkpoint identity, injects the bounded checkpoint
  context as auditable system context, and links the new run with
  `parent_run_id`.
- An optional request id is protected by the existing durable idempotency
  ledger. Exact retries replay the response; changed payloads cannot create a
  second run.

## Explicit non-goals

This slice does not add a scheduler, unattended auto-resume, headless
authority, VM deployment, auto-promotion, or broader Computer Use
permissions. Those require a separate authority decision and an observable
need described in [ADR-002](ADR-002-runtime-boundaries.md).
