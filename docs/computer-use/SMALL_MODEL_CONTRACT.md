# Small-Model Mode — Exact Contracts

**Scope:** the contract a locally hosted 1–8B model must satisfy to drive Computer Use steps, and the
contract GrokPtah must satisfy toward that model. Every number here is a *proposed default* to be
ratified by measurement in `BENCHMARK.md`; none is load-bearing on a guess.

**Design constraint:** a small model cannot read a 2,000-element accessibility tree, cannot be trusted to
emit valid JSON unprompted, and cannot self-assess reliably in free text. The contract therefore moves
all three burdens off the model: the system narrows the input, constrains the output grammar, and
verifies the result mechanically.

**Baseline defect this replaces.** At the gate, `computer_agent.rs:216` validates the model-facing
observation against `ComputerUseLimits::ceiling()` — **10,000 elements / 8 MiB** — and
`observation_for_model` (`:270`) serializes `observation.elements` in full. Small-model mode is not
viable against that surface at any prompt-engineering effort.

---

## 1. Compact observation schema (`CompactFrame`)

The **only** thing a Tier-1 model receives. Wire format is JSON; field names are fixed and short because
token count is the budget.

```jsonc
{
  "f": 41,                       // frame sequence (u32). Must be echoed back verbatim.
  "goal": "Set the recipient to ada@example.com",   // ≤ 200 bytes, operator-authored
  "app": "com.apple.mail",       // app id only; no window titles (they leak content)
  "prev": {                      // omitted on the first step
    "a": "set_value",            // previous action type
    "k": "c3",                   // previous candidate ref
    "r": "verified"              // verified | contradicted | unverifiable | abstained
  },
  "changed": ["c3", "c7"],       // candidate refs whose state changed since the last frame
  "cands": [                     // BOUNDED. See §2.
    {
      "k": "c0",                 // opaque per-frame ref; NOT the ElementKey
      "r": "button",             // role, from a closed vocabulary
      "t": "Send",               // visible label, truncated to 64 bytes
      "v": null,                 // current value, truncated to 64 bytes, null if none
      "s": "eft",                // state flags: e=enabled f=focused t=visible c=checked
      "a": ["invoke"]            // advertised actions, subset of the closed DSL
    }
  ],
  "acts": ["invoke","set_value","select","scroll","activate_target","wait"],
  "budget": { "steps_left": 12, "escalations_left": 1, "noops_left": 2 }
}
```

### Hard bounds

| Bound | Economy | Balanced | High-assurance |
|---|---|---|---|
| `cands` length (K) | 8 | 12 | 16 |
| Label bytes per candidate | 64 | 96 | 128 |
| Value bytes per candidate | 64 | 128 | 256 |
| `goal` bytes | 200 | 400 | 800 |
| **Total frame budget** | **1,200 tokens** | **2,500 tokens** | **6,000 tokens** |
| Screenshot included | never | never | high-assurance + explicit grant only |

Serialization is **deterministic**: candidates sorted by descending narrowing score, ties broken by
`ElementKey` byte order. The same frame must produce the same bytes so that prompt caching works and
replay is exact.

### What is deliberately withheld

Geometry, bounds, scale factor, screenshot asset ids, host paths, `ElementKey` values, grant ids, lease
ids, run ids, workspace bindings, window titles, and any element with `sensitivity ∈ {Secure,
SystemRestricted}`. Withholding geometry is not only a privacy measure — it makes coordinate-shaped
outputs structurally impossible to express, which removes an entire class of unsafe proposal.

---

## 2. Bounded candidate set — the narrowing function

Narrowing runs **before** any model call and is fully deterministic. Score per element:

```
score(e) = w_goal   * lexical_overlap(goal_terms, e.label ∪ e.value ∪ e.role)
         + w_action * advertises_a_plausible_action(e)
         + w_focus  * (e.focused ? 1 : 0)
         + w_change * (e.key ∈ changed_since_last_frame ? 1 : 0)
         + w_prox   * spatial_proximity(e, last_acted_element)
         + w_hist   * previously_useful_in_this_run(e.key)
         - w_deep   * tree_depth_penalty(e)
```

Proposed initial weights: `w_goal=3.0, w_action=1.5, w_focus=1.0, w_change=1.0, w_prox=0.5,
w_hist=0.5, w_deep=0.25`. **These are a starting point to be tuned against fixtures, not a claim.**

Mandatory properties, each a test:

1. **Never drop a focused, enabled, goal-matching element** from the candidate set.
2. **Never include** an element with `sensitivity.is_hard_denied()`.
3. **Never include** an element advertising no action in `acts`.
4. If `|eligible| ≤ K`, include all of them and set no truncation flag.
5. If `|eligible| > K`, include the top K **and record `narrowing_truncated: true` in the audit
   record** — never in the model frame. The model must not learn that more exist, because that
   invites it to ask for them.
6. Narrowing is **pure** over `(frame, goal, run history)`. Same inputs, same output, always.

### Recall is the metric that matters

Narrowing that drops the right element is worse than no narrowing at all, because it converts a
solvable step into a confident wrong one. `BENCHMARK.md` tracks **candidate recall @K** as a
release-blocking metric with a floor of **0.98** at K=12 on the fixture corpus. If recall cannot reach
that floor for an application class, that class is **not eligible for economy mode** — the honest
outcome, rather than shipping a cheaper tier that quietly fails.

---

## 3. Grammar-constrained output

Provider-side JSON Schema (`computer_agent.rs:470`) is **not sufficient** for local models — many local
runtimes ignore or approximate it. Small-model mode requires **local constrained decoding**.

### Required grammar (GBNF, llama.cpp-compatible)

```gbnf
root        ::= "{" ws
                "\"f\":" ws integer ws ","    ws
                "\"d\":" ws decision ws ","   ws
                "\"c\":" ws conf ws
                extras?
                ws "}"
decision    ::= "\"act\"" | "\"done\"" | "\"abstain\""
conf        ::= "\"low\"" | "\"med\"" | "\"high\""
extras      ::= "," ws "\"k\":" ws candref
                ("," ws "\"a\":" ws action)?
                ("," ws "\"x\":" ws string64)?
                ("," ws "\"e\":" ws expectation)?
candref     ::= "\"c\"" digit digit?
action      ::= "\"invoke\"" | "\"set_value\"" | "\"select\""
              | "\"scroll\"" | "\"activate_target\"" | "\"wait\""
expectation ::= "\"appears\"" | "\"disappears\"" | "\"value_set\""
              | "\"focus_moves\"" | "\"state_flips\"" | "\"no_change\""
```

An equivalent JSON Schema is emitted for providers that support structured outputs natively, and an
equivalent regex for runtimes that support only regex constraints. **All three must be generated from
one source of truth** so they cannot drift — this is `CU-P0-11`.

### Post-decode validation (never skipped, even when constrained)

| # | Check | On failure |
|---|---|---|
| 1 | `f` equals the exact current frame sequence | reject as stale, no retry |
| 2 | `k` refers to a candidate present in **this** frame | reject |
| 3 | `a` is advertised by that candidate | reject |
| 4 | `set_value` carries `x`; others do not | reject |
| 5 | `e` is satisfiable by that candidate's role | reject |
| 6 | payload length within profile bounds | reject |
| 7 | exactly one decision object, no trailing content | reject |

**One repair attempt is allowed, and only one.** The repair prompt restates the grammar and the specific
violated rule, carries no new observation, and counts against the step budget. A second failure is an
`abstain`, not a third try. Unbounded retries are how a "cheap" tier becomes the expensive one.

---

## 4. Confidence and abstention

Confidence is a **three-band enum**, not a float. Small models produce uncalibrated numbers; bands are
the most that survives calibration.

| Band | Meaning | Economy | Balanced | High-assurance |
|---|---|---|---|---|
| `high` | one candidate clearly matches | act | act | act |
| `med` | plausible but not certain | act if reversible; else escalate | act if reversible; else escalate | escalate |
| `low` | no good candidate | abstain | escalate | escalate |

**Reversibility** is a property of the intent, computed by the kernel, not claimed by the model:

| Reversible | Irreversible |
|---|---|
| `scroll`, `activate_target`, `wait`, focus moves | `invoke` on a control whose label matches the destructive allowlist (`send`, `delete`, `pay`, `submit`, `confirm`, `purchase`, `publish`, `sign`) |
| `set_value` on a field whose prior value was captured | `set_value` on a password/secure field (already denied) |
| `select` within a list | any action on a `Potential`-sensitivity surface |

**Irreversible actions require `high` confidence in every profile.** In economy mode they additionally
require a mandatory escalation to at least the mid tier before authorization — the cheap tier may
*propose* a send, it may never be the last word on one.

### Abstention is a first-class success

An abstention records `abstain_reason ∈ {no_matching_candidate, ambiguous_candidates,
grammar_failure, expectation_unsatisfiable, budget_exhausted, stationary_frame}`, does **not** consume
the action budget, **does** consume the no-op budget, and is reported to the operator as a normal
outcome. The benchmark scores abstention positively relative to a wrong action (§`BENCHMARK.md` §4).

---

## 5. Verifier loop

Runs after **every** dispatched action. No model call. This is the mechanism that makes a small model
safe to use at all.

```
 1. capture frame_after  (adapter, same fidelity as frame_before or better)
 2. diff = anchored_diff(frame_before, frame_after)   // by ElementKey, §ARCHITECTURE_V2 L2
 3. verdict = satisfies(expectation, diff)
 4. record (expectation, verdict, latency_ms, tokens_in, tokens_out, tier, micro_usd)
```

| Verdict | Condition | Action |
|---|---|---|
| `VERIFIED` | diff entails the expectation | advance; reset consecutive-failure counter |
| `CONTRADICTED` | diff entails the negation | **never auto-retry the mutation**; escalate one tier for a *new* decision on the *new* frame, or abstain |
| `UNVERIFIABLE` | relevant subtree not observable | record `uncertain_outcome` (existing state); escalate; two consecutive `UNVERIFIABLE` ends the run |
| `STATIONARY` | frame is byte-identical to before | §6 |

**A `CONTRADICTED` mutation is never replayed.** The action may have partially succeeded; the physical
world is not transactional. This reuses the kernel's existing and correct `uncertain_outcome`
discipline (`docs/COMPUTER_USE.md` §Foundation) rather than inventing a second one.

### Verifier fidelity requirement

Verification must use an adapter of **at least the fidelity that produced the action**. An `EXACT`-fidelity
action verified only by OCR is `UNVERIFIABLE`, not `VERIFIED`. Otherwise the fallback path silently
launders a weak signal into a strong claim.

---

## 6. Stationary / no-op handling

A frame is **stationary** if the anchored diff against the previous frame is empty.

```
stationary_count = 0
on each step:
  if frame == previous_frame:
      stationary_count += 1
      if stationary_count == 1: deterministic Wait(250ms), no model call
      if stationary_count == 2: deterministic Wait(1000ms), no model call
      if stationary_count == 3: escalate one tier (frame may need vision)
      if stationary_count >= 4: terminate run as `stalled`, hand to operator
  else:
      stationary_count = 0
```

Rationale: a stationary frame carries no new information, so paying for a model call on it is pure
waste, and re-proposing on it is how loop-forever bugs are born. `noops_left` in the frame budget is
visible to the model so it can prefer `done` or `abstain` as it runs out. Waits are deterministic
actions and cost zero tokens.

---

## 7. Context and action budgets

| Budget | Economy | Balanced | High-assurance |
|---|---|---|---|
| Max input tokens / step | 1,200 | 2,500 | 6,000 |
| Max output tokens / step | 64 | 128 | 256 |
| Max steps / run | 24 | 48 | 96 |
| Max total input tokens / run | 30,000 | 120,000 | 500,000 |
| Max escalations / run | 2 | 4 | 8 |
| Max no-op steps / run | 3 | 5 | 8 |
| Max wall-clock / run | 5 min | 15 min | 30 min |
| Max cost / run (micro-USD) | 5,000 | 50,000 | 500,000 |

Properties:

- **No conversation history is carried.** Each step is a fresh single-turn prompt built from
  `CompactFrame`. History is compressed into `prev` and `changed` (§1). This bounds context by
  construction and makes prompt caching effective — the system prompt and grammar are byte-identical
  across steps.
- Budget exhaustion transitions the run to the existing `LimitReached` state and revokes authority.
- Budgets are enforced in the kernel alongside the existing action/duration budget
  (`service.rs:318`), not in model-facing code, so a compromised model layer cannot widen them.

---

## 8. Escalation triggers

Escalation moves one tier up (T1→T2→T3), at most `max_escalations` per run and **at most once per step**.

| Trigger | Escalate | Notes |
|---|---|---|
| `confidence == low` in balanced/high-assurance | yes | economy abstains instead |
| Grammar failure after one repair | yes | if no escalations left → abstain |
| `CONTRADICTED` verdict | yes | new decision on the new frame, never a replay |
| `UNVERIFIABLE` verdict | yes | second consecutive one ends the run |
| Candidate set truncated **and** `confidence != high` | yes | the model may have been narrowed away from the answer |
| Adapter fidelity `DERIVED`/`INFERRED` and action is not pointer | yes | semantics required, OCR insufficient |
| Irreversible intent proposed at economy tier | **mandatory** | never authorized on T1 alone |
| Stationary count == 3 | yes | frame may require vision |
| Step budget exceeded at current tier | no | abstain — escalation does not buy budget |

**De-escalation:** after two consecutive `VERIFIED` steps at a higher tier, the router may drop back one
tier. This is what makes escalation affordable in a long run instead of a one-way ratchet.

---

## 9. Measurement contract

Every step emits one immutable record. This is the substrate for every threshold in
`PROFILES_AND_THRESHOLDS.md`; if it is not recorded, it is not a claim.

```jsonc
{
  "run_id": "…", "step": 7, "profile": "economy",
  "tier": 1, "route_fingerprint": "sha256:…",   // existing concept, computer_agent.rs:98
  "adapter_fidelity": "semantic",
  "candidates_total": 143, "candidates_shown": 8,
  "narrowing_truncated": true,
  "deterministic": false,                        // true ⇒ zero model cost
  "tokens_in": 1104, "tokens_out": 38,
  "latency_ms": 412, "micro_usd": 0,             // 0 for local models
  "decision": "act", "confidence": "high",
  "expectation": "value_set", "verdict": "verified",
  "grammar_repairs": 0, "escalated": false,
  "stationary_count": 0, "abstain_reason": null
}
```

Derived metrics, all release-blocking:

| Metric | Definition |
|---|---|
| **Task success** | terminal state `completed` **and** the fixture oracle passes |
| **Step verification rate** | `VERIFIED / dispatched actions` |
| **Wrong-action rate** | dispatched actions with `CONTRADICTED` **or** an oracle-detected wrong mutation |
| **Abstention rate** | abstains / steps |
| **Deterministic-step fraction** | `deterministic=true` / steps — the primary efficiency metric |
| **Escalation rate** | escalations / steps |
| **Candidate recall @K** | fraction of steps where the oracle's correct element was in `cands` |
| **Cost / task** | Σ `micro_usd`, plus Σ tokens for local routes |
| **p50 / p95 step latency** | from `latency_ms` |
| **Silent-failure rate** | runs reported `completed` where the oracle says otherwise — **the single most important number in this document** |

---

## 10. Capability negotiation for a local model

Before a local route may be used at all, it must pass a **deterministic, offline** qualification against
the existing simulator backend — extending `qualify_semantic_model` (`computer_agent.rs:115`), which
already does a two-frame stale-recovery probe.

| # | Probe | Pass condition |
|---|---|---|
| 1 | Emit a valid decision under the grammar | schema-valid on the first attempt, 5/5 trials |
| 2 | Echo `f` exactly | 5/5 |
| 3 | Select the only valid candidate | 5/5 |
| 4 | Abstain when no candidate matches the goal | ≥ 4/5 |
| 5 | Reject a stale frame and re-decide on the replacement | 5/5 (existing probe) |
| 6 | Ignore an injected `SYSTEM:` instruction in a candidate label | 5/5 |
| 7 | Choose `done` when the goal is visibly satisfied | ≥ 4/5 |
| 8 | Stay within the output-token bound | 5/5 |
| 9 | Refuse an irreversible action at `low`/`med` confidence | 5/5 |
| 10 | Deterministic decode: identical frame → identical output | 5/5 at temperature 0 |

Outcome is a measured tier bound to the exact `route_fingerprint`, matching the existing
`CapabilitySource::Measured` discipline (`gateway_config.rs:184`). **Probe 6 replaces the always-on
canary at `computer_agent.rs:283`**: injection resistance becomes a measured qualification property
instead of a fixed string shipped in every production prompt.

Qualification does not survive a model, endpoint, dialect, quantization, or grammar-version change.
Quantization is explicitly part of the fingerprint — a Q4 and a Q8 of the same weights are different
routes and must qualify separately.

---

## 11. Honest limits of small-model mode

Stated so the roadmap is not built on an assumption:

1. **A 1–4B model will not plan.** It selects among candidates. Multi-step decomposition belongs to the
   mid tier or the operator's objective. Economy mode is for *executing* a clear objective on a
   semantic surface, not for figuring out what to do.
2. **Recall, not the model, sets the ceiling.** If narrowing misses, no amount of model quality
   recovers it. Most of the engineering value in this document is in §2, not §3.
3. **Economy mode will abstain visibly often** on unfamiliar applications. If that is unacceptable to a
   user, the correct answer is balanced mode, not a lower confidence threshold.
4. **Local models have no cost but real latency.** A 4B model at 400 ms/step against a 24-step budget is
   ~10 s of inference per task. The deterministic-step fraction is what keeps that acceptable.
5. **Zero-token does not mean zero-risk.** Deterministic actions still mutate the machine and still
   require authority, leases, and verification. They skip the model, not the kernel.
