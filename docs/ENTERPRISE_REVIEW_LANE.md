# Enterprise gateway review lane

This document describes the candidate-side admission boundary for the full
enterprise review lane. It is a product contract, not a live certification.

## Why this exists

A user may be restricted to a company-approved OpenAI-compatible gateway and a
modest model. GrokPtah should still provide a bounded, multi-hour code review
through decomposition, durable workers, memory, and evidence — without silently
routing a stronger model or publishing changes.

The shared bridge contract is
`crates/codegen/grokptah-agent-bridge/src/enterprise_review.rs`.
`EnterpriseReviewLease` is the only object the future gateway broker should
hand to the review runtime. It contains opaque identifiers and fingerprints,
never a bearer, URL, API key, or provider response. Its gateway attestation is
detached-signed with an operator-selected Ed25519 key; the runtime verifies it
against a separate public trust record before treating the route as admitted.

## Admission contract

The runtime accepts a lease only when all of these are true:

- the lease and gateway attestation use the exact versioned schemas;
- lease and attestation validity windows include the current time;
- route, endpoint fingerprint, model, and modest-tier binding match exactly;
- the route-binding digest matches the canonical lease fields;
- premium fallback is explicitly disabled;
- an external egress-firewall attestation is present;
- the review is read-only, with network, workspace writes, and publication
  explicitly disabled; and
- requests, authoritative tokens, and wall-clock duration fit the bounded
  review policy and the hard campaign maxima.

Any missing, stale, mismatched, or over-broad field rejects the lane before a
provider turn. The returned `EnterpriseReviewEvidence` is secret-free and is
safe to include in a public campaign report.

After admission, `enterprise_review_plan.rs` freezes seven bounded specialist
passes (correctness, security, concurrency, performance, tests, API, and UX).
Each pass has a deterministic objective digest and request/token/time budget.
The same plan can be projected into a secret-free
`EnterpriseReviewWorkPlan`: seven stable idempotency keys and validated
`WorkTemplate` records, intentionally independent so a host scheduler may run
the passes in parallel and safely re-materialize them after a restart. The
projection is side-effect free; only an authorized host broker may issue the
worker credential, bind the workspace, and persist the resulting WorkItems.
The plan retains and validates the secret-free admission evidence, including
the exact route/model/policy binding, so a recovered plan cannot silently
broaden its permissions or fallback policy.
Its nested durable work template is also deny-unknown, so a transport or
broker cannot add an unrecognized policy field that deserialization would
otherwise discard.
The orchestration service now has a host-authorized materialization helper that
uses the plan-bound keys as per-pass idempotency request IDs, so a partial
broker retry replays completed WorkItems instead of duplicating them.
The loopback MCP control plane exposes this as
`ptah_create_enterprise_review`. That tool accepts only the review identity,
secret-free repository/scope fingerprints, the signed lease, and the separate
operator trust record. It requires the ordinary `WorkCreate` authority and
verifies the detached signature before creating any WorkItem; callers cannot
submit a pre-built work plan or self-authorize a route. This is the supported
handoff for a company gateway whose model is weaker than the local orchestrator:
the gateway owns provider execution, while GrokPtah owns decomposition,
durability, retry identity, and read-only policy.
The run accepts only safe location references, deduplicates findings across
passes, and can resume only from a checkpoint bound to the exact plan digest.
The resulting outcome is execution evidence, not a quality claim.

## Current status

The candidate now ships deterministic admission validation and denial tests for
expiry, route/model drift, premium fallback, missing egress attestation,
network/write/publication permission, bound overruns, unknown fields, and
secret-free evidence, plus a host-authorized MCP materialization path that
rejects unsigned or self-authored gateway leases. It also ships deterministic
seven-pass planning and checkpoint/resume tests. This does **not** close the live gate: the
operator-owned broker, approved gateway, gateway-signed deployment attestation,
external egress-firewall attestation, authoritative usage, durable worker
execution, and multi-hour paired quality run remain required before Stage 12
can pass.

The fake benchmark remains a contract test only and must continue to report
`qualityClaimEligible=false`.

## Certification-lab host attachment

The certification lab can now consume the broker handoff without receiving a
bearer or endpoint. Set `GROKPTAH_ENTERPRISE_REVIEW_LEASE` to a disposable,
regular JSON file containing one `EnterpriseReviewLease`, and
`GROKPTAH_ENTERPRISE_REVIEW_TRUST` to a separate regular JSON file containing
the operator-selected `EnterpriseGatewayTrust` public key. The loader rejects
missing, stale, malformed, symlinked, oversized, unsigned, incorrectly signed,
or broadened material before any provider call. The resulting evidence is the
existing secret-free `EnterpriseReviewEvidence` projection.

```sh
export GROKPTAH_ENTERPRISE_REVIEW_LEASE=/run/user/1000/grokptah/review-lease.json
export GROKPTAH_ENTERPRISE_REVIEW_TRUST=/run/user/1000/grokptah/review-trust.json
cargo run --locked --manifest-path evals/certification-lab/Cargo.toml -- \
  review --repository "$PWD" --live --preflight
```

This attachment proves admission only. The runner still returns an
**indeterminate** live report until the operator-owned broker supplies real
provider observations, authoritative usage/quota evidence, restart continuity,
and the paired multi-hour quality result. When a lease is present, the sealed
report binds its route, deployment-policy, credential, and model fingerprints
plus the attestation flags without copying the lease or any secret. Ambient API
keys, token commands, compatible-gateway discovery, and fallback routes remain
refused.
