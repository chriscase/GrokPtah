# Swarm control plane

The swarm control plane is a provider-neutral, durable abstraction for running
a graph of related child agents. It is the structural answer to something the
`grok-build-orchestrator` profile currently asks for in prose: delegate widely,
review the results, synthesize. Today that delegation is real but ephemeral —
the `task` tool spawns background children with `tokio::spawn`, and the fan-out
lives in one process's memory.

`grokptah-swarm-control-plane` is a self-contained crate with no runtime of its
own. It owns no threads, opens no sockets, reads no clock, and generates no
randomness. Callers pass `now` in and persist the state out. That is what makes
the whole state machine replayable, deterministic, and testable without a
provider.

## Not a second manager, and not a second queue

Durable manager plans coordinate *ordinary Work items* through the existing
ledger, with its claim leases, assignments, reviews, approvals, and native
executor. That coupling is deliberate and this crate does not duplicate it.

The swarm control plane sits one level lower and one level wider. It has no
opinion about Work, no dependency on `grokptah-agent-bridge`, and no provider
baked in. It describes a task graph, decides what is dispatchable, and records
what was dispatched. A manager, an MCP surface, or the desktop can each adopt it
without adopting the others' storage. Where manager plans answer "what should
this Agent do next in the ledger", a swarm answers "which children may run right
now, and what exactly did we already send".

## What the slice proves

**Dependency ordering and parallel readiness.** Tasks become ready only when
every declared dependency has succeeded. Siblings on independent branches become
ready together, and dispatch order is deterministic: priority descending, then
task ID. Nothing depends on hash or iteration order.

**Deterministic validation.** A graph is checked in a fixed order, so failures
are reproducible. Unique task and worker IDs, resolvable dependencies, bounded
fan-out, bounded dependencies, and acyclicity are all enforced before a child is
spawned. Cycle detection is iterative rather than recursive, so a deep or
adversarial graph cannot exhaust the stack.

**Fail-closed provider, model, and capability admission.** A `ProviderCatalog`
records exact provider/model pairs and the roles, capabilities, and capability
modes each pair has been *measured* to hold. A worker naming anything absent
from the catalog is refused. An empty catalog admits nothing. This matches the
existing rule that a model name is never proof of capability.

**Isolation is part of the contract.** A worker that can write the workspace or
run commands must require a dedicated worktree; so must any worker that is not
read-only. This mirrors `SUBAGENT_ISOLATION.md` rather than restating it, and
`IsolationRequirement::as_subagent_isolation` projects straight onto the
existing `SubagentIsolationMode` wire enum. Each task also declares its exact
capability set and mode; the mode must be no broader than its worker's mode,
and the effective task authority determines the worktree requirement. Read-write
tasks must explicitly declare `WriteWorkspace`; execute tasks must explicitly
declare `ExecuteInWorktree`. As in that document, worktree separation prevents
routine edit collisions; it is not an operating-system sandbox, and nothing
here claims otherwise.

**No browser or raw-host authority is expressible.** `WorkerCapability` is a
closed set with no browser variant and no raw-host variant, so no specification
— however authored or deserialized — can request either. Specification types use
`deny_unknown_fields`, so an unrecognized field fails closed instead of being
silently dropped.

**Computer Use is leased, never minted.** A task may declare that it requires
Computer Use, and such a task is undispatchable unless the caller attaches an
externally issued grant reference that is structurally valid, live at the
dispatch instant, and bound to the exact swarm, task, dispatch, run, target,
owner, and required action class. The durable store consumes the grant identity
in the same compare-and-swap that records the dispatch, so a grant cannot be
copied into another dispatch or swarm. The control plane never issues, extends,
or revalidates the external grant. A lease attached to a task that does not
require Computer Use is also refused.

**Restart safety without guessing.** Dispatch is two-phase. Planning is a pure
projection that writes nothing; `record_dispatch_requested` writes the durable
record, and the caller must win the durable spawn claim before spawning.
Dispatch identity is derived from
`(swarm, task, attempt)`, so replaying a planning pass proposes the identifier
already on disk instead of minting a second one, and replaying the write returns
the stored record without moving a counter.

A crash between the write and the spawn leaves a `Requested` or
`SpawnClaimed` record with no acknowledgement. `recover` marks exactly those
`Uncertain`; an `Acknowledged` dispatch carries a provider handle, can be
probed, and is left running. A durable spawn claim has one winner, so replaying
the request cannot authorize a second spawn. An uncertain dispatch is never
resent, never settled by assumption, and never cancelled blind. Only
`reconcile_uncertain` carrying positive evidence resolves it, and
`DispatchProbe::Unknown` deliberately resolves nothing. Because a possibly
running child may still hold real capacity, an uncertain task continues to
occupy its admission slot, and the swarm will not declare a terminal outcome —
including a completed cancellation — while one is outstanding.

**Cancellation at both scopes.** A task that never started is cancelled
outright; a live task moves to `Cancelling` and settles when the caller confirms
its terminal outcome. A whole-swarm cancel stops admission immediately and
reaches `Cancelled` only once nothing may still be running.

**Failure propagation without invention.** A failure blocks its transitive
dependents and spares independent branches; the `CancelSwarm` policy instead
fails fast. No replacement work is ever invented implicitly, matching the
manager's replan rule.

**Quorum and synthesis gates.** A synthesis task names reviewer tasks it also
depends on, plus a quorum, and omitting that gate is invalid. The gate is
evaluated on reviewer *verdicts*, not on reviewer success: a reviewer that runs
to completion and rejects has done its job and still withholds its approval. A
completed review must report a verdict, and only a review task may report one.

**Redacted, credential-free projections.** `TaskProgressRow` and `EvidenceRow`
have no field that could carry a credential — `WorkerSpec::credential_ref` is a
keychain reference and is simply not projected. All free-form text passes
through the repository's shared secret sanitizer and a byte bound that cuts on a
character boundary.

## Bounds

A swarm holds at most 64 tasks and 32 workers, at most 16 simultaneous
dispatches, at most 16 direct dependents or declared dependencies per task, at
most 16 reviewers per gate, and at most 256 dispatch attempts inside a
12-hour ceiling. The task ceiling matches the durable manager's per-plan step
ceiling so a swarm cannot outgrow the coordinator that will eventually project
it.

## What remains

This slice is a control plane and a proof, not a shipped feature. Every item
below is deliberately out of scope here.

**Real mixed Grok/Claude/Cursor dispatch.** No child has been spawned through
these types. The seam is `DispatchIntent` → spawn → `record_dispatch_acknowledged`,
and the first adapter should be a single read-only child on one provider, with
the catalog populated from measured qualification results rather than by hand.

**Manager, MCP, and desktop wiring.** Nothing imports the crate yet; the only
product integration is its workspace registration. `DurableSwarmStore` defines
the persistence/CAS and global lease-consumption seam, but no production store
or adapter is included. Adoption still means choosing the existing Work ledger
as the authority boundary and qualifying the narrow task-tool adapter.

**A whole-swarm live restart campaign.** Restart safety is proven by
construction and by tests that serialize, reload, and recover. It has not been
proven against a real process that dies mid-spawn with real children running.
That campaign is what would justify trusting the uncertainty rules in
production.

**Computer Use lease integration.** The lease reference deliberately mirrors the
shape of the existing `ActionGrant` but is not connected to it. Wiring means
resolving a real grant, honoring revocation and remaining uses at the moment of
action rather than only at dispatch, and deciding how a lease that expires
mid-run is surfaced.

**An operator dashboard.** `project_progress` and `project_evidence` are built
for one, including the `needsOperatorAttention` flag that an uncertain dispatch
raises. No surface renders them.

**Continuous integration coverage.** A dedicated root-workspace lane now runs
formatting, locked metadata, focused tests, full crate tests, and Clippy for
this crate. The desktop workflow still builds the nested
`desktop/src-tauri`, `grokptah-agent-bridge`, and certification-lab workspaces;
the crate is not part of the shipped desktop binary.

## Test evidence

Before the first repair the source contained 84 explicit `#[test]` functions
and one `no_run` doctest, not the 83 claimed by the candidate commit. The
current repaired crate contains 105 explicit tests plus that doctest. The tests
are deterministic and provider-free; they do not qualify a production
persistence store, task-tool adapter, or live Computer Use runtime.
