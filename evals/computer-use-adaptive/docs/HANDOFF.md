# Adaptive Computer Use evaluation handoff

## Status: PASS (synthetic) / remaining live and packaged work is out of this lane

| Field | Value |
| --- | --- |
| Exact base | `origin/main` `c6f1cb23e9d6217005599850d9e0d6f7df64d5a1` |
| Branch | `grok/cu-adaptive-evaluator-authority-v2` (correction of #446; does not modify `grok/cu-adaptive-eval-harness-v1`) |
| Allowlist | `evals/computer-use-adaptive/**`, `docs/COMPUTER_USE_ADAPTIVE_EVAL.md` |
| Production runtime edited | **none** |
| Synthetic campaign | **PASS** |
| Unauthorized dispatches | **0** |
| Safety violations | **0** (release-failing=false) |
| Task success | **425 / 425** (repeats=5, seed=435272) |
| Provider calls | **0** |
| Cost USD | **null** (fake adapters; not fabricated) |
| Campaign digest (repeats=5, seed=435272) | `2fce11ff4e0de769267f4b22555a23029b34f7e3944679afffb1881489e74198` |
| Campaign digest (repeats=5, seed=435273) | `5cfab2c5acb95cfdb97972de16a4317d835e09316ee815f6b3f0693dab94b9bc` |
| Fixture hash | `614a8b4b0bf5d5f559764f894661475a11e75e1e40279bdbe5e48cf5387cc20a` |
| Verifier | **ok**, independent matrix reconstruction; 0 errors |
| Live / packaged macOS | **not claimed** |

Evaluator residuals closed on this head: `release_failing` is independently
recomputed and a disagreeing claim is `VERIFIER_ERROR`; evidence digesting and
unknown-surface observe return `Result` (no run-path `expect`/`panic`);
deny-unknown-fields and nullable `costUsd` remain required. Synthetic PASS is
still not live, VM, or production-runtime qualification.

### Schema compatibility / migration

- Report now requires `campaignDigest`, `metrics.observationCount`, `metrics.actionCount`, and `liveContinuation.receipts` (empty array on synthetic runs).
- Episode results require `expectedOutcome`, `expectedTaskSuccess`, `fixtureMatch`.
- Evidence objects require `repetition` and `contentSha256`.
- `campaign-evidence.json` is an **object** (`grokptah.cu_eval_evidence_set.v1` with `items[]`), not a bare array.
- Scenario documents require `schemaVersion`, `expected` as `{unauthorizedDispatch, safetyViolation, cells}`, and `splitVisual` / `pairDispatch`. World `successFlag` is required; `visualGrant` is an optional `{granted, grantId}` object. Agent `leaseState` replaces `leaseGranted`. Effects use `type`. Script events are `{type}` objects with `phase`.
- `liveContinuation.receipts` is required (empty array on synthetic). Extra trace fields fail closed.

### Distinctions

- **Synthetic PASS**: this harness's 5-repeat campaign. Not live eligibility.
- **Live eligibility**: structured `ProviderReceipt` with stable identity/digest. Not implemented in this lane; `GROKPTAH_CU_ADAPTIVE_LIVE=1` is refused.
- **Packaged qualification**: out of scope (#444 / #435 packaged macOS).
- **Production runtime readiness**: out of scope; this crate does not edit adaptive-profile runtime.

Unmerged Efficient/Balanced/Frontier runtime on developer checkouts is **not**
authoritative. This lane does not assume it.

## Naming decision

Canonical #435 names: `economy`, `balanced`, `high_assurance`.

Compatibility aliases (ingest only): `efficient` → `economy`,
`frontier` → `high_assurance`. Aliases are not a product rename and not a
weaker-safety mode. Economy is an efficiency policy with identical safety
authority.

Recorded in `src/naming.rs` and every campaign report `naming` object.

## Commands (verified)

```sh
cd evals/computer-use-adaptive
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --bin grokptah-cu-adaptive-eval -- --out campaign-out --repeats 5 --seed 435272
```

Focused crate only. Do not `cargo test --workspace`, `cargo clean`, or overlap a
protected target.

## Campaign evidence (repeats=5)

- Episodes: 2100 (12 families × variants × 3 profiles × 5 adapters × 5 repeats)
- Outcomes: success 425, fail_closed 790, no_progress 375, escalate 195, uncertain 180, abstain 135
- Stale-action attempts: 330 (denied; never unauthorized)
- Invalid actions: 3020 (malformed/denied; never unauthorized)
- Recovery after two restarts: 60/60 converged
- Economy image bytes: **0**; High Assurance image bytes: 12920
- Economy observation bytes: 657325; High Assurance: 846565
- Model units kind: `compact_observation_bytes` (not vendor tokens)
- Latency kind: `virtual_clock_ms`
- Held-out: `heldout.card2_ok`
- Fixture hash: `614a8b4b0bf5d5f559764f894661475a11e75e1e40279bdbe5e48cf5387cc20a`

## Fixture coverage

See `docs/COVERAGE_MATRIX.md`. Families:

1. unique semantic, no screenshot
2. duplicate names / contextual disambiguation
3. missing semantics / visual grounding
4. AX/pixel contradiction and stale observation
5. moving / resized / restarted target
6. repeated no-op / stationarity
7. sensitive / credential / system / prompt-injection label
8. takeover racing inference and dispatch
9. timeout before-send / after-send / after-input + two-restart crash cut
10. local semantic planner + separately authorized visual grounding
11. profile/model capability downgrade mid-run
12. same-domain contention and isolated domains

Fake adapters: tools-capable text-only, weak multimodal, malformed/overconfident,
stationarity-loop, frontier multimodal. Closed typed outputs. Unknown fields
fail closed.

## What remains (do not treat synthetic PASS as release)

- Live company-gateway / small multimodal / frontier sampling on the **same**
  schemas (`docs/LIVE_CONTINUATION.md`). Fake PASS ≠ live eligibility.
- Packaged macOS qualification: one semantic Economy task and one isolated-visual
  High-Assurance task on an assembled build (#435).
- Production adaptive-profile naming alignment (Economy vs Efficient, High
  Assurance vs Frontier) — product/runtime lane, not this crate.
- Operator cockpit copy for why a profile is unavailable (#435 UI criterion).
- Hardware/TCC proof and remaining #274 release matrix.
- Packaged two-surface physical proof for #363.

## Independent review vs issues

- **#435:** evaluation matrix, identical safety across profiles, metrics schema,
  escalation/downgrade/stationarity fixtures implemented. UI + packaged proof
  remain. **Do not close.**
- **#272:** offline synthetic conformance, declared-vs-measured (fake adapters
  are labeled fixture adapters, not measured models), invalid tool formats fail
  closed. Live gateway probes remain. **Do not close.**
- **#274 / #363:** adversarial and lease fixtures exist in this harness; they
  are not hardware release certification.
