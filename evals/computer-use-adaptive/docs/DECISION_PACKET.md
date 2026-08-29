# Phase 1 — tree / issue / naming decision packet

## Exact source gate

- Repository: `chriscase/GrokPtah`
- Base: `origin/main`
- SHA: `c6f1cb23e9d6217005599850d9e0d6f7df64d5a1`
- Subject: `Manager v2: autonomous durable coordination (#339)`
- Unmerged adaptive-profile runtime (Efficient / Balanced / Frontier) on
  developer checkouts is **not** the evaluation authority.

## Authoritative issues

| Issue | Role in this lane |
| --- | --- |
| #267 | Epic. Child #435/#272/#274/#363. Not closed. |
| #435 | Adaptive Economy / Balanced / High Assurance + required 12-family matrix. |
| #272 | Provider-neutral conformance evals; declared vs measured capability. |
| #274 | Adversarial fail-closed; prompt injection is observed content. |
| #363 | Same-domain serialization vs isolated concurrent surfaces; leases. |

## Naming decision (explicit, not silent)

Issue #435 names **Economy / Balanced / High Assurance**.

Some unmerged source candidates expose **Efficient / Balanced / Frontier**
(`efficiencyMode` in an unmerged Computer Use schema and cockpit copy).

**Decision recorded by this evaluation lane:**

1. Canonical identifiers in schemas, reports, and tests are `economy`,
   `balanced`, `high_assurance`.
2. Compatibility aliases, ingest only: `efficient` → `economy`,
   `frontier` → `high_assurance`.
3. Aliases are not a fourth profile, not a safety ladder, and not a product
   rename. This lane does not change production identifiers.
4. Economy is an efficiency policy: fewer observation/image/action/model units.
   Safety, grants, leases, takeover, stale revalidation, and sensitive-surface
   denial are identical across profiles.

## Lane allowlist

- `evals/computer-use-adaptive/**`
- `docs/COMPUTER_USE_ADAPTIVE_EVAL.md`

Out of scope: `computer_use/adaptive_profile.rs`, provider-send ledger,
headless/broker adapter, native helper, VM backend, desktop cockpit runtime.
