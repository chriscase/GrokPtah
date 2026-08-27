# grokptah-cu-adaptive

An adaptive Computer Use planner/executor contract that runs the same way with
a small, cheap, locally served model and with a strong hosted one, plus a
deterministic synthetic benchmark that exercises it at 3-, 30-, and 300-step
horizons.

This crate sits **above** the provider-neutral safety kernel in
`grokptah_agent_bridge::computer_use`. It never replaces that kernel and never
widens it: every refusal maps onto a kernel error code, every dispatchable
intent maps onto a kernel action, and every bound is at or inside the kernel's
own. The bridge-side test
`crates/codegen/grokptah-agent-bridge/tests/computer_adaptive_conformance.rs`
asserts each of those so the two cannot drift apart quietly.

**Nothing here runs a model, calls a provider, opens an application, requests a
permission, or dispatches input.** The benchmark's world is a deterministic
in-process fixture; its cost and latency figures are synthetic accounting units
that do not convert into tokens, currency, or milliseconds on any real system.
Every receipt carries the full disclaimer set saying so, and reconciliation
refuses a receipt that drops any of it.

## Shape

| Module | What it holds |
|---|---|
| `vocabulary` | closed refusal / escalation / approval / stop vocabularies |
| `schema` | the plan and verdict contract, `deny_unknown_fields` throughout |
| `redaction` | the sensitivity ladder and text that cannot be serialized |
| `profile` | the three efficiency profiles and the authority invariants |
| `tier` | declared model-class capabilities and their floors and ceilings |
| `confidence` | thresholds and the strictness ladder |
| `grounding` | semantic and region grounding, verified against the live frame |
| `budget` | envelopes, ledgers, deadlines |
| `lease` | single-holder lease, compare-and-swap, frame tokens |
| `gates` | mandatory human approval gates |
| `escalation` | the ladder, carrying authority forward unchanged |
| `cancel` | cancellation and idempotent cleanup |
| `executor` | admission, re-derivation, and disagreement |
| `ledger` | exact counters plus a bounded event tail |
| `receipt` | receipts that are derived, re-checkable, and explicit |
| `bench` | the synthetic world, scenarios, reference planner, runner, suite |

## Verification

```sh
cargo fmt -p grokptah-cu-adaptive -- --check
cargo clippy -p grokptah-cu-adaptive --all-targets --locked -- -D warnings
cargo test -p grokptah-cu-adaptive --locked
```

Full design notes, the profile table, the scenario families, and the list of
things the benchmark explicitly does not measure are in
[`docs/COMPUTER_USE_ADAPTIVE.md`](../../../docs/COMPUTER_USE_ADAPTIVE.md).
