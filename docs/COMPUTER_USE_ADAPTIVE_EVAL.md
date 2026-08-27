# Computer Use adaptive evaluation (issues #435 / #272)

Deterministic, provider-neutral evaluation harness for adaptive Computer Use
profiles. Implementation lives in [`evals/computer-use-adaptive/`](../evals/computer-use-adaptive/README.md).

This document is the operator handoff. It is not a production runtime.

## Status

Synthetic campaign is the qualification evidence this lane can produce without
live providers or packaged macOS hardware. Simulator PASS does **not** certify
release (#435 packaged Economy + isolated-visual High Assurance remain open;
#272 live gateway sampling remains open).

## Commands

```sh
cd evals/computer-use-adaptive
cargo test --locked -- --test-threads=1
cargo run --locked --bin grokptah-cu-adaptive-eval -- --out campaign-out --repeats 5 --seed 435272
cargo run --locked --bin grokptah-cu-adaptive-eval -- \
  --verify-report campaign-out/campaign-report.json \
  --verify-evidence campaign-out/campaign-evidence.json
```

Do not run `cargo test --workspace`. Do not `cargo clean`. Do not overlap a
protected target directory.

## Schemas

- `evals/computer-use-adaptive/schemas/grokptah-cu-eval-scenario.v1.schema.json`
- `evals/computer-use-adaptive/schemas/grokptah-cu-eval-result.v1.schema.json`
- `evals/computer-use-adaptive/schemas/grokptah-cu-eval-evidence.v1.schema.json`
- `evals/computer-use-adaptive/schemas/grokptah-cu-eval-evidence-set.v1.schema.json`
- `evals/computer-use-adaptive/schemas/grokptah-cu-eval-report.v1.schema.json`

Live continuation must reuse these exact schema versions. Fake adapter success
never becomes `live_authoritative`.

## Naming

Canonical: Economy / Balanced / High Assurance (`economy`, `balanced`,
`high_assurance`). Compatibility aliases: `efficient`→`economy`,
`frontier`→`high_assurance`. See
[`evals/computer-use-adaptive/docs/DECISION_PACKET.md`](../evals/computer-use-adaptive/docs/DECISION_PACKET.md).
