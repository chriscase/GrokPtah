# Operator reconciliation

An always-on host eventually reaches a run it cannot prove. A worker crashed
between sending an attempt and recording its outcome; a lease expired while
nobody was watching; a provider answered with a state the adapter does not
recognize; the event journal advanced past an operator's cursor. Nothing in
the system can honestly close those runs out, because nothing in the system
knows what happened.

An operator does. This contract is how they say so.

It is defined in `crates/common/grokptah-agent-sdk/src/reconciliation.rs`,
mirrored for desktop, CLI, and API callers in
`desktop/src/lib/operatorReconciliation.ts`, and pinned by
[`docs/schemas/grokptah-operator-reconciliation.v1.schema.json`](./schemas/grokptah-operator-reconciliation.v1.schema.json).

Contract identifier: `grokptah.operator-reconciliation.v1`.

## What reconciliation can never do

Reconciliation records evidence and resolves operator-visible state. It never
resends, retries, resumes, or otherwise mutates a provider attempt. Three
independent things enforce that:

1. `ReconcileAction` is a closed enum with no resend/retry/resume variant, and
   `ReconcileAction::mutates_provider_attempt` is an exhaustive `match`
   returning `false`. A new variant is a compile error until someone states,
   in that function, what it does to a provider attempt.
2. Resolution writes a verdict onto the ledger entry. `AttemptObservation` is
   an input to the projection and is never written back.
3. `grokptah-agent-sdk` depends on `serde` and `serde_json` and nothing else.
   No code reachable from this contract can open a socket, read a file, spawn
   a process, or hold a credential — not by policy, but because the dependency
   graph offers no way to.

Making a provider do something again is a different capability, with a
different approval, on a different surface.

## Truthful state

A projection reports two things separately: the last durably written
lifecycle state, and how firmly the authority stands behind it.

| Confidence | Meaning |
| --- | --- |
| `confirmed` | Corroborated by evidence inside the policy's freshness bound. |
| `unconfirmed` | The last thing written, with nothing corroborating it now. |
| `uncertain` | The authority cannot prove the run's state at all. |

`uncertain` is not a lifecycle state and never replaces one. A run can be
`completed` *and* `uncertain` — that is exactly what a crash between "send"
and "record" leaves behind, and flattening it into `failed` would be a lie in
the direction that loses work.

## Why a run needs attention

Reasons are emitted most- to least-severe. The order is load-bearing: two
surfaces rendering the same run agree on which line goes first.

| Reason | Severity | Domain |
| --- | --- | --- |
| `uncertain_outcome` | blocking | model or provider |
| `crash_recovered` | degraded | worker or lease |
| `lease_expired` | degraded | worker or lease |
| `provider_ambiguity` | blocking | model or provider |
| `cancel_unconfirmed` | degraded | operator decision |
| `deadline_exceeded` | advisory | operator decision |
| `stream_gap` | advisory | worker or lease |
| `stale_observation` | advisory | operator decision |

`uncertain_outcome`, `provider_ambiguity`, and `crash_recovered` force
`uncertain` confidence. The rest degrade it to `unconfirmed`.

### The domain split is the point

Three failures that look identical on a status page need three different
responses:

- **model or provider** — the provider's own answer is ambiguous or
  unreported. Go read the provider's console.
- **worker or lease** — our worker, lease, or host failed. The provider may
  be perfectly fine. Go read our journal.
- **operator decision** — nothing is broken. The run is waiting on a human.

`AttentionReason::domain` is total and stable, so a queue can be grouped by
domain without the grouping logic living in a UI.

### Provider ambiguity

`ProviderState::Unknown` always contradicts local state. An adapter that
cannot classify what the provider said has given us no basis to agree with
ourselves, and treating "I could not parse this" as "everything is fine" is
how a stuck run gets reported green.

### Stream gaps

Sequences are assigned contiguously, so the entry an operator wants next is
always `cursor + 1`. A retained window start is *not* enough to detect a gap:
retention pins the resolving entry, so the retained set can have a hole above
its own first element. `cursor_expired` compares against the next actually
retained sequence, which is exact in either shape.

A gap is never presented as complete history.

## The reconciliation action

```text
record_evidence     attach evidence, assert nothing
acknowledge         acknowledge the attention, state stays as unprovable as it was
resolve_completed   declare, on the evidence, that the attempt completed
resolve_failed      declare, on the evidence, that the attempt failed
resolve_cancelled   declare, on the evidence, that the cancel took effect
```

Every `resolve_*` action requires at least one evidence record. Asserting an
outcome without evidence is how a ledger becomes fiction.

Evidence is digest-addressed: a `kind`, a content `digest`, and a bounded
redacted `summary`. It never carries a payload. Raw provider output routinely
contains credentials and customer data, and an operator ledger is one of the
longest-lived records in the system.

### Fences

| Fence | Behaviour |
| --- | --- |
| `requestId` | Idempotency key. An exact replay returns the original entry. |
| `expectedRevision` | Must equal the observed revision, or `stale_revision`. |
| First verdict wins | A second resolving action gets `already_resolved`. |

Replay is checked **before** the revision fence, deliberately. A client whose
first call succeeded and whose response was lost must not be punished for its
own success: its retry replays rather than failing on a revision its own
earlier call moved.

A second operator who loses the race is told, not silently merged. They can
still attach what they saw with `record_evidence` — only the verdict is
closed, not the record.

## Durability and restart

The ledger is pure state over an append-only entry list. The host owns
persistence; after a restart it replays stored entries through
`ReconciliationLedger::recover`, which rebuilds the cursor, the verdict, and
the idempotency index from the entries alone.

Recovery fails closed on a torn journal. Sequences must strictly increase, and
a duplicated `requestId` is rejected rather than half-trusted. A crash between
"append" and "acknowledge" therefore recovers cleanly, while corruption is an
error rather than a quietly wrong ledger.

Retention slides the window at `MAX_LEDGER_ENTRIES`, but never evicts the
resolving entry: losing it would let an already-resolved run be re-resolved.

## Reading history

`history` and `inspect` are read-only and fenced by an `AuthorityBinding`.

Unknown records and cross-authority records return the *identical*
`not_available` error, so the surface cannot be used to probe whether a run
exists. `list_attention` silently drops unbound ledgers and returns no count,
marker, or ordering artifact that would reveal how many were filtered out.

Every operator-facing reference is an `OpaqueRef`: bounded, free of control
characters, free of path and URL syntax, and checked against the scope it
stands for so it cannot embed a workspace path or session identity. The
contract does not *derive* opaque values — deriving one here would be a
non-cryptographic fingerprint dressed up as a privacy boundary. The authority
mints them; this type enforces that they are actually opaque.

## Determinism

Every clock value is a parameter. `project_attention` reads no ambient clock,
which is why a cockpit, a CLI, and a test provably produce byte-identical
output for one record — and why the test suite has no timing flake surface.

## Operator handoff

`desktop/src/lib/operatorReconciliation.ts` is transport-neutral and
side-effect free. It parses an authority's projection and *builds* a request;
the caller sends it with whichever client it already holds.

```ts
const attention = parseRunAttention(await readProjection());
console.log(summarizeForOperator(attention));

const payload = buildReconcileRequest({
  requestId: crypto.randomUUID(),
  scope: { sessionId, workspace, runId },
  expectedRevision: attention.revision,
  action: "resolve_failed",
  evidence: [
    {
      kind: "provider_projection",
      digest: "sha256:5f0c9a",
      summary: "provider console shows the attempt never started",
    },
  ],
  note: "closing out after the worker crash",
  operator: { operatorRef, authorityRef },
});
```

`parseRunAttention` refuses an unknown contract version rather than
best-effort rendering it, so a newer authority cannot show an operator a
partial truth. `sortByUrgency` gives a deterministic queue order — severity,
then oldest observation, then `runRef` — so two surfaces agree on row order.

Suggested tool names for a host exposing this over MCP are
`ptah_reconcile_run` and `ptah_get_reconciliation_history`; the TypeScript
module exports both as constants alongside the payload builders.

## Tests

- `crates/common/grokptah-agent-sdk/src/reconciliation.rs` — unit tests for the
  projection, the closed action set, redaction, and opaque references.
- `crates/common/grokptah-agent-sdk/tests/operator_reconciliation.rs` — 20
  deterministic scenarios: synthetic crash-cut, replay, dropped-response
  replay, concurrent operators, stale revision, torn-journal recovery, pruned
  history, pinned-verdict gaps, redaction, and bounded resources.
- `desktop/src/lib/operatorReconciliation.test.ts` — 20 cases covering
  parsing, queue ordering, the operator summary, and payload construction.

The Rust and TypeScript suites assert the same golden crash-cut document, so
the two implementations of one contract cannot drift apart silently.
