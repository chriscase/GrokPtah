# Durable work graph

A work graph is a host-supervised set of related child agents with declared
dependencies, durable leases, and per-attempt authority. It is the structural
answer to a fan-out that today lives in one process's memory.

The graph is **not a second control plane**. It is a submodule of the existing
`orchestration` module:

| Concern | Owner |
|---|---|
| Persistence | the existing `OrchStore` root, behind its exclusive store lock |
| Admission capacity | the host's existing orchestration capacity |
| Authorization | the same bearer + session + workspace checks as a run request |
| Computer Use grants | the existing Computer Use ledger and `ActionGrant` |
| Audit | the existing orchestration audit log |

There is no parallel scheduler, no second lease universe, no second credential
universe, and no authority that exists only in memory.

## What this slice establishes

**One canonical Work/Lease/Run graph.** `WorkGraphSpec` declares work items,
their dependencies, and the workers that may run them. `WorkGraphRecord` is the
durable state: work records, leases, provider attempts, minted authorities, and
a bounded budget ledger, all under one compare-and-swap revision.

**Deterministic validation and assignment.** A graph is checked in a fixed
order, so a rejection is reproducible. Cycle detection is an iterative Kahn peel,
so a deep or adversarial graph cannot exhaust the stack. Dispatch order is
priority descending, then work id ascending — never hash or iteration order.

**Bounded admission.** `plan_admissions` is a pure projection that writes
nothing. It takes the slot count the host's existing capacity already granted and
narrows it by the graph's in-flight cap, attempt budget, token budget, and
deadline. It can only ever admit less than the host permits, never more. When it
admits nothing it says which bound stopped it, so "nothing to do" is
distinguishable from "not allowed to do it".

**Host-minted, action-time authority.** An `ActionAuthority` is a durable
capability statement bound to an exact workspace, session, agent, provider route
(provider, profile, endpoint, model, effort), capability and policy revision
pair, execution bounds, and one exact attempt identity. It is sealed with a
binding digest, written before it is returned, and consumed through a durable
one-winner claim. Every mismatch at use is a refusal; there is no partial credit.

**Frozen provider routes.** `ProviderRouteSnapshot` is captured before an attempt
exists, so a profile edited mid-flight cannot retroactively change what an
in-flight attempt was authorized to reach. `credential_ref` is an opaque keychain
reference and `credential_fingerprint` binds credential identity without
persisting bearer material.

**One Computer Use consumption path.** `consume_grant_for_action` is the only
function that turns a grant into permission to act, and it operates on the
`ActionGrant` the Computer Use ledger issued. The durable claim key includes the
Computer Use run's control epoch, so a pause, takeover, stop, or recovery makes
every earlier binding unusable — revocation has no window in which a revoked
grant still consumes.

**Restart safety without guessing.** A lease is written before a child exists.
A crash between the write and the spawn leaves a lease that is neither
acknowledged nor settled; `recover` marks exactly those uncertain. An
acknowledged lease carries a handle, can be probed, and is left running. An
uncertain dispatch is never resent, never settled by assumption, and never
cancelled blind — only a probe carrying positive evidence resolves it, and
`DispatchProbe::Unknown` deliberately resolves nothing.

**No auto-retry after an uncertain send.** A provider attempt row is installed
before the host enters the transport. An outcome of `UncertainAccept`, or an
`Admitted` row found after a restart, sets `ExplicitNewAttemptOnly`, and
admission skips that work item until an operator acts.

**Truthful terminal state.** The graph refuses to declare any terminal
lifecycle — including a completed cancellation — while a child's fate is unknown
or capacity is still held. A discarded item is terminal and is never counted as a
success.

**Quorum on verdicts, not on success.** A synthesis item names reviewers it also
depends on plus a quorum. The gate counts reviewer *verdicts*: a reviewer that
ran to completion and rejected has done its job and still withholds its approval.
When the remaining undecided reviewers can no longer meet the gate, the synthesis
item is blocked rather than left pending forever.

**Secret-free projections.** Every DTO is built by naming the fields it carries,
never by serializing a durable record wholesale. `WorkerSpec::credential_ref` has
no field to land in. Free-form text passes through the caller's redactor — the
same one that covers the durable journal — and a byte bound that cuts on a
character boundary.

**No browser or raw-host authority is expressible.** `WorkCapability` is a closed
set of four with no browser and no raw-host variant, and specification types use
`deny_unknown_fields`, so an unrecognized field fails closed rather than being
silently dropped.

## Bounds

At most 64 work items and 32 workers per graph, 16 simultaneous dispatches, 16
dependencies or reviewers per gate, 256 attempts inside a 12-hour ceiling, 16
evidence entries per item, and 500 bytes of free-form text per projected field.

## What remains

Every item below is deliberately out of scope here and is **not** claimed.

- **No child has been spawned through these types.** The seam is
  `DispatchIntent` → spawn → `acknowledge`, and the first adapter should be a
  single read-only child on one provider.
- **No live provider dispatch.** Route snapshots are exercised with loopback
  fixtures and synthetic credential references only.
- **Live Computer Use runtime integration.** The consumption path is bound to
  `ActionGrant` and fenced on the control epoch, but no live Computer Use backend
  has driven it.
- **Desktop command wiring.** `DesktopGraphDto` and
  `desktop_work_graph_dto_scoped` exist and are covered, but no Tauri command
  surfaces them and no UI renders them.
- **Graph creation over MCP.** Creation mints authority and freezes a provider
  route, so it stays a host-side typed call; MCP exposes status and control only.
- **A whole-graph live restart campaign.** Restart safety is proven by
  construction and by tests that persist, reload, and recover — not against a
  real process that dies mid-spawn with real children running.
