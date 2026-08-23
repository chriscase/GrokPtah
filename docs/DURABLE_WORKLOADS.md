# Durable workloads

Issue #305 adds the first transport-neutral workload contract to the shared
`grokptah-agent-bridge` runtime. It is the common contract for a local
desktop, a local service, and a hosted service; those hosts must not maintain
separate workload semantics or copies of the ledger.

## The ownership model

| Record | Durable responsibility | Product projection |
| --- | --- | --- |
| `WorkItem` | The intent, policy, dependencies, deadline, retry budget, current state, and result | A workload card/list item |
| `WorkAttempt` | One claimant's lease, heartbeat, linked finite Runs, progress, and terminal outcome | Attempt history and live ownership |
| Agent identity | Long-lived persona, authority, and memory identity | Persistent-agent area |
| Session/Lane | Build context and workspace scope | A frequently archived development lane |
| Run | One bounded execution of an attempt | Run inspector, events, tests, diff, and approval UI |

Archiving a Lane changes its visibility and blocks new mutations through the
service boundary. It does not delete or hide its durable workload history from
authorized reads, and it does not retire the Agent identity. A routine activation
may attach a new WorkItem to the same Agent and Lane without changing this
ownership model. See [DURABLE_ROUTINES.md](DURABLE_ROUTINES.md) and
[COORDINATOR_WORKERS.md](COORDINATOR_WORKERS.md).

## State and lease contract

Work starts in `queued`. Missing or unsuccessful dependencies make it
`blocked`; once all dependencies are `succeeded`, it becomes claimable again.
A successful claim creates an attempt and moves the item to `leased`.
Progress moves both item and attempt to `running`. Completion moves to
`succeeded`, unless `requiresApproval` is set, in which case the item and
attempt stop at `awaiting_approval`. Failure either consumes a retry and
returns to `queued` or becomes `failed`. Release returns an active attempt to
`queued`; cancellation is terminal.

Claims are lease-token scoped. The raw token is returned only in the claim
response; only a hash is stored, and the hash is omitted from all API,
desktop, and durable idempotency projections. A replay reconstructs the token
from the authenticated service credential and durable attempt identity, then
verifies it against the stored hash. An expired active lease is recorded as an
`expired` attempt and can be retried when policy permits.

The current store is a single-owner, file-backed JSON ledger protected by the
existing exclusive store lock and atomic writes. It supports crash/reopen
recovery and serializes competing claims. It intentionally accepts only one
concurrent attempt per WorkItem in this first slice; a multi-node scheduler,
database backend, and approval-decision operation remain follow-on work.
Named bearers can be narrowed to one durable Agent identity with
`AuthCredential::with_agent_binding`; the service rejects cross-agent worker,
heartbeat, assignment, and message mutations for such a bearer. Credential
issuance/rotation and an independent long-running multi-worker proof are still
required for the Stage 6 release gate.

## Service reconciliation

Opening the ledger performs one recovery pass, and every live
`OrchestrationService` starts the same transport-neutral workload supervisor.
The supervisor runs every five seconds in both the desktop's embedded control
plane and `grokptah-service`. It does not execute model turns or invent a
second queue; it only reconciles durable state:

- closes active attempts whose leases expired while a client or process was
  disconnected;
- returns retryable work to `queued`, or records terminal failure when its
  retry budget is exhausted;
- applies dependency blocking/unblocking and deadline failure; and
- leaves Agent identities and archived Lanes untouched.

`ptah_get_capacity` exposes the last reconciliation timestamp, outcome counts,
and any supervisor error under `health.workloadSupervisor`. `/ready` fails
closed when the latest pass reports an error. This is a single-process,
single-writer supervisor today: hosted deployments gain the same recovery
semantics, but multi-node ownership still requires the future database-backed
coordinator boundary described in ADR-002.

Native persistent-Agent execution is a separate, opt-in dispatcher in the same
process. See [NATIVE_AGENT_EXECUTION.md](NATIVE_AGENT_EXECUTION.md).

## MCP/service surface

Read tools:

- `ptah_list_work(session_id, workspace)`
- `ptah_get_work(session_id, workspace, work_id)`

Mutation tools:

- `ptah_create_work`
- `ptah_assign_work` (human/coordinator owner change; never starts execution)
- `ptah_claim_work`
- `ptah_renew_work`
- `ptah_link_work_run`
- `ptah_report_work_progress`
- `ptah_release_work`
- `ptah_complete_work`
- `ptah_fail_work`
- `ptah_cancel_work`
- `ptah_retry_work` (explicitly reopens a failed item only within its retry budget)
- `ptah_approve_work` (human decision for an approval-gated completion)

Mutating calls use the existing durable request-id/idempotency mechanism. A
replayed request returns the original response; the same request ID with a
different payload is rejected. Unbound coordinator credentials may act on
behalf of in-scope Agent resources and remain attributable by auth token.
Bound worker credentials resolve omitted claimant identity to their bound
Agent and fail closed on cross-agent requests. Per-principal issuance,
rotation, and the independent multi-worker outcome remain separate release
milestones.

The desktop's remote-service adapter advertises and decodes the two read tools
into typed `DurableWorkItem`, `DurableWorkAttempt`, and `RemoteWorkSnapshot`
projections. It uses the same authenticated MCP boundary as other remote
sessions and runs, so local and hosted deployments share the same wire
contract. The desktop Work view adds a human-reviewed control surface for
creating, assigning, retrying, approving, and cancelling Work Items. Local
actions call the embedded ledger directly; hosted actions use fresh
idempotent MCP request IDs. Neither path exposes lease tokens or worker-only
claim/progress/completion controls to the UI. Every human mutation can carry a
revision fence, and stale UI actions fail with `stale_version` rather than
silently overwriting newer state.

Approval decisions are durable and attributable. An approval-gated worker
completion remains `awaiting_approval` until an authenticated operator calls
`ptah_approve_work`; the resulting reviewer identity, optional note, and time
are retained on the Work Item. Assignment never claims a lease, retry never
exceeds `maxAttempts`, and cancel remains terminal. Archived Lanes remain
readable but reject all of these mutation paths.

## Conformance evidence

The bridge integration tests cover:

- store reopen after an expired lease;
- exactly one winner in a concurrent claim race;
- dependency blocking and unblocking;
- approval-aware completion;
- duplicate claims, wrong tokens, and durable attempt history;
- create and claim idempotency, including conflicting replay rejection;
- omission of `leaseTokenHash` from protocol responses;
- progress and completion through the live loopback MCP server;
- authorized reads surviving Lane archival and mutation rejection after archive.
- deterministic lease/deadline reconciliation and supervisor status;
- service restart shutdown/reopen without a lingering ledger lock.
