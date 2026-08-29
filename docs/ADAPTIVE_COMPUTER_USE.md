# Adaptive Computer Use profiles

Issue [#435](https://github.com/chriscase/GrokPtah/issues/435) asks for one
Computer Use capability that works with both an inexpensive company-hosted
gateway model and a frontier multimodal model, without changing its safety
guarantees. This document describes the production seam that implements the
policy half of that: which profile a run executes in, why, when it escalates,
when it stops, and what an operator can read about all of it.

It does **not** describe live model qualification. No live company-gateway,
small multimodal, or frontier campaign has run. See
[Residuals](#residuals-not-closed-by-this-seam).

## The three profiles

Canonical identifiers are `economy`, `balanced`, and `high_assurance`. There are
three, and there will be three.

| Profile | Observation the model sees | Model calls | Repairs | Screenshot capture | Pointer / key chord |
| --- | --- | --- | --- | --- | --- |
| Economy | 48 elements, 24 KiB, semantic only | 16 | 1 | no | no / no |
| Balanced | 256 elements, 128 KiB, plus geometry | 48 | 2 | yes | yes / no |
| High Assurance | 1024 elements, 512 KiB, plus a redacted capture reference | 96 | 3 | yes | yes / yes |

Every number above is an **efficiency budget**. None of them is a safety
control. `ProfileBudget` and `SafetyFloor` are separate types precisely so that
distinction is structural rather than a review convention.

### Compatibility aliases

Historical developer checkouts and unmerged donor branches used `efficient` and
`frontier`. Those two tokens are accepted **on ingest only** — persisted session
metadata and deployment overrides keep deserializing — and are canonicalized
immediately:

```
efficient -> economy
frontier  -> high_assurance
```

They are never emitted, never enumerated, and never produce a fourth mode.
`AdaptiveProfile::ALL` has three entries, `Serialize` writes only canonical
names, and `aliases_do_not_invent_extra_modes` proves both.

Aliasing carries the donor's *identity*, not its *semantics*. The #453 candidate
had `Frontier` disable the host verification that `Efficient` required, so a
lexical rename would have made "High Assurance" mean **less** assurance than
"Economy". That inversion is dropped on the way in: verification is not a budget
knob, so it does not live in the budget table at all.

## It sits on top of the sealed boundary, never beside it

This layer is built **above** the host-owned proposal seal
([#473](https://github.com/chriscase/GrokPtah/pull/473)), not alongside it.
`propose_computer_action` returns raw provider bytes with no authority;
`seal::accept_model_proposal` is the single universal validator, run against the
live record at application time; and profile narrowing
(`enforce_profile_budget`) runs **strictly after** the seal and can only reject
more. There is no path from raw bytes or from a deserialized value to a staged
action or a completion, and the adaptive layer does not add one.

Completion is the same story: a model saying "done" buys nothing. Only a
host-issued `ActionReceipt`, verified against the single postcondition frame in
the same authority epoch, admits `complete_verified`.

## Authority is re-derived every turn, never cached

The first cut of this layer kept adaptive state in a process-local map keyed by
**session**, and re-used the capability evidence it was opened with. Both were
wrong. `AdaptiveController::begin_turn` now re-derives three things on every
single turn and refuses when any has moved:

1. the compare-and-swap **revision**, so a stale or duplicate caller cannot act;
2. the **capability generation** (#458) — a secret-free digest over route
   identity, effective tier, provenance, capability schema version, a one-way
   credential generation, and the operator policy. A same-route tier downgrade,
   a credential rotation, a schema drift, or an operator policy edit all move
   the digest, and a moved digest is a stop, never a reuse;
3. the **task risk**, against the run's risk high-water mark, so a later
   destructive objective cannot ride the authorization a routine one obtained.

### Declared capability is observation-only

`OperatorCapabilityPolicy::trust_declared_capability` defaults to `false`. A
provider asserting its own competence in a config file is not a measurement, so
a declared-only route may observe but never act until a local operator
explicitly opts in. The policy participates in the generation digest, so
changing it invalidates every authority decided under the old policy.

## The admitted turn is bound into the seal

Re-deriving authority at admission is not enough on its own. A turn is admitted,
the provider is asked, and the answer arrives some seconds later — and in that
window the facts can move. Before the binding existed, the kernel seal proved
only that the *run* had not moved (version, control epoch, observation), and the
profile was re-read at application time rather than carried from admission. So a
tier downgrade, a credential rotation, a policy edit, a schema drift, or an
escalation to a different profile could all happen mid-inference and the answer
would still apply.

`begin_turn` now mints an opaque `permit_id` and writes it to the record as
`active_permit`. `ModelProposalContext::from_run_with_permit` binds that permit,
the canonical profile, and the full capability generation into the sealed
proposal, and the binding is re-checked **twice**: once when the provider's
answer is sealed, and again inside `authorize_against` under the lock that
guards the mutation.

- `begin_turn` is the only thing that writes `active_permit`. Every stop,
  escalation, completion, restart recovery, and subsequent admission clears or
  replaces it, so a seal from an invalidated turn has nothing to match.
- The record's `revision` is deliberately **not** bound: revision is the
  compare-and-swap witness for admission, and ordinary accounting advances it.
  Binding it would conflate "this run spent money" with "this run's authority
  changed".
- `ModelProposalContext::from_run` — the permit-free path — **refuses** a run
  that has an adaptive record. A run under adaptive authority has no way to
  obtain an unbound seal.

## One spend path, so the SDK cannot skip the budget

`enforce_profile_budget` lives in `apply_accepted_locked`, the one function every
spend goes through, and takes its profile from the seal rather than from a fresh
read. It used to sit in the model-bytes entry point instead — which left the
SDK seam, `apply_accepted_proposal`, taking an already-sealed capability and
applying it with no profile ceiling at all.

Taking the profile from the seal also closes the cheaper-ceiling read: a run that
has escalated since the turn was admitted cannot be spent at the profile it
started under, because `authorize_against` has already proven the live record
still agrees with the bound one.

## The host mints the evidence and the risk

`begin_adaptive_turn` takes an `AdaptiveTurnRequest`: *inputs*, not conclusions.

- **Host capability evidence** is derived inside the service by
  `HostCapabilityEvidence::observe` on the run's own current observation, plus
  the build constant `HOST_INDEPENDENT_VERIFIER_AVAILABLE`. A caller cannot
  unlock High Assurance by asserting a verifier this build does not have.
- **Task risk** is classified inside the service from the operator's objective
  and that same frame. A caller cannot label a destructive objective `Routine`
  to slip it past the run's risk high-water mark.

What remains an input is the *model* half of the evidence, which is a fact about
the configured route that only the host can resolve — credentials, gateway
config, measured qualifications. Its declared claims are observation-only unless
local operator policy says otherwise, and that policy is part of the generation.

## A record that fails its own invariants is not authority

The durable record is a file. `AdaptiveRecord::check_invariants` verifies that
attempts reconcile, that no screenshot bytes were ever accounted to a model, that
the profile is within both the evidence ceiling and the ceiling it was decided
under, that the escalation history is a contiguous climb ending where the record
says it is, and that the terminal outcome and lifecycle agree.

`enforce_invariants` runs on **every load**, not only at restart, and converts a
violating record into a terminal `record_invalid` stop. The record is kept, so
the operator can see that this happened and why, but every admission path already
refuses a terminal record.

## The record is durable, per-run, and recovered

`AdaptiveRecord` is a field on `ComputerRun`, written through the same
crash-atomic store as the rest of the run: profile, capability generation, risk
high-water, revision, lifecycle, spend, escalation history, stationarity state.

- Keyed by **run**, so a second run in the same session starts from a fresh
  selection rather than inheriting spend and history.
- `#[serde(default)]`, so a record written before the field existed
  deserializes to `None` — and `None` is "no adaptive authority", not "no
  constraints". The host refuses to spend a model call against it.
- Startup recovery marks an in-flight turn `interrupted`, advances the
  revision so a response from before the restart can never be applied, and
  **replays nothing**. `two_process_restart_interrupts_without_replay` proves
  that across a real process boundary: a child writes an in-flight record and
  exits hard, and the parent asserts what recovery did to it.
- Operator takeover, stop, and any withdrawal of Computer authority end the
  record durably rather than dropping it, so the account of what happened
  survives.
- **A refusal at selection is a record too.** When the policy engine refuses to
  start a run at all, `AdaptiveRecord::stopped_at_selection` writes a stopped
  record carrying the reason. Writing nothing there had two consequences: the
  operator projection read `None` for a run the host had just refused, so the
  reason survived only as an audit line; and the run still read "no adaptive
  record", so the very next objective — at any lower risk — opened a fresh
  selection on the same authorized run. That is the probe-then-proceed shape
  this layer exists to refuse, so a stopped run now stays stopped.

## Every provider attempt is counted, and refusals count as failures

`provider_attempts` is incremented **before** the request leaves. A timeout, a
transport failure, prose instead of a tool call, an unknown field, a stale
frame — all of them cost money and all of them consumed the run's allowance, so
all of them count exactly as much as a success. A turn is accounted for only
once the seal and the profile budget have both had their say: a body that parsed
and was then refused is a **failed** attempt, because charging it as accepted
would let a model that reliably proposes forbidden actions look as productive as
one that proposes valid ones — and would reset the consecutive-unusable-answer
streak that exists to stop that loop. `provider_attempts` is always
`accepted_attempts + failed_attempts`.

Provider-reported usage is recorded even when the body that carried it then
failed to parse: it was still billed, and dropping it would make a misbehaving
cheap model look cheaper than it is.

## Budgets that are advertised are enforced

`maxTurnMillis` wraps the provider call in a real timeout; exceeding it is a
counted failed attempt, not a wait. `maxRepairs` is spent through
`record_repair`, which returns `RepairBudgetExceeded` at the ceiling. A number
in the projection that nothing reads is a claim, not a control, so any budget
that could not be enforced was removed rather than published.

The same rule removed two signals. `low_confidence` had no producer — the
proposal wire schema carries no confidence field — and `contradictory_semantics`
needs an AX/pixel cross-check this build has no pixels for. Both are gone until
something can raise them.

## The safety floor is one constant

```rust
SafetyFloor::REQUIRED   // the only value of this type a caller can obtain
```

There is no `SafetyFloor::for_profile`. `AdaptiveProfile::safety_floor()` takes
`self` so call sites read naturally and then discards it, which
`profile_independent_safety_floor` asserts for all three profiles. The floor
holds:

- host verification of the frame the model claims to be looking at;
- proposal binding to one exact observation id **and** sequence;
- completion accepted only for the exact current run version and observation;
- no screenshot bytes across the model boundary, in any profile;
- no action from free-form prose;
- no automatic replay after an uncertain dispatch;
- at most two identical consecutive frames before the run must escalate or stop;
- at most two consecutive unusable answers;
- a 0.700 confidence floor, identical everywhere;
- at most two verification failures before halting rather than escalating again.

Four `const _: () = assert!(...)` blocks in `computer_profile::profile` fail the
**build** if a budget escapes the provider-neutral kernel ceiling, if budgets
stop being monotonic in `Economy <= Balanced <= HighAssurance`, if any profile
admits screenshot bytes, or if the floor stops holding the #435 invariants.

## One validation path

Model output becomes a proposal through exactly one function, in two ordered
stages:

1. `validate_proposal_universal` — every safety rule, **taking no budget
   argument**: staleness, the closed action grammar, argument coherence, element
   existence, enablement, sensitivity, and the advertised-action check.
2. `enforce_budget` — the active profile's ceilings. It can only ever reject
   more, and it runs strictly after stage 1.

There is no ordering in which a generous budget admits something stage 1
refused, because a profile has no argument to stage 1 to influence. Below both,
`computer_use::policy` revalidates target, grant, freshness, sensitivity, and
geometry immediately before dispatch, unchanged and profile-blind.

## Selection and escalation

`AdaptivePolicyEngine` is stateless and pure: no clock, no randomness, no
provider round-trip. The same evidence and the same signal always give the same
answer.

**Selection** takes capability evidence and a task risk class:

| Risk | Floor profile |
| --- | --- |
| routine | Economy |
| consequential | Balanced |
| destructive | High Assurance |

Risk is classified from the local operator's objective text and the
observation's own sensitivity — never from model output, because a model that
wants to delete something has every incentive to describe it as tidying up.

Selection is clamped by `CapabilityEvidence::ceiling()`, the highest profile the
model *and* host can honestly support:

- a model that cannot emit structured tool calls, or whose Computer tier is
  unattributed, may not propose at all;
- a text-oriented model (no declared image input) is capped at Economy;
- a model qualified only against the deterministic simulator is capped at
  Economy, and the projection says `syntheticOnly: true`;
- High Assurance additionally requires a verifier **independent of the proposing
  model**.

When the risk floor exceeds the ceiling, the run **stops**. It does not run
under a profile the evidence cannot back. This is the difference between an
adaptive system and one that relabels a small model as a frontier one when the
task gets hard.

**Escalation** climbs one rung at a time, and every rung is attributable to
exactly one signal:

| Signal | Producer | Outcome |
| --- | --- | --- |
| `ambiguous_observation` | duplicate accessible names among rendered actionable candidates | escalate, or stop at the ceiling |
| `missing_semantics` | zero actionable elements in the bounded view | escalate, or stop at the ceiling |
| `repeated_stationarity` | the same structural frame digest, twice over | escalate, or stop at the ceiling |
| `repeated_uncertainty` | consecutive unusable model answers | escalate, or stop at the ceiling |
| `verification_failed` (first) | a dispatch the postcondition frame did not confirm | escalate, or stop at the ceiling |
| `verification_exhausted` (second) | the same, again | always stop |
| `capability_generation_changed` | the #458 digest moved under a live decision | always stop |
| `capability_revoked` | operator takeover, stop, or authority withdrawal | always stop |
| `higher_risk_objective` | a later objective above the run's risk high-water | always stop |
| `turn_budget_exceeded` | `maxTurnMillis` elapsed | always stop |
| `repair_budget_exceeded` | `maxRepairs` spent | always stop |
| `budget_exhausted` | `maxModelCalls` spent | always stop |

There is no "continue anyway" arm. `every_signal_resolves_to_escalate_or_stop_and_never_to_continue`
asserts that exhaustively across the signal vocabulary and all three profiles.

Escalation never exceeds the ceiling; at the top of the ladder there is nothing
to buy, so the run stops. Absent model-reported confidence counts as low
confidence, because an absent number is not a high number.

## Where it is wired

`AgentHostHandle::propose_computer_action` is the runtime boundary the desktop
cockpit and any headless caller both go through. Per turn it:

1. resolves the route and confirms the model may propose at all (unchanged);
2. builds evidence from the **same** capability record the eligibility check
   read, so one route cannot yield two answers;
3. classifies task risk from the objective and the observation;
4. selects or resumes the run's `AdaptiveController` and admits one turn through
   its compare-and-swap revision;
5. runs stationarity detection **before** spending a model call;
6. asks the model under the admitted profile's budget.

`AdaptiveController` owns everything that accumulates: the profile in force,
bounded escalation history, the stationarity window, spend, and the terminal
outcome. `begin_turn` refuses a stale revision, so a late response from a
cancelled inference, a duplicate cockpit request, or a second racing caller
cannot advance the run. `recover_interrupted` is the restart path: interrupted,
in-flight turn dropped, revision advanced, **nothing replayed** — the same
posture the durable Computer Run takes.

Adaptive state is discarded on operator takeover, stop, completion, and any
withdrawal of Computer authority, so a new run never inherits a previous run's
profile, escalation history, or spend.

## What the operator reads

`AdaptiveProfileProjection` rides on `ComputerRunProjection` — the *shared*
read shape the cockpit, the MCP read surface, and any SDK consumer already
consume — as well as on `ComputerCockpitSnapshot`. Like the run projection, it is redaction-safe **by construction**:
there is no field for an element label, value, geometry, evidence token, or
frame digest.

It carries the profile (canonical name), the reason code and its fixed operator
sentence, the risk class, the capability evidence behind the decision, the
budget in force, the safety floor (so an operator can see for themselves that it
does not move), every escalation with its cause and revision, spend, the
stationary repeat count, whether the view the model saw was bounded, and — once
the run ends — the exact terminal reason.

Unknown is a value. `promptTokens` and `completionTokens` are `null` until a
provider actually reports usage; they are never zero-filled and never estimated.
There is no `costUsd` field at all, because this process has no price table and
a field that is always null is a promise it might one day not be.

## Bounded candidate ranking

Beyond a profile's element ceiling the host ranks candidates deterministically —
focused and actionable first, then actionable, then focused, then the rest, then
disabled, ties broken by element id — and renders the bounded prefix, marking
the payload truncated. Hard-denied elements are dropped before ranking.

Bounding only ever narrows the model's choices; the kernel still revalidates
against the full current observation. The operator projection reports
`observationTruncated`, so a failed Economy step reads as *bounded view* rather
than as *bad model*, and a bounded view that could not find the right control is
a legitimate reason to escalate.

## Deterministic offline campaign

`tests/computer_use_adaptive_profile.rs` runs 4 frames × 5 adapters × 5 repeats
× 3 profiles = **300 episodes**, with no provider call, no socket, no screen,
and no dispatch. Adapters cover a text-only gateway, a weak multimodal model
reaching for coordinates it was never given, a malformed and overconfident
model, a stationarity loop, and a frontier-class model.

It asserts three properties:

1. anything the safety-only validator refuses is refused in every profile
   (`safety_bypasses == 0`);
2. for every proposal any profile accepted, `ComputerPolicy::authorize_action`
   returns the identical verdict (`kernel_disagreements == 0`);
3. zero dispatches.

A synthetic PASS means the code refuses what it says it refuses, on these
fixtures. It is not evidence that a live model can drive a real application.

## The whole path, end to end

`tests/computer_use_adaptive_end_to_end.rs` walks the full sequence rather than
any one stage of it:

```text
host admission -> provider -> seal -> profile budget
               -> operator approval -> dispatch -> postcondition -> completion
```

Each gate asserts two things a single-stage test cannot. First, **which stage
refused** — a test that only checks "this failed" cannot distinguish a budget
rejection from a seal rejection, and that distinction is the safety argument:
the seal refuses first, and the profile can only ever refuse *more*. Second,
**zero dispatches on any refusal** — the fixture backend counts every `act` it
is asked to perform, so a refusal anywhere before dispatch has to leave that
counter untouched.

The gates are: the happy path (exactly one dispatch, then a verified completion
on the host-issued receipt); a forged completion refused at the seal; an
oversized text entry the kernel would have accepted and the Economy budget does
not; a declined operator approval; a later higher-risk objective stopped at
admission before any spend; a stopped run that admits nothing afterwards; and a
provider transport failure that is still a paid attempt.

## The wire shape is pinned twice

`ComputerRunProjection` is the read shape the cockpit, the MCP surface, and SDK
consumers all share, so adding a key to it changes a public contract. It is
pinned in two places, and both lists must be updated together:

- `tests/mcp_sdk_interop/run_computer_reads_smoke.mjs` — `PROJECTION_KEYS` and
  `ADAPTIVE_KEYS`, checked by an independent Node client over real loopback
  HTTP;
- `the_projection_wire_shape_is_pinned` in
  `tests/computer_use_adaptive_durability.rs` — the same pin in the ordinary
  bridge suite.

The Rust pin exists because the Node one needs an `npm ci` behind it: adding
`adaptive` to the projection passed every fast local check and was caught only
by a hosted run. The Rust pin now fails first, in a second, with both lists
printed.

## Relationship to the standalone evaluator

The naming contract here is the one the `#446`/`#448` evaluation lane published:
canonical `economy` / `balanced` / `high_assurance`, aliases ingest-only. The
production seam adopts that contract and adds the piece the evaluator
deliberately could not: `syntheticOnly` travels to the operator projection, so a
simulator PASS is visible as exactly what it is. The monorepo does not treat a
synthetic PASS as live eligibility anywhere — a session-measured model is capped
at Economy by `CapabilityEvidence::ceiling`.

## Residuals not closed by this seam

- **No independent postcondition verifier, so High Assurance is unreachable.**
  `HOST_INDEPENDENT_VERIFIER_AVAILABLE` is `false`. #473 gives the host a real
  receipt and a single verifying frame, which is what makes *completion*
  honest — but that frame is captured by the same host loop that dispatched the
  action, not by an independent checker, and there are no pixels to cross-check
  it against. Destructive objectives therefore stop with
  `independent_verifier_unavailable`. Real pixels **and** an independent
  verifier are both prerequisites; flipping the constant is the whole change
  once they exist.
- **The #458 generation here is this lane's own.** A dedicated implementation
  lane owns the canonical capability generation. This layer computes and binds
  an equivalent digest so it can refuse a downgrade today; when the canonical
  one lands, this becomes a swap of the digest's source, not of its use.
- **No live model campaign.** No company-gateway, small multimodal, or frontier
  model has run the #435 evaluation matrix. No measured cost, latency, or safety
  comparison exists.
- **No packaged macOS qualification.** Neither a semantic Economy task nor an
  isolated-visual High-Assurance task has run on an assembled build.
- **Cross-frame progress signatures**, witnessed waiting, and durable
  publication of boundary refusals belong to
  [#465](https://github.com/chriscase/GrokPtah/issues/465), not here.
- **Provider-reported confidence** is not yet carried on the proposal wire
  schema, so `low_confidence` is currently reachable only through the absent-
  confidence path.
