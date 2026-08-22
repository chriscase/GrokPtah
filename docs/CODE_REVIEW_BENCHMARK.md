# Code-review benchmark (certification-contract slice)

This slice is a **mutation-resistant benchmark contract**, not the production
review runtime and not a live gateway certification. It does **not** ship the
enterprise review lane and does **not** quality-certify GrokPtah.

The intended product outcome, still gated on production prerequisites, is to
prove that a user restricted to a company-approved modest OpenAI-compatible
model can obtain materially better long-running code review from GrokPtah than
from the **same** model used single-pass, without code/secret egress or
workspace/PR mutation. This pull request only builds the fail-closed
benchmark. Fake runs **cannot prove quality**.

## What this slice contains

- Strict deny-unknown campaign, corpus, and fake-provider schemas under
  `evals/code-review-benchmark/`.
- 24 held-out cases across 8 synthetic project families, balanced across
  correctness/bounds, cross-file dataflow, auth/injection/secrets,
  concurrency/restart/idempotency, quota/resource bounds, and
  API/schema/backcompat/docs/ops.
- Fair paired arms that share one opaque deployment/route/credential/model/
  effort/decoding binding, corpus digest, and prompt/response cap:
  - **baseline**: fresh session per case, exactly one model request, no tools
    or memory
  - **GrokPtah**: persistent agent per family evolution, at most six
    requests/case, immutable manifest reads/search and scoped memory only
- A deterministic one-to-one scorer. A true positive requires the hidden
  file, symbol, accepted region, category, and causal atom. Duplicates and
  lures are false positives. The scorer reports ordinary and
  severity-weighted precision/recall/F1, usefulness, finding Brier/ECE, case
  completeness calibration, paired lift with project-cluster bootstrap, and
  cost ratios. Hidden oracles never enter the review-runtime input boundary.
- Workspace immutability: pre/post Merkle roots, Git ref fingerprints, and
  remote publication counts must match. Shell, write, MCP, Computer, and
  publish are denied. Public artifacts are numeric/structural only.
- Fake/live separation: the deterministic fake proves scorer mutations,
  request/cardinality/restart/route-drift/quota/publish-denial/redaction
  behavior and **must** emit `qualityClaimEligible=false`. Only an
  operator-owned approved live compatible gateway with a gateway-signed
  deployment attestation **and** an external egress-firewall attestation
  could prove lift. Those attestations are **not implemented**. Live mode
  is Indeterminate.

Public report schema: `grokptah.code-review-benchmark.v1`.

## Commands

From the repository root. Override paths must be absolute. Fake cannot prove
quality; the CLI repeats that on stderr.

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  review --preflight --repository "$PWD"

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  review --repository "$PWD"

cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  inspect --campaign "$PWD/evals/runs/code-review-benchmark/<campaign-id>"
```

Live mode is explicit and fail-closed. It does not attach to a compatible
gateway, does not consume ambient `XAI_API_KEY` / `XAI_API_BASE` /
`GROKPTAH_TOKEN_COMMAND` overrides, and does not treat Grok Build OIDC as a
substitute for an enterprise-gateway lease:

```sh
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  review --live --repository "$PWD"
```

Inspect verifies seals and digests and exits nonzero for safety failure or
Indeterminate. There is no dashboard in this slice.

Default fake artifacts write under the ignored root
`evals/runs/code-review-benchmark/<campaign-id>/`.

## Bounds and live thresholds (not claimed here)

Per-case: baseline ≤1 request / 8k tokens / 120s; GrokPtah ≤6 / 24k / 10m.
Campaign: ≤400 requests, 1.25M authoritative tokens, 8h, 8 continuations,
128MiB public artifacts. Missing authoritative usage is Indeterminate.

Required **live** thresholds, recorded but not applied as a quality claim on
fake runs: precision ≥.75; weighted recall ≥.75; high/critical recall ≥.85;
usefulness ≥.70; Brier ≤.20; ECE ≤.15; paired weighted-utility lift ≥.15
with project-cluster-bootstrap 95% lower bound >.08; recall lift ≥.15 with
lower bound >.05; wins ≥6/8 families and no family >.10 worse. Cost ratios
long/baseline: tokens ≤6×, requests ≤6×, wall ≤5×. Efficiency superiority
is not claimable without its own confidence interval.

Two randomized live replicates are declared. They are not executed in this
slice.

## Remaining live/production prerequisites

Live enterprise attach is **stopped** in this slice because it requires
service/provider production changes that are out of allowlist:

1. A service-issued disposable enterprise-gateway lease for an operator-owned
   approved live compatible gateway.
2. A gateway-signed deployment attestation proving approved/modest tier,
   backend revision, validity, and no premium fallback.
3. An external egress-firewall attestation allowing only that opaque approved
   endpoint.
4. Production support for a modest OpenAI-compatible enterprise route that
   does not bypass current ambient-route safeguards (API-key, base-URL, and
   token-command overrides remain refused).
5. Metadata-only provider observation and authoritative usage on that route,
   without workspace/PR mutation or code/secret egress.

Until those exist, live review mode must remain Indeterminate and no one may
claim the enterprise review lane is shipped or quality-certified.
