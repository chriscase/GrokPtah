# Native persistent-Agent execution

This slice lets the runtime-home owner execute eligible durable Work for a
persistent Agent without a focused desktop session or an external MCP worker.

Path:

`Routine or coordinator → durable Work → native Agent worker → finite bounded Run → durable result`

## Opt-in policy

Managed execution lives on `AgentSpec.managed_execution`. Legacy records
deserialize as disabled. Enabling it requires `revise_agent_spec` /
`ptah_set_managed_execution`, which writes an attributable revision.

Defaults:

- `enabled`: false
- `allowedWorkKinds` / `allowedSourceRoutineIds`: empty means unrestricted
  once enabled
- `maxConcurrentRuns`: 1
- finite prompt, round, duration, and token ceilings
- `retryEligible`: false
- `requiresApprovalBeforeExecution`: false

`WorkPolicy.managed_execution` may be `inherit` (default) or `forbid`. Forbid
keeps the item manual-only even when the Agent is enabled.

Authority may only narrow. Computer Use and `bypassPermissions` Agents are
rejected. Enabling managed execution clears `bypassPermissions` on the spec.
The supervisor never sets global auto-approve and never grants Computer Use.

Manager-decision Work is a stricter native-execution subtype. Before spawning
the provider task, the native executor installs a host-owned proposal-only
capability on the exact admitted Run; the permission gate then auto-denies
every tool from the first model event. The durable Work → intent → Run link is
the audit trail, not the enforcement lookup. The decision still uses the
Agent's captured, provider-agnostic model route and finite bounds, but model
output can only become a typed proposal for the manager applicator.

### `retryEligible`

This flag is independent of Work retry policy and is enforced on every native
admission and recovery path.

- `false` (default): native managed execution must not automatically admit
  attempt 2 or later. Failure, lease expiry, interruption, and process restart
  close the current intent and leave Work in an inspectable terminal `failed`
  state unless Work is already cancelled or succeeded.
- `true`: the native executor may admit a **new** attempt and a **new** finite
  Run only when Work retry policy still has budget for that cause
  (`retryFailed` for failed/limit, `retryExpired` for interrupted/expired) and
  `attempt_count < maxAttempts`.

`ManagedExecutionPolicy.allows_auto_retry` is the single predicate for
**native auto-admission**. `retryEligible: false` does not rewrite a Work
item that an operator reopened with `ptah_retry_work`: that item stays
`queued` and claimable by an external/manual worker. The native supervisor
skips it without mutating it and without creating another Run. The original
failed native attempt remains inspectable.

## Dispatcher ownership

The runtime-home process is the only dispatcher:

- desktop-embedded `OrchestrationService`
- locally hosted `grokptah-service`
- cloud/VM-hosted `grokptah-service`

Desktop refresh timers are not dispatchers. Multiple MCP clients may observe
and control one home. They do not become extra dispatch owners. The exclusive
orchestration store lock (`ADR-002`) prevents two processes from opening the
same home.

## Provider route and admission accounting

Every native admission records the exact provider identity it routes to
**before** the provider task is spawned. `ManagedExecutionIntent.providerRoute`
carries `{providerId, modelId}` alongside the opaque `modelSelectionKey`, so
the durable record names the route rather than an encoded string.

The route is re-derived from the captured AgentSpec revision and must match
the identity that spec already recorded. A spec whose selection key cannot be
re-parsed, or whose parsed identity disagrees with its stored
`provider_id`/`model_id`, is **not admitted**: the tick counts
`skippedUnroutable` and leaves Work `queued`. There is no default-provider
fallback.

`MAX_CONCURRENT_PROVIDER_RUNS` (4) is a finite host ceiling on live intents
that share one provider identity. It is provider-neutral — the same number
applies to every provider — and it is independent of the per-Agent
`maxConcurrentRuns` (1-4). Because several Agents may share one provider, the
per-Agent ceiling alone leaves a provider unbounded across the home; this
ceiling bounds it. Both ceilings are counted from durable intents in
`claiming`, `admitted`, `parked`, and `resolving`, so duplicate supervisor
ticks and process restarts re-derive the same admission answer. A declined
admission counts `skippedProviderCapacity`, distinct from
`skippedIneligible`, and never mutates the Work item.

Legacy intents written before this record deserialize with no
`providerRoute`. Their provider identity is re-derived from the captured
selection key for accounting, so an upgrade cannot silently under-count live
capacity.

`ptah_list_execution_intents` reports the ceiling and a per-provider live
count under `providerAdmission`. The ceiling is home-wide, but
`liveInScopeByProvider` counts only the intents that caller is already
authorized to read: a Lane never learns another Lane's provider identities or
counts. Home-wide pressure is visible without that disclosure through
`ptah_get_capacity`'s `health.nativeExecutor`, which reports
`skippedProviderCapacity`, `skippedUnroutable`, and
`maxConcurrentProviderRuns`.

The ceiling is a host constant in this slice. Making it operator-configurable
per deployment is deliberate follow-up work, not part of this contract.

### Durable provider quota

Online native Runs also reserve a durable, provider-neutral quota row before
provider execution can begin. The pool key is
`{owner, workspace, provider, credentialFingerprint, class}`; it contains no
bearer material. The closed classes distinguish coding execution, manager
proposals, qualification, and Computer sessions so one authority class never
borrows another class's accounting identity.

The initial host limits are a one-hour fixed window, four in-flight
reservations, 10,000,000 reserved tokens, and 4,096 reserved provider
requests per pool. A Run reserves its finite `maxTotalTokens` and a hard
provider-request ceiling of `maxRounds + 1` (the extra slot is the bounded
recovery-grace step). Compaction, planning, and child-model calls share that
same ceiling. Reaching it records the host-authored
`provider_request_quota_reached` stop; it is not a prompt convention.

Reservation and Run creation occur under the same store lock. Queued
admission uses a `quota-admission-intents` recovery journal; immediate Agent
activation embeds the reservation in the existing activation journal. Store
open completes either side of a partial write, rejects mismatched immutable
identities, settles a terminal Run written before its quota update, and
expires a reservation that has neither a Run nor a recovery intent. A stable
reservation without a Run, or a quota-linked Run without its reservation,
cannot be treated as healthy state.

Authoritative terminal usage consumes `min(reservedTokens, measuredTokens)`
and the measured request count; unused capacity is released. A terminal Run
with complete zero usage is refunded. Missing usage or an unresolved provider
request remains reserved deliberately: without a durable provider-attempt
certainty record, refunding it could turn an accepted-but-unknown request into
free capacity. The provider-attempt phase closes that operationally
conservative case.

## Work-attempt / Run relationship

A `ManagedExecutionIntent` binds, under the store lock:

- Agent ID and AgentSpec revision
- Work ID and revision
- WorkAttempt ID and lease
- Run ID once admitted
- source Routine/Activation when present
- model route, intersected bounds, and input hash

There is never more than one live Run for one Work item. Duplicate supervisor
ticks and request-id replays return the committed relationship. The native
submit uses `intent_id` as `request_id` (`ptah_native_execute`) so an orphan
Run can be rediscovered from the durable idempotency receipt or by scanning
runs for that request ID.

## Intent state machine

| State | Meaning |
| --- | --- |
| `claiming` | Durable intent exists; claim and/or Run admission may still be in flight |
| `admitted` | A finite Run is linked; the executor heartbeats the lease |
| `parked` | Run asked for permission; Work and attempt are `awaiting_input` |
| `resolving` | Operator resolve is in flight; host oneshot not yet committed |
| `finalized` | Terminal for this intent; does not consume concurrency |
| `abandoned` | Admission did not commit a Run; any claim was released |

Live concurrency counts `claiming`, `admitted`, `parked`, and `resolving`.

### Admission sequence

1. Write intent `claiming` (no attempt, no Run)
2. Claim Work (attempt + lease)
3. Persist `attemptId` on the intent
4. `submit_task` with `request_id = intent_id`
5. `link_work_run`
6. Persist `runId` and `admitted`

### Claiming recovery (atomic admission)

Recovery of a `claiming` intent **first** discovers any Run already created
for that intent/request ID (`intent.runId`, then the idempotency receipt, then
`find_run_by_request_id`). It never blindly releases a Work claim when a Run
already exists.

| Crash window | Recovery |
| --- | --- |
| Intent written, before Work claim | Abandon intent. Work stays `queued`. No Run. |
| Work claimed, before submit | Abandon intent and release the attempt. Work returns to `queued`. |
| Submit committed, before WorkAttempt link | Adopt the Run, link the attempt, mark `admitted`. |
| WorkAttempt linked, before intent `runId`/`admitted` | Adopt the same Run (link is idempotent), mark `admitted`. |

If `submit_task` returns an error after the Run was persisted, the supervisor
reconciles instead of releasing. A later tick therefore sees at most one live
Run and one valid Work-attempt relationship.

### Interrupted-Run recovery

An `admitted` or `parked` intent whose Run is `interrupted` is never left
live. The executor:

1. Does **not** resume the interrupted model invocation
2. Closes the old intent (`finalized`) and the Work attempt (`expired`)
3. Applies **both** `retryEligible` and Work retry (`retryExpired`)
4. Creates a new attempt and finite Run only when both permit
5. Otherwise leaves Work `failed` with an inspectable result

The same close path is used for failed/limit Runs (`retryFailed`) and for
restart (`OrchStore::open` marks unfinished Runs `interrupted`, then the
native tick runs the transition above). Capacity cannot leak: a finalized
intent no longer counts toward `maxConcurrentRuns`.

## Finite invocation and checkpoints

Each Work attempt creates a **new** finite Run through the existing
`submit_task` admission path. Interrupted model invocations are never
resumed. A verified checkpoint may be included as bounded context, but the
Run is still a new invocation.

Input is assembled only from durable sources: Work objective and policy,
AgentSpec revision, parent Work, **relevant** messages, and
routine/activation metadata. The focused Lane transcript, desktop model
picker, and inbox polling are not inputs.

### Prompt bounds

`assemble_managed_run_input` receives the **intersected** `RunBounds`
(server ceiling ∩ Agent default ∩ managed policy ∩ Work policy). The
assembled prompt is valid UTF-8 whose complete byte length, including the
truncation marker, is at or below `min(effective.maxPromptBytes,
MAX_MANAGED_CONTEXT_BYTES)` and never panics on a multi-byte boundary.
Very small limits omit the marker when it cannot fit and truncate the
source at a character boundary instead of panicking.

### Message context

The executor does **not** inject the first 16 messages from the entire Lane.
`list_messages(afterSeq=0)` returns the oldest page, so the supervisor
loads `list_recent_messages` (the newest retained window, up to 200 of
the 500 retained records) and then `select_relevant_managed_messages`:

- keep messages whose `workId` is this Work
- keep messages with no `workId` that are from or to the assigned Agent
- drop unrelated Work and other-Agent traffic
- drop expired questions
- collapse duplicate same-thread / same-kind / same-body material
- keep the newest matching records, then emit them in deterministic `seq` order

## State mapping

| Run | Work |
| --- | --- |
| queued / running | leased / running |
| progress event | Work progress |
| completed | succeeded, or awaiting approval |
| failed / limit | failed, or queued for a new attempt when both retry policies allow |
| permission/input | awaiting input + durable question |
| cancelled | Work cancelled; late writes fail |
| interrupted | intent finalized; Work failed or re-queued per both retry policies; no auto-resume |

## Restart and retry

Crash recovery:

- `claiming` with no Run → abandon; Work is queued again
- `claiming` with a Run already committed for `intent_id` → adopt that Run
- admitted Run still live → heartbeat the existing lease
- interrupted Run → close intent/attempt; never resume; both retry policies
  decide whether a **new** attempt is eligible
- terminal Run → complete/fail/cancel the Work attempt and finalize the intent

Late completion after lease expiry is rejected. Retry creates a new attempt
and a new Run only when policy allows. Native admission never loops a
forbidden retry: queued Work with `attempt_count >= 1` and
`retryEligible = false` is skipped, not sealed, so a manual `ptah_retry_work`
remains claimable by an external worker.

Completed, failed, cancelled, and interrupted closes write a
`managed-finalization` journal first, then the attempt, Work, and intent.
Store open and supervisor ticks replay leftover journals. Replay never
overwrites cancelled or succeeded Work, always finalizes the intent, and
releases concurrency. Partial writes (journal only; attempt only; Work
without intent) converge to the same durable outcome.

## Approval and input

- `requiresApproval` on Work still pauses completion for `ptah_approve_work`
- `requiresApprovalBeforeExecution` requires `ptah_authorize_work_execution`
- Permission requests park Work as `awaiting_input` and emit a question
  message. They are never auto-approved.

Each host `PermissionRequest` carries the in-flight Run ID from the
session's turn tracker. The native executor parks an intent only when
`request.runId` equals `ManagedExecutionIntent.runId`. It never guesses
from session identity or insertion order.

`ptah_resolve_work_input`:

1. Inspects the parked/resolving intent for exact session and workspace
   match without mutation
2. Requires a genuine in-memory host pending permission whose session and
   Run match, with a live oneshot receiver
3. Marks the intent `resolving`
4. Signals `permission_respond`
5. Commits Work and attempt back to `running` and the intent to `admitted`

If the host permission is missing, stale, cancelled, bound to another Run,
or its receiver is gone, the call fails and durable state stays parked.
A failed host signal aborts `resolving` back to `parked`. Replay of an
already-resolved permission ID is a conflict. Process restart drops
in-memory pending permissions; resolve then fails honestly rather than
unparking. Supervisor recovery of `resolving` also fails closed: it aborts
back to `parked` (and finalizes only if the Run is already
interrupted/terminal). A missing host entry is not treated as a delivered
decision, because `permission_respond` removes the in-memory oneshot before
send.

## Current limitations

- The host still admits **one in-flight turn per session**. Native admission
  uses the existing `submit_task` path with `allow_queue: false`, so a busy
  session is `session_busy` rather than a second concurrent turn.
- Permission oneshots are process memory. They are not restart-durable.
  Resolution after restart, cancel, or a dead receiver fails closed.
- Named control-plane credentials remain **operator-equivalent**. They share
  one service `owner_id` and are not bound to a single Agent.
- `maxConcurrentRuns` is bounded to **1–4** (default 1). Live intents in
  `claiming`, `admitted`, `parked`, and `resolving` consume that ceiling.

## Local versus hosted

The same bridge supervisor runs in desktop-owned homes and hosted services.
`ptah_get_capacity.health.nativeExecutor` and `/ready`
(`nativeExecutorError`) expose health. Desktop shows the policy state and
explicit enable/disable; it does not own dispatch.

## Operational recovery

1. Inspect `ptah_get_capacity` and `ptah_list_execution_intents`
2. If the supervisor reports an error, `/ready` fails closed
3. Reopen the home in one process only
4. Abandoned claiming intents return Work to `queued` when no Run exists
5. Claiming intents that already have a Run are adopted, not released
6. Interrupted Runs are closed; a new attempt is admitted only when both
   retry policies allow

## Follow-up

- Operator-configurable per-provider admission ceiling
- Operator-configurable quota pools, spend conversion, and operator projections
- Manager-agent planning and decomposition
- Message-triggered routine activation
- Per-principal worker credentials bound to one Agent
- Computer Use for unattended Agents (not in this slice)
