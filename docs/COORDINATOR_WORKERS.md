# Coordinator and worker delegation

Issue #307 adds the first coordinator/worker vertical slice on top of durable
Agents, Work, and routines. The harness remains the source of truth. Models
may propose decomposition or answers, but every mutation is a validated
workload or message operation.

## Identity

A worker is a durable Agent, not a connected socket. The projection is built
from:

- `AgentRecord` / `AgentSpec` for identity, workspace, declared tools, model,
  and policy limits;
- `WorkerPresence` heartbeats for liveness;
- Work items and attempts for load and lease ownership.

A live heartbeat is not a lease. Lease ownership is only an active
`WorkAttempt` whose claimant is that Agent.

## Declared versus measured capability

- **Declared** capability is the captured Agent specification: allowed tools,
  model route, and run bounds.
- **Measured** capability is optional (`measured: null` until a qualification
  record exists). Absence is explicit; it is not inferred from a TCP session.

Assignment intersects manager bounds, worker bounds, and the server ceiling.
Computer Use workers are rejected in this slice. A manager cannot grant bounds
or authority it does not itself possess.

## Delegation

Parent/child Work uses `parentWorkId`. Assignment states:

| Status | Claimable? |
| --- | --- |
| `unassigned` | yes |
| `offered` | no, until accept |
| `accepted` | yes, only by the assigned Agent |
| `declined` | yes, unassigned |

Every offer, accept, decline, reassign, reprioritize, block, cancel, and
review request writes an attributable `WorkDecision` (actor, reason,
timestamp, work revision). Existing `ptah_assign_work` remains the direct
assign path and records `accepted`.

Concurrent reassignment and cancellation are serialized by the store lock and
revision fences. A stale `expected_revision` fails closed.

## Messages

Messages are durable records, not a second execution queue. Kinds: `status`,
`question`, `answer`, `instruction`, `handoff`, `review_request`,
`review_result`, `informational`.

- Ordering is a single store-wide sequence; inbox/outbox are filtered views.
- Cursor reads use `after_seq`. A cursor below the retained window returns
  `cursor_expired`.
- Send and ack are idempotent through the existing request-id ledger.
- Questions expire after 15 minutes unless overridden. Answering an expired
  question fails closed.
- Payloads are bounded to 8 KiB and secret keys are redacted.
- Cross-workspace send/list is rejected.

## Late responses

If a lease expires, the attempt is recorded `expired` and the Work returns to
`queued` when policy allows. A later complete/fail with the old lease token
fails (`work lease is no longer active`). The worker must claim again.

## Message-triggered activation

Sending a message does not create Work and does not poll an inbox. A future
adapter may fire `RoutineTrigger::External { Message }` through the routine
activation boundary. That adapter is reserved and unsupported in this slice.

## Recovery

Reopening the store preserves workers, decisions, messages, and Work. MCP
clients reconnect and resume from cursors. Replaying the same `request_id`
returns the original Work or message and creates no duplicate.
