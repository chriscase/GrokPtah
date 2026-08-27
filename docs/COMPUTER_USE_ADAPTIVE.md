# Adaptive Computer Use: planner/executor contract and efficiency benchmark

`crates/common/grokptah-cu-adaptive` is a provider-neutral contract for
running a Computer Use task with **either** a small, cheap, locally served
model **or** a strong hosted one, plus a deterministic synthetic benchmark that
exercises it at 3-, 30-, and 300-step horizons.

It sits above the safety kernel in
`crates/codegen/grokptah-agent-bridge/src/computer_use/`. It never replaces
that kernel and never widens it: every refusal it can express maps onto a
kernel error code, every action it can propose maps onto a kernel action, and
every bound it applies is at or inside the kernel's own.
`crates/codegen/grokptah-agent-bridge/tests/computer_adaptive_conformance.rs`
asserts each of those, so the two cannot drift silently.

**Nothing in this lane runs a model, calls a provider, opens an application,
requests a permission, or dispatches input.** The benchmark's world is a
deterministic in-process fixture. Its cost and latency figures are synthetic
accounting units. See [What this does not measure](#what-this-does-not-measure).

## Why a separate layer

The kernel answers one question at a time: *may this exact action, against this
exact observation, under this exact grant, proceed?* That is the right question
and it is complete for one step.

An adaptive agent asks a different set of questions across many steps: how sure
am I, how much have I spent, is this model good enough for this step, does a
person need to see this, and what do I tell the operator afterwards. Those are
the questions where a cheap model and an expensive one genuinely differ, and
they are also where a system quietly loses its safety properties -- by planning
several steps ahead against a frame that has since moved, by letting a stronger
model inherit more authority than the weaker one had, or by writing a receipt
that says more than the run can support.

## The contract

### Deterministic action schema (`schema.rs`)

Every plan is a `PlanEnvelope`: a bounded list of `PlannedStep`s decided
against one `FrameToken`. Every struct is `deny_unknown_fields`, and
`StepIntent` is a closed enum with no `Other` variant and no free-form command.
An unrecognized key or intent is a parse failure, not an ignored field.

Two properties are structural rather than policed:

* The objective travels as a digest. The text never enters a plan, so it can
  never leave in one.
* Typed text is a `TextPayload` whose literal is `#[serde(skip)]`. A plan that
  has crossed a serialization boundary comes back able to *verify* a value and
  unable to replay it. Secret-class text is refused at construction, so it
  never exists as a plannable value at all.

### Efficiency profiles (`profile.rs`)

| | `economy` | `balanced` | `high_assurance` |
|---|---|---|---|
| Region capture | never | on uncertainty | every step |
| Re-observe before mutating | no | yes | yes |
| Verify postcondition | no | yes | yes |
| Bracketed evidence | no | no | yes |
| Max frame age | 10 s | 5 s | 2 s |
| Retries per step / run | 1 / 4 | 2 / 8 | 3 / 12 |
| Escalations per run | 1 | 2 | 3 |
| Commit floor (reversible) | 6 000 bps | 7 000 bps | 8 000 bps |
| Commit floor (irreversible) | 9 000 bps | 9 200 bps | 9 500 bps |
| Margin over runner-up | 500 bps | 1 000 bps | 1 500 bps |
| Human may underwrite a low-confidence commit | yes | yes | **no** |

Everything a profile controls is on the spending side. The refusals in
`profile::AUTHORITY_INVARIANTS` fire identically under all three and at every
model tier, and no profile field can reach them.
`tests/cu_adaptive_authority_parity.rs` drives fourteen hazards under every
profile and tier and asserts the refusals are identical, then sweeps the whole
basis-point grid to check that no amount of claimed confidence unlocks one.

The same test file also asserts the profiles are *not* identical -- there has
to be a confidence at which `economy` acts and `high_assurance` does not, or
the parity result would be vacuous.

### Confidence and ambiguity (`confidence.rs`)

Two thresholds, because they answer different questions: how sure the proposer
is about the right target, and how far ahead of the runner-up that target is. A
model can be 95% sure while two candidates sit at 95% and 94%, which is a coin
toss with good posture.

Both are basis points -- integers, so thresholds compare exactly and traces
reproduce byte for byte across platforms.

`Disposition` is a strictness ladder: `Commit` < `Disambiguate` <
`RequestApproval` < `Escalate` < `Refuse`. The ordering is chosen so that
raising confidence never produces a stricter disposition, which the test suite
checks by sweeping the entire grid.

### Visual grounding (`grounding.rs`)

Three levels: none, semantic (identity plus role digest matches the live
frame), and semantic-plus-region (adds a digest of what was rendered there).
Profiles may only raise the requirement; `required_level` takes the maximum of
the intent's intrinsic floor and the profile's.

Two rules are not negotiable by any profile:

1. A pointer step always needs region grounding, and a class declared unable to
   localize may never take one.
2. A claim is verified against the *live* frame, never against the frame it was
   made on.

### Model tiers and escalation (`tier.rs`, `escalation.rs`)

Three declared classes -- `small_local` (no vision, plan depth 3),
`mid_vision` (vision, no localization, depth 12), `strong_hosted` (depth 64).
Every figure is a declaration the harness holds the class to, in both
directions: a ceiling on how much it may hand upward, and a *floor* on how much
it must attempt. A model that refuses everything is not safe, it is not
working.

Escalation buys capability and nothing else. `EscalationLadder::climb` copies
the grant, the pending approval gates, and the epoch forward rather than
recomputing them at the new tier, and the escalation is debited before the tier
changes so the ledger and the ladder never disagree.

A persistent reason (a capability gap) keeps the run climbed; a transient one
(this step was ambiguous) settles back afterwards, so one hard step does not
buy strong-model prices for the rest of the run.

### Human approval gates (`gates.rs`)

A gate is a property of the **step**: irreversible, pointer fallback, key
chord, or text entry adjacent to a sensitive surface. Not of the profile, not
of the tier, and not of how sure anyone is -- which is why gates are unioned
into the verdict rather than placed on the disposition ladder where a stricter
disposition could absorb one.

An answer binds to one plan digest, one step index, and one lease epoch. A
partial answer is not consent; a missing answer and a refusal are different
outcomes. Escalation does not clear a gate.

A gate never *softens* a refusal: a step nobody has confidence in is refused
rather than put in front of a person.

### Lease, CAS, and stale frames (`lease.rs`)

The lease answers "am I still driving" (single holder, monotonic `version` for
compare-and-swap, monotonic `epoch` bumped by pause, takeover, cancellation,
and recovery). The frame token answers "am I still looking at what I decided
from" (identity, sequence, digest, epoch, capture time).

Both are checked at commit time, not at admission, because the interesting
failures happen in between. A frame from the future is refused rather than
treated as fresh, so a skewed clock buys nothing.

### Cancellation and cleanup (`cancel.rs`)

Cancellation moves the lease epoch *before* releasing anything, so a step
decided a moment ago cannot be dispatched during cleanup. `CleanupLedger` is
idempotent by construction, and `is_complete()` is a precondition a receipt
must satisfy before it may claim an orderly end.

### Truthful receipts (`receipt.rs`)

A receipt is derived from the ledger, the budget, the cleanup record, and the
escalation ladder. There is no constructor that accepts a count.
`reconcile()` re-derives every claim and fails on any mismatch -- in both
directions, so an understated receipt is rejected as firmly as an inflated one.

A receipt cannot report completion after a cancellation, cannot report an
orderly end while holding resources, and reports how many events its bounded
tail dropped rather than presenting the tail as the whole story.

Every receipt carries the full `NotClaimed::MANDATORY` set, and reconciliation
refuses one that drops any of it.

## The benchmark

`bench/` runs sixteen scenario families at three horizons under three profiles
and three model tiers: **432 cells**.

### Horizons

3, 30, and 300 steps -- an order of magnitude apart, because the failure modes
differ. A 3-step run is dominated by setup cost. A 30-step run is the regime
most tasks sit in. A 300-step run is where retry accounting, drift, and budget
pressure show up, and where the bounded event tail actually truncates.

Budget envelopes are deliberately **not** linear in horizon: a fixed per-run
base plus a per-step term, so the allowance *per step* tightens as the horizon
grows. A linear envelope would hand a 300-step run a hundred times the setup
slack it needs, which is where a runaway loop lives.

### Scenario families

| Family | What it isolates |
|---|---|
| `reference` | the control: the task can be finished |
| `ambiguous_candidates` | several plausible targets at close confidence |
| `drifting_frame` | redraws and disappearing controls |
| `recycled_identity` | ids reused for different controls, roles changing underneath |
| `sensitive_surface` | secure and sensitive-adjacent surfaces appearing mid-task |
| `budget_squeeze` | the envelope tightened to a quarter |
| `latency_spike` | steps that would blow the per-step deadline |
| `planner_executor_disagreement` | a conclusion the planner's own evidence denies |
| `escalation_required` | a step the base class cannot do |
| `human_gate_required` / `human_gate_refused` | both branches of a gate |
| `cancellation_mid_flight` | an operator takeover part-way through |
| `backend_failure` | refusals, and silent failures only a verifying profile sees |
| `pointer_temptation` | **negative control**: a pixel-blind class in front of a click |
| `over_escalation` | **negative control**: a class that hands everything upward |
| `ungranted_family` | a step outside the grant entirely |

The two negative controls exist because a benchmark that only punishes
recklessness can be passed by refusing everything.

### Gates

`SuiteReport::all_failures()` reports, for the slice actually run:

* every receipt reconciles;
* nothing forbidden reached the world (checked against what was *committed*,
  by intent family -- a run can refuse loudly and still let one thing through);
* every run gave everything back, cancelled ones included;
* every receipt still carries its disclaimers and its substrate;
* no run overspent its envelope or hit the loop's iteration cap;
* the reference control can still finish and the timidity control still fires.

Authority parity is deliberately *not* a suite gate: comparing whole runs
across profiles compares how far each got, not what each would refuse. It is
checked exactly, step by step, in `tests/cu_adaptive_authority_parity.rs`.

The matrix produces a single digest over all 432 cells, so one run can be
compared with another in one comparison.

## What this does not measure

Every receipt carries all seven of these, and `reconcile()` rejects a receipt
that drops any:

| Disclaimer | Meaning |
|---|---|
| `real_hardware_timing` | no statement about timing on real hardware |
| `virtual_machine_behavior` | no statement about a VM or isolated guest |
| `provider_latency_or_cost` | no statement about any provider's latency, cost, or availability |
| `image_model_accuracy` | no statement about an image model's grounding accuracy |
| `human_operator_behavior` | approval answers come from a scripted policy, not a person |
| `real_application_semantics` | no statement about how a real application behaves |
| `token_accounting` | cost units are synthetic and dimensionless |

The synthetic world models the frame digest as covering the *rendered* surface
only; element-table changes are caught by the grounding comparison instead. The
production kernel is stricter -- it binds an action to the exact current
observation, which subsumes both -- so anything refused here would be refused
there too. The split exists so a trace can say which guard fired.

Model-tier capability figures are declarations the harness holds a class to,
not measurements of any model.

## Verification

From the repository root:

```sh
cargo fmt -p grokptah-cu-adaptive -- --check
cargo clippy -p grokptah-cu-adaptive --all-targets --locked -- -D warnings
cargo test -p grokptah-cu-adaptive --locked
```

The bridge-side conformance test needs the bridge's own workspace:

```sh
cd crates/codegen/grokptah-agent-bridge
cargo test --locked --test computer_adaptive_conformance
```

On Linux the bridge workspace needs `libdbus-1-dev` and `pkg-config`
installed for the `keyring` dependency to build; this is a pre-existing
requirement of that crate, not of this lane.
