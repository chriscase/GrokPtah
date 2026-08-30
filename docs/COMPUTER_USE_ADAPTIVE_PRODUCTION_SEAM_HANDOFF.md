# Computer Use adaptive production seam handoff

Status: source candidate only; not live, packaged, VM, provider, or release qualification.

## Exact identity

- Base: `origin/main` at `948c30d2797f0080829e5ed829dcd25c8d8063e1`
- Candidate branch: `codex/adaptive-production-current-main-v2`
- Candidate head: `6261138f197f7b197fe8d3fb44cfc5438665a46a`

The branch ports only the adaptive planner/executor review seam onto current main. The historical donor was used as read-only reference; its stale durable, wait, authority, provider, desktop, and VM work was not carried over.

## What the seam guarantees

- `act` remains the unchanged default path and keeps its original mutation identity.
- Opt-in `act_with_plan` runs the typed adaptive review only after `ComputerPolicy::authorize_action` succeeds.
- The review can admit only an already-authorized action or refuse it; it cannot grant, retry, downgrade, re-observe, or dispatch.
- Economy, Balanced, and High Assurance profiles only tighten observation age, confidence, and ambiguity checks.
- Claims reject unknown fields and malformed confidence/candidate relationships.
- Human approvals are opaque, host-minted, bound to the live run/control epoch/observation, and never accepted from wire JSON.
- Replay identity includes an adaptive claim and a non-secret approval marker when present.
- The adaptive projection is redacted and omitted for plain actions, preserving existing MCP projection keys for existing clients.

## Validation

- `cargo fmt --all -- --check`
- `cargo metadata --locked --offline --no-deps --format-version=1`
- `cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all-targets -- --test-threads=1` — all tests passed, including 15 adaptive integration tests and the live MCP smoke.
- `cargo clippy --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all-targets -- -D warnings`
- `git diff --check`

## Residual gates

This does not prove company-gateway behavior, provider cost truth, packaged macOS permissions, signed helper/TCC admission, Virtualization.framework boot/input/cleanup, or a long soak. It also does not wire a planner provider or operator UI; those integrations must preserve the same policy-first ordering and opaque approval boundary.

Promotion requires independent review, hosted checks, and an explicit decision to extend the public MCP/protocol contract. Do not merge the stale donor branches or claim live eligibility from this source-only candidate.
