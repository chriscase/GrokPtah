# Adaptive Computer Use controller

A Computer Run does not need a large vision model to decide most of its steps.
Clicking the only enabled Save button, filling the one focused text field, or
dismissing a modal with a single dismiss control are decisions a small,
inexpensive, locally hosted gateway model can make — or that need no model at
all. The expensive model earns its cost when the cheap path is *provably*
untrustworthy: the surface has no semantics, the semantics disagree with each
other, the same frame keeps coming back, or the last action failed
verification.

`desktop/src/lib/adaptiveComputerUse.ts` is that policy, as a reusable,
provider-neutral controller. It performs capability negotiation and
decision-making only. It opens no socket, holds no model client, and never
touches a screenshot; the caller owns transport and the host owns action
enumeration and authority.

This document is the handoff for the slice that landed. It describes what is
implemented and enforced, not a roadmap claim.

## What the controller does not own

Lifecycle truth is borrowed, never re-derived:

- **Authority, control disposition, grant validity, terminality** come from the
  authoritative `ComputerRunProjection` (`desktop/src/lib/protocol.ts`), the
  same serialized payload an external coordinator receives. Anything other
  than a live `agent_owned` run holding an unrevoked, unexpired, unexhausted
  grant is authority loss, and authority loss halts the controller.
- **Capabilities** come from the negotiated `CapabilitySet`
  (`desktop/src/lib/capabilities.ts`). `negotiateAdaptiveCapabilities` reports
  what the host already granted and fails closed; it never asks for more.
- **Which actions are legal** comes from the host, as an enumerated candidate
  list. The controller chooses among candidates; it cannot invent one.
- **What counts as success** comes from the host too. Every candidate carries
  its own `AdaptiveExpectation`, so a model chooses *which* authorized move to
  make but can never define the postcondition it will be judged against.

The controller imports `ComputerRunProjection` as a type only, so nothing from
`protocol.ts` reaches the runtime bundle.

## Execution profiles

| Profile | Confidence floor | Large-model calls | Verification rule |
| --- | --- | --- | --- |
| `economy` | 0.55 | 0 (never escalates) | Self-verification accepted |
| `balanced` | 0.70 | up to 4 | Self-verification accepted |
| `high_assurance` | 0.85 | up to 12 | Independent verifier required |

`economy` does not escalate at all: when the cheap path fails it abstains with
`escalation_not_permitted` and hands the step back rather than quietly buying
an expensive call. `high_assurance` treats a verified-but-self-checked step as
*not* verified — it holds with `independent_verification_required` until a
second verifier reports, and that hold is deliberately an abstention rather
than a large-model escalation, because the hold wants a second opinion on the
result, not a new plan.

Callers may tighten any budget ceiling. Widening is clamped to the profile
default, so a misconfigured consumer cannot buy itself more authority.

## Step lifecycle

```
createAdaptiveController(config)
  -> adaptiveIngestObservation(state, observation, projection)   // authority + monotonic revision
  -> adaptiveDecideStep(state, candidates)                       // pure read; spends nothing
       act      -> adaptiveCommitPlan(state, plan, cost)         // mutation gate; charges one step
       consult  -> small-model request  \
       escalate -> large-model request   > adaptiveAdoptModelDecision(state, request, reply, ...)
       abstain / halt                   /
  -> adaptiveVerifyPlan(plan, before, after, { independent })    // semantic before/after
  -> adaptiveRecordVerification(state, result, cost)
  -> adaptiveStepProjection(state)                               // the only public shape
```

`adaptiveDecideStep` is a pure read: it moves no counter. Only
`adaptiveAdoptModelDecision` and `adaptiveCommitPlan` (and the cost argument to
`adaptiveRecordVerification`) can move the budget, and a model call is charged
whether or not its reply turned out usable.

Decision order is deliberate: budgets bound everything, then observation
validity, then a pending escalation, then trust in the current semantics, then
the deterministic path, and only then a model.

**No model is consulted when exactly one authorized action exists**, at any
profile including `high_assurance`. There is nothing to choose; assurance comes
from verifying the result, not from paying a model to restate the only legal
move.

## Grammar-constrained output

A decision request carries both a llama.cpp-compatible GBNF grammar and the
equivalent JSON-schema constraint. Both enumerate exactly the candidate ids
actually offered:

```
root ::= "{" ws "\"candidateId\"" ws ":" ws candidate ws "," ws ... "}"
candidate ::= "\"cand-save\"" | "\"cand-title\""
rationale ::= "\"only_authorized_action\"" | ... | "\"uncertain\""
confidence ::= "0" | "1" | "0." [0-9] [0-9]?
boolean ::= "true" | "false"
```

There is no free-text production in either form. The model answers with an
enumerated candidate id, a numeric confidence, a rationale **code**, and an
abstain flag — four keys, nothing else. `parseAdaptiveDecisionAnswer` rejects
rather than repairs: prose, code fences, prose wrapped around JSON, an extra
`reasoning` field, an invented or unauthorized candidate id, a confidence
outside the unit interval, an unknown rationale code, or a reply over the 4 KiB
output ceiling. Candidate ids are restricted to a grammar-safe alphabet, so
they embed as GBNF literals without escaping.

Identifiers are enumerated in the grammar, so a model cannot name an element or
action the host did not offer.

## Escalation reasons

Escalation is always an explicit code, never a judgement call:

| Reason | Trigger |
| --- | --- |
| `screenshot_only_surface` | The surface has no semantic backing at all |
| `missing_semantics` | Neither AX nor DOM data is available |
| `contradictory_semantics` | AX and DOM disagree (bounded reason codes) |
| `repeated_uncertainty` | Two consecutive unusable or low-confidence answers |
| `verification_failed` | The postcondition was contradicted |
| `no_op_detected` | The frame did not move when it should have |
| `independent_verification_required` | `high_assurance` with no second verifier |

A verification failure escalates **once**. A second failure of the same plan
halts with `verification_exhausted` rather than escalating again — re-running a
move that already failed twice is the blind-retry loop this policy exists to
prevent.

## Stationarity and staleness

Frames are represented as an opaque lowercase-hex digest. No pixel enters the
module, and the digest itself is excluded from the public projection —
`frameChanged` is the entire visible trace of what the screen did.

- Two consecutive identical frame digests (`ADAPTIVE_STATIONARY_LIMIT`) mean
  the last action did nothing, so the next decision escalates instead of
  replaying the same move.
- A mutating action that leaves the frame identical verifies as `stationary`,
  distinct from `contradicted`, so a caller can tell "the app refused" from
  "the app changed in the wrong way".
- Observation revisions must strictly increase. A replayed revision is
  rejected, which makes stale observations refusable rather than merely
  unlikely.
- `adaptiveAuthorizePlan` is the mutation gate: a plan may act only against the
  exact observation id, revision, and control epoch it was decided from.
  `adaptiveCommitPlan` returns `null` for an unauthorized plan, so a stale plan
  cannot advance the controller even if a caller skips the explicit check.

## Boundary rules

Nothing below may cross the public boundary, and each is covered by a test:

- **No frame bytes.** Screenshots never enter; frame digests never leave.
- **No secrets, host paths, clipboard text, or absolute URLs.** Every free-text
  field (element roles and labels) is bounded and screened against the same
  privileged-marker set the external-worker boundary uses. The screen is
  duplicated rather than imported, so tightening one boundary cannot silently
  loosen the other.
- **No raw values.** Element values cross only as opaque hex digests, and text
  entry crosses only as an opaque host-side `valueRef`. A password or clipboard
  payload has no field to travel in.
- **Restricted labels are withheld even from a locally hosted gateway.** The
  model does not need the text to pick among enumerated candidates, so a
  `restricted` element's label is dropped and flagged `labelRedacted`.
- **No raw model prose.** Every field on a plan is an id, a number, an enum, or
  a boolean.
- **No generic execute escape.** Action kinds are the closed set
  `activate_target | invoke | set_value | select | scroll`, and each kind's
  structure is checked (only `set_value` carries a value reference, only
  `scroll` carries a delta, only `activate_target` omits an element).
- **Unauthorized candidates never reach a model** and can never become a plan.

## Consuming it

The controller ships in both published entry points — `@grokptah/client` and
`@grokptah/client/ui-core` — with no UI internals, no React, and no Tauri
dependency. `desktop/scripts/verify-public.mjs` exercises it inside the built
bundle (deterministic no-model path, grammar-constrained request, prose
rejection, projection hygiene), and
`desktop/scripts/run-public-consumer-smoke.mjs` drives a full step from a
packed tarball as an external consumer would, including the stale-plan refusal.

## Measured limits

| Limit | Value |
| --- | --- |
| Elements per observation | 48 |
| Elements per model request | 32 |
| Candidates per request | 16 |
| Model answer ceiling | 4096 bytes |
| Element label / role | 256 / 64 bytes |
| Identifier | 128 bytes |
| Contradiction codes | 8 |
| Stationary limit | 2 identical frames |
| Uncertainty limit | 2 consecutive answers |

## What this slice does not do

- No HTTP client, model client, or provider adapter. Wiring a real locally
  hosted gateway is a separate lane.
- No Rust implementation. This is the TypeScript policy surface only; the Rust
  core still owns observation capture, action execution, and the durable run.
- No grounding for `screenshot_only` surfaces. The controller can *detect* one
  and escalate, but coordinate grounding is not implemented.
- No benchmark. The profiles' confidence floors and budget ceilings are
  reviewable defaults, not measured operating points.
