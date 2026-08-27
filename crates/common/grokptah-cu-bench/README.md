# grokptah-cu-bench

An adversarial Computer Use benchmark and qualification authority for
GrokPtah. It runs a fixed catalog of synthetic surfaces against an agent,
under three execution profiles and two model classes, and says whether that
agent is qualified — separating *how much it can do* from *what it is allowed
to do to the operator's machine*.

Nothing in this crate calls a model provider, touches a real screen, reads a
real file, or opens a socket. A run is a pure function of its inputs.

## Why the separation matters

The central claim is that an execution profile buys verification and a model
class buys capability, and **neither buys authority**:

| | may differ by profile | may differ by model class |
|---|---|---|
| task coverage, step/latency/token budgets | yes | yes |
| escalation headroom | yes | yes |
| authority and privacy thresholds | **no** | **no** |

A small local gateway model earns a *narrower* certificate, not a weaker one.
`tests/cu_bench_authority_parity.rs` enforces this: every proposal in the
corpus is pushed through the guard under all three profiles, and an authority
refusal in one that is an allow in another fails the build. The one knob a
profile may move is the observation freshness bound, and it may only move
tighter.

## What it measures

Eleven metric families, per (model class × profile) cell:

task success · unsafe-proposal rate · unnecessary-escalation rate ·
abstention quality · step count · latency budget · model-token budget ·
recovery success · deterministic replay · evidence completeness ·
zero-tolerance counters (authority violations, privacy violations, false
success, post-takeover actions, collateral effects)

All ratios are basis points computed with integer arithmetic, so a report is
byte-identical wherever it is produced.

## What it covers

Twenty-one hazard families, twenty-six scenarios. Four baseline workflows
(editor, file, browser, terminal) so that unnecessary escalation is
measurable, then: dynamic AX reorder, duplicated labels, menus and modals,
virtualized scrolling and narrow context, stale observations, unexpected
navigation, app/URL/window mismatch, prompt injection, credential/path/
clipboard leakage, ambiguous pixels, stationarity loops, crash and restart,
operator takeover, competing agents, VM helper loss, network transitions, and
false-success traps.

`HazardFamily::ALL` is the contract; the gate test fails if a family has no
scenario.

## Negative controls

A benchmark whose only subject passes proves nothing about the benchmark.
Each scenario declares what a deliberately-bad agent must be caught doing —
`MustNotComplete`, `MustEarnAuthorityRefusal`, `MustFalselySucceed` — and
`tests/cu_bench_negative_controls.rs` runs the controls and checks it. The
controls cache element ids across observations, follow instructions they read
on screen, and claim success when they run out of plan.

## Running it

```
cargo test -p grokptah-cu-bench                              # everything
cargo test -p grokptah-cu-bench --test cu_bench_gate         # the CI gate
cargo run  -p grokptah-cu-bench --example calibrate          # print the matrix
cargo run  -p grokptah-cu-bench --example controls           # run the controls
cargo run  -p grokptah-cu-bench --example emit_artifacts     # regenerate artifacts
```

The gate test verifies the checked-in artifacts match what the code generates,
so `emit_artifacts` is how you accept an intentional change to the benchmark.

## Qualifying a candidate

Implement `agent::Agent` and hand it to `suite::run_matrix` through an
`AgentFactory`. The candidate never sees the world model, the mutation
schedule, the oracle, or the guard — it sees observations and produces
intents, which is the same boundary the reference agent works behind.

```rust
let scenarios = catalog::all();
let factory = |class, scenario: &Scenario| -> Box<dyn Agent> {
    Box::new(MyCandidate::new(class, scenario.goal.clone()))
};
let report = suite::run_matrix(&scenarios, &factory);
assert!(report.authority_clean());
```

## Artifacts

`artifacts/` holds the canonical fixture set — taxonomy, invariants, profiles,
thresholds, scenarios, workflow matrix — plus `manifest.json` digesting all of
it. Cite `manifestDigest` to say which benchmark a result came from.

## On comparisons

`matrix.rs` produces a representative workflow matrix shaped like the tables
general computer-use agents are measured on. That resemblance is the only
relationship: **no system outside this repository has been run through these
fixtures**, so this crate supports no comparative claim of any kind.
`ExternalComparison` has exactly one variant to keep that from being softened
by omission, and `head_to_head_protocol()` states what a real comparison would
require.

## Known limits

- **Modeled, not measured.** Token and latency figures come from a declared
  cost model, not a stopwatch. They compare configurations on identical work;
  they are not predictions of production cost.
- **Integer geometry.** Production geometry is `f64`; fixtures use integers so
  digests are reproducible across targets.
- **No long-horizon lane.** Every scenario is bounded by a step budget and runs
  against one surface. Nothing here speaks to drift over a long session.
- **Screenshots are digests.** Bounded region digests with an ambiguity flag.
  Enough to score whether a model guesses at pixels it cannot read; silent on
  whether real image understanding would have resolved the region.
- **Hardware and provider behaviour are out of scope.** See
  `docs/COMPUTER_USE_BENCHMARK.md`.
