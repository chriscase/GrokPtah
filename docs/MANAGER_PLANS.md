# Durable manager plans

Manager plans are the first bounded coordination layer for persistent Agents.
They let a durable Agent turn an objective into a dependency graph of ordinary
Work items, dispatch ready steps, observe results, and stop for an explicit
re-plan when a step fails.

The autonomous supervisor is **Experimental**, not a certified always-on
soak, and not a product named Grokbot
([`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md)). Shipped ManagerSupervisor
must not be collapsed with Planned hosted Grokbot certification. It is a
different surface from Computer Use: this supervisor must not grant Computer
Use, and Computer Use non-goals must not be read as denying manager autonomy.

## One ledger, not another queue

The plan is durable coordination metadata. Executable work remains in the
existing Work ledger and therefore uses the same claim leases, assignments,
messages, reviews, approvals, native executor, and hosted/local storage rules.
The plan's root Work item is an explicit host-enforced `isContainer` record;
it is never eligible for reconciliation admission, manual claims, or native
execution. Child Work items carry the normal `parentWorkId` link.

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
`awaiting_review`, `succeeded`, `failed`, `blocked`, `cancelled`, or
`superseded`. Supersession is an attributable replan result, not a worker
transition.

Creation rejects duplicate IDs, unknown dependencies, cycles, out-of-scope
Agent identities, Computer Use workers, and bounds that exceed the manager,
worker, or server policy. A plan contains at most 64 steps, can have at most
16 ready/in-flight steps, and can be re-planned at most 16 times. Every
mutation is request-idempotent and may carry an expected plan revision. Plan
writes use a store-locked compare-and-swap, so two callers that read one
revision cannot both materialize different Work for one step.

`advance` materializes only steps whose declared dependencies have succeeded.
It does not resume an interrupted model invocation, grant approval, grant
Computer Use, or widen the captured Agent authority. If a child Work fails,
the plan becomes `needs_replan`; no replacement Work is invented implicitly.
The operator or manager must provide a reason and new step specifications via
`replan`, after which a later `advance` can continue the graph.

## Autonomous supervisor (Manager v2)

Plans remain manual by default for JSON and behavioral compatibility. Passing
`autonomous: true` at creation opts that plan into the process-owned manager
supervisor. The shared Rust service runs in desktop and hosted modes; no
focused window, UI timer, or second scheduler is involved.

Autonomous creation fails closed unless the named manager is the lane's
canonical active Agent and its managed-execution policy is enabled,
approval-free, and allows `manager-decision` Work. Manual plans keep the
legacy behavior and do not require native execution.

The supervisor wakes on a two-second bounded interval and relevant durable Run
events. A pass scans at most 16 plans and 64 observations, using a rotating
stable cursor so a hot plan cannot permanently starve older plans. Its health,
last pass, error, and counts appear under `health.managerSupervisor` in the
existing capacity/readiness projection.

For an active plan, the supervisor performs the same Work projection and
materialization as an explicit tick. Work is written before the CAS-fenced
plan revision; recovery adopts the tagged child if a crash occurs between
those writes. Notifications use a deterministic message identity plus the
per-step Work-revision fence, so interval and event wakeups converge without
duplicates.

## Durable manager decisions

When an autonomous plan reaches `needs_replan`, the supervisor creates one
deterministic `manager-decision` Work assigned to the plan's manager Agent.
The Work uses the existing managed executor and a new finite Run; no manager
execution queue exists. Its input is a bounded snapshot of the objective,
plan and Work revisions/outcomes, manager AgentSpec revision, and finite
bounds. It never includes the Agent's full transcript.

The linked decision record fences the plan revision, manager AgentSpec
revision, triggering Work/message IDs, input hash, decision Work and Run,
proposed directive, validation outcome, applied mutation IDs, and timestamps.
Occurrence and Work IDs are content-derived, so recovery after each durable
write converges on the same decision.

Decision Runs are proposal-only at the host permission gate. Every tool call,
including MCP, Computer Use, approval, promotion, resume, and terminal access,
is denied by an immutable host-owned capability downgrade installed before
the provider task is spawned. The durable decision/Work/intent/Run records
remain the audit trail, but permission enforcement never waits for a later
ledger link. This is a host boundary, not a prompt instruction.

Model output must be exactly one bounded JSON envelope; unknown fields fail.
The current directive allowlist is deliberately small:

- `append_replacement_steps`, naming every failed step and blocked descendant
  it supersedes
- `request_operator_intervention`
- `no_safe_action`

The envelope must match the occurrence, plan, expected plan revision, manager
Agent, exact AgentSpec revision, and input snapshot hash. Replacement steps
then flow through the existing replan validation and CAS operation. Malformed,
oversized, stale, cross-scope, inactive-identity, and duplicate proposals fail
closed. Explicitly superseded historical failures no longer prevent a valid
replacement graph from reaching `succeeded`.

## Explicit manager tick

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

## Authority and provider boundary

The manager remains a bounded coordinator. It observes and routes durable
state; it does not approve tools, grant Computer Use, resume interrupted Runs,
promote or merge code, widen Work/Agent/model/token/duration/workspace bounds,
or treat a named Agent ID as authentication. Approval- and permission-gated
Work remains visibly awaiting operator input.

Manager reasoning uses the manager Agent's captured provider/model selection
through the ordinary native execution path. The supervisor and directive
contract do not hard-code a provider. **Grok Build session/gateway routing is
already Supported** on that path (`~/.grok/auth.json` / OIDC, after
`XAI_API_KEY`, keychain, and `GROKPTAH_TOKEN_COMMAND`). Compatible gateway
requests consume provider quota. GrokPtah does not synchronize a Grok Build
account balance. Exact live certification remains a distinct, unproven
question. A local durable host quota ledger is **Pending — not shipped** on
[PR #352](https://github.com/chriscase/GrokPtah/pull/352) and cannot be treated
as shipped while that PR remains draft; merge requires independently certified
repair of the five confirmed P1s ([`ROADMAP_TO_100.md`](ROADMAP_TO_100.md)
stage 1). First live certification of this supervisor is roadmap stage 2;
hosted Grokbot soak is stage 6 and requires least-privilege tokens first
(stage 3). Draft manager-cert PRs
[#344](https://github.com/chriscase/GrokPtah/pull/344)–[#348](https://github.com/chriscase/GrokPtah/pull/348)
are **Pending — not shipped**.
