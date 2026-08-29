# Host authority spine

`crates/common/xai-host-authority` is one canonical authority root: a single
durable store and a single receipt family covering four gates. It exists so
there is exactly one answer to "who authorised this, against what, and did it
happen" — rather than several stores that can disagree.

## The four gates

**G1 — host-issued principal root.** The host mints principal identity,
credential incarnations, authentication generations, sessions, workspaces and
resource incarnations. A caller presents a bearer; it never mints identity.

**G2 — sealed capabilities and one-use leases.** A capability's scope is fixed
at issue time. An `EffectLease` authorises exactly one action, bound to that
action's digest together with the observation revision and digest it was
planned against.

**G3 — the physical-send attempt lattice.** A physical provider send requires a
`PhysicalSendPermit`. `HostAuthority::begin_send` is its only producer, and the
permit is consumed by value at settlement.

**G4 — typed audit.** An append-only, hash-chained log. Its *ordering* carries
the safety property, not its contents.

## The binding tuple

Every receipt carries the whole tuple, and equality is total — a mismatch in
any component is a refusal, not a near miss:

principal · credential incarnation · authentication generation · capability
generation · session · workspace · resource incarnation · control epoch ·
observation revision and digest · action digest · expiry · one-use consumption
· provider attempt identity.

A physical send binds more: the permit is bound to the **URL, method, wire
dialect, credential, model and body** together, not the body alone. Changing
any one of them after admission invalidates the permit, so a request admitted
for one endpoint or one credential cannot be replayed against another.

## Invariants, and how they are held

| Invariant | How |
| --- | --- |
| No caller-forgeable approvals | Every receipt has private fields and `pub(crate)` construction. Compile-fail doctests pin it. |
| No ordinary send bypass | A send needs a permit; `begin_send` is its only producer. There is no raw store accessor of any kind. |
| Administration is not ambient | Replacing credentials, rotating epochs or generations, and exporting the log need a `HostAdminAuthority`, returned only by `open` and not `Clone`. |
| Pre-effect persistence failure prevents dispatch | The attempt record and the intent audit record must both be durable *before* the permit is constructed. No permit, no dispatch. |
| A possible effect never reports ordinary failure | Any settlement whose persistence fails returns `Uncertain`, because the durable record still reads `sending` and recovery will conclude the same. |
| Ambiguity never auto-retries | There is no retry API. `reconcile_attempt` is the only exit, and it takes provider truth the host established. |
| Reads are scoped | `attempt_projection` reports another principal's attempt exactly as it reports a missing one, so it is not an existence oracle. |
| Projections are secret-, content- and path-free | Identifiers render as truncated domain-separated digests. Bodies, URLs, credentials and paths are digested on the way in and never stored. |
| A damaged root refuses service | Absent or unparsable state fails closed. A root that lost `authority.json` but kept audit evidence refuses rather than minting a fresh lineage. |
| Concurrency is real | Every mutation and every audit append takes an exclusive `flock`, which is held per open file description and so serialises processes, not just threads. |

## Two defects this design closes by construction

**Caller-first-claim.** A resource exists only if `issue_resource` created it.
Naming an unknown resource returns `UnknownResource` instead of inserting a
binding for the caller, so a caller cannot own a resource by being first to
name it.

**Stale-bearer resurrection.** A durable credential record always pins the
secret it authenticates — there is no "fingerprint absent, accept anything"
branch, and no caller-supplied credential list to match against. Any secret
change, removal and re-add, or owner change mints a fresh incarnation and
generation and invalidates every capability, lease and resource derived from
the old one.

## What is deliberately absent

There is **no delegation**: a sealed capability already binds exact resource,
effect and expiry to one principal, so there is no scope to widen. There is
**no retention, deletion or compaction** entry point, authenticated or
otherwise, so no operator act can drop evidence without breaking the chain.
There is **no environment-asserted operator authority**.

## Adjacent seam

The Computer Use `act` path applies the same rule at its own boundary: an
error the backend reports after it was handed an action settles as
`UncertainOutcome` unless its code cannot be raised after emission. The
classification is an exhaustive match, so a new error code forces a decision
rather than defaulting to "it definitely did not happen".
