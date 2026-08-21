# Durable manager plans

Manager plans are the first bounded coordination layer for persistent Agents.
They let a durable Agent turn an objective into a dependency graph of ordinary
Work items, dispatch ready steps, observe results, and stop for an explicit
re-plan when a step fails.

## One ledger, not another queue

The plan is durable coordination metadata. Executable work remains in the
existing Work ledger and therefore uses the same claim leases, assignments,
messages, reviews, approvals, native executor, and hosted/local storage rules.
The plan's blocked root Work item is only a visible container; it is never
eligible for execution. Child Work items carry the normal `parentWorkId` link.

The same control operations are available through the local and hosted MCP
surfaces:

- `ptah_create_manager_plan`
- `ptah_list_manager_plans`
- `ptah_get_manager_plan`
- `ptah_advance_manager_plan`
- `ptah_tick_manager_plan`
- `ptah_replan_manager_plan`

## State and bounds

Plans have the states `active`, `needs_replan`, `succeeded`, `failed`, and
`cancelled`. Steps are `pending`, `ready`, `in_flight`, `awaiting_input`,
`awaiting_review`, `succeeded`, `failed`, `blocked`, or `cancelled`.

Creation rejects duplicate IDs, unknown dependencies, cycles, out-of-scope
Agent identities, Computer Use workers, and bounds that exceed the manager,
worker, or server policy. A plan contains at most 64 steps, can have at most
16 ready/in-flight steps, and can be re-planned at most 16 times. Every
mutation is request-idempotent and may carry an expected plan revision.

`advance` materializes only steps whose declared dependencies have succeeded.
It does not resume an interrupted model invocation, grant approval, grant
Computer Use, or widen the captured Agent authority. If a child Work fails,
the plan becomes `needs_replan`; no replacement Work is invented implicitly.
The operator or manager must provide a reason and new step specifications via
`replan`, after which a later `advance` can continue the graph.

## Bounded manager tick

`ptah_tick_manager_plan` is the durable observation loop for a plan. It first
advances an active plan using the current Work ledger, then projects each
child's current Work revision into one durable message addressed to the
manager Agent:

- `awaiting_input` becomes a `Question`.
- `awaiting_approval` or `review` becomes a `ReviewRequest`.
- `succeeded`, `failed`, `cancelled`, or `blocked` becomes a `Status` update.

Each step fences the last notified Work revision and message ID. Repeating a
tick, restarting the owning process, or replaying its request therefore does
not create another notification for the same Work revision. A concurrent tick
must use the expected plan revision and fails with a stale-version result. If a
step fails, the tick reports the terminal outcome and leaves the plan in
`needs_replan`; it never invents replacement Work or silently widens authority.

The tick does not execute model Runs. Authorized child Work remains eligible
for the existing native Agent executor, which continues to enforce managed
execution policy, finite Run bounds, permission parking, and the no-resume
rule. A caller may invoke the tick from a local desktop, hosted service, or a
future owner-side timer without introducing a second queue or scheduler.

## Durable identity and Lane lifecycle

`managerAgentId` names a durable Agent resource. It is validated against the
requested session and workspace and recorded as a resource for authority and
audit. It is not treated as proof that the authenticated bearer is that Agent;
the authenticated principal remains the actor in idempotency and audit data.
Archiving a Lane does not archive or detach the Agent identity or its plan
history. Hosted service instances and local desktop instances read the same
plan and Work record shapes.

## Current boundary

The manager remains a bounded coordinator. It observes and routes durable
state; it does not approve tools, grant Computer Use, resume interrupted Runs,
or promote code on its own.
