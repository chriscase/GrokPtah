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
rejected. The supervisor never sets global auto-approve.

## Dispatcher ownership

The runtime-home process is the only dispatcher:

- desktop-embedded `OrchestrationService`
- locally hosted `grokptah-service`
- cloud/VM-hosted `grokptah-service`

Desktop refresh timers are not dispatchers. Multiple MCP clients may observe
and control one home. They do not become extra dispatch owners. The exclusive
orchestration store lock (`ADR-002`) prevents two processes from opening the
same home.

## Work-attempt / Run relationship

A `ManagedExecutionIntent` binds, under the store lock:

- Agent ID and AgentSpec revision
- Work ID and revision
- WorkAttempt ID and lease
- Run ID once admitted
- source Routine/Activation when present
- model route, intersected bounds, and input hash

There is never more than one live Run for one Work attempt. Duplicate
supervisor ticks and request-id replays return the committed relationship.

## Finite invocation and checkpoints

Each Work attempt creates a **new** finite Run through the existing
`submit_task` admission path. Interrupted model invocations are never
resumed. A verified checkpoint may be included as bounded context, but the
Run is still a new invocation.

Input is assembled only from durable sources: Work objective and policy,
AgentSpec revision, parent Work, bounded messages, and routine/activation
metadata. The focused Lane transcript, desktop model picker, and inbox
polling are not inputs.

## State mapping

| Run | Work |
| --- | --- |
| queued / running | leased / running |
| progress event | Work progress |
| completed | succeeded, or awaiting approval |
| failed / limit | failed or retryable per policy |
| permission/input | awaiting input + durable question |
| cancelled | Work cancelled; late writes fail |
| interrupted | no auto-resume; lease expiry/retry |

## Restart and retry

Crash recovery:

- intent with no Run → abandon the incomplete claim; Work is queued again
- admitted Run still live → heartbeat the existing lease
- interrupted Run → do not resume; Work retry policy decides the next attempt
- terminal Run → complete/fail/cancel the Work attempt

Late completion after lease expiry is rejected. Retry creates a new attempt
and a new Run only when policy allows.

## Approval and input

- `requiresApproval` on Work still pauses completion for `ptah_approve_work`
- `requiresApprovalBeforeExecution` requires `ptah_authorize_work_execution`
- Permission requests park Work as `awaiting_input` and emit a question
  message. They are never auto-approved. `ptah_resolve_work_input` forwards
  the operator decision to the existing host permission oneshot.

## Local versus hosted

The same bridge supervisor runs in desktop-owned homes and hosted services.
`ptah_get_capacity.health.nativeExecutor` and `/ready`
(`nativeExecutorError`) expose health. Desktop shows the policy state and
explicit enable/disable; it does not own dispatch.

## Operational recovery

1. Inspect `ptah_get_capacity` and `ptah_list_execution_intents`
2. If the supervisor reports an error, `/ready` fails closed
3. Reopen the home in one process only
4. Abandoned claiming intents return Work to `queued`
5. Interrupted Runs wait for lease expiry or an explicit retry

## Follow-up

- Manager-agent planning and decomposition
- Message-triggered routine activation
- Per-principal worker credentials bound to one Agent
- Computer Use for unattended Agents (not in this slice)
