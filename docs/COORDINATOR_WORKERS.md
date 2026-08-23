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

Lease-scoped renew, link, progress, release, completion, and failure calls
also re-check the durable attempt claimant for a bound credential; possession
of a lease token alone cannot turn one worker bearer into another worker.

### Authenticated principal versus Agent resource

Named control-plane credentials (`AuthCredential`) share one service account
(`owner_id`). A credential may remain coordinator-scoped, or may be bound to
exactly one durable Agent with `with_agent_binding`. Bound credentials cannot
name another Agent, and omitted worker identities resolve to their bound
identity. This narrows impersonation without treating a bearer as proof that
the Agent resource exists; the normal session/workspace/active-identity checks
still apply. Production-shaped credential issuance and the long-running
multi-worker evidence remain open.

That does not let a caller invent or impersonate an Agent identity:

| Field | Source | Meaning |
| --- | --- | --- |
| `from_actor` / `actor_id` / `credential_id` | authenticated `token_id` | The bearer that performed the mutation |
| `from_agent_id` / `to_agent_id` / `actor_agent_id` / `agent_id` | request, then store | Durable Agent **resource** the principal named |

The service never copies a caller-supplied Agent id into `from_actor`. An
unbound coordinator credential that names `from_agent_id` is recorded as that
credential **acting on behalf of** the Agent, not as the Agent itself. A bound
worker credential cannot make that cross-identity request.

Every referenced Agent is loaded under the store lock and must:

- exist as a durable `AgentRecord`;
- belong to the requested session (`known_lane_ids`) and workspace;
- still be an active identity (`Active`, `Waiting`, or `Interrupted`).
  `Failed` and `Completed` Agents are rejected.

Unknown, cross-workspace, and inactive Agent ids fail closed. Message
acknowledgement validates session and workspace **before** writing
`acked_at` / `acked_by` (`ack_message_scoped`).

### Coordinator versus worker operations

- **Coordinator mutations** (`offer`, `reassign`, and the existing
  block/reprioritize/review paths) authorize the caller as an operator. The
  named worker is an assignment target. Optional `manager_agent_id` is a
  resource used for privilege-amplification checks and audit; it is not proof
  that the caller *is* that manager.
- **Worker mutations** (`accept`, `decline`, `heartbeat`) take a worker
  `agent_id` that must pass the same in-scope/active checks. Accept/decline
  additionally require the Work to be offered to that Agent. The authenticated
  principal remains `actor_id`.

Bounded-authority assignment is unchanged: Computer Use workers are rejected,
and a manager cannot grant bounds it does not itself possess.

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
review request writes an attributable `WorkDecision` (authenticated `actor_id`,
reason, timestamp, work revision). When the request names an Agent,
`actor_agent_id` and `policy_revision` (`AgentSpec.revision` of the acting
Agent, else of the assigned worker) are populated. Block, reprioritize, and
review in this slice still omit those two fields because those APIs do not
take an acting Agent; follow-up below.

Existing `ptah_assign_work` remains the direct assign path and records
`accepted`.

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
- Cross-workspace send/list/ack is rejected. Ack does not mutate a
  foreign message.
- `from_agent_id` / `to_agent_id` must name in-scope, active Agents.

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

## Follow-up

- **Production-shaped worker credential issuance.** The runtime now supports
  binding a credential to one Agent and rejects cross-identity worker and
  message operations. A deployment still needs a real issuance/rotation
  workflow plus independent multi-worker evidence before this is a release
  claim.
- **Coordinator-only versus worker-only tools.** Keep the current
  operator-equivalent bearer model until that binding exists; do not split
  the control-plane credential set in this slice.
- **`WorkDecision` attribution on block / reprioritize / review.** Populate
  `actor_agent_id` and `policy_revision` once those APIs accept an optional
  acting Agent. They remain absent here because the request has no Agent
  resource to attribute.
